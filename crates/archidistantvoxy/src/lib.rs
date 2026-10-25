//! `ArchiDistantVoxy`: native Pumpkin plugin streaming server-generated LODs to
//! Distant Horizons clients (protocol 16, DH 3.3.x).
//!
//! SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::cast_possible_truncation)]
// The #[plugin_method] macro (pumpkin-api-macros) wraps plugin methods in
// GLOBAL_RUNTIME.block_on(async move { ... }) without awaiting the inner
// future, tripping clippy::async_yields_async in the generated code.
#![allow(clippy::async_yields_async)]

use std::collections::HashMap;
use std::sync::Arc;

use pumpkin::entity::player::Player;
use pumpkin::plugin::api::context::Context;
use pumpkin::plugin::api::events::player::player_custom_payload::PlayerCustomPayloadEvent;
use pumpkin::plugin::api::events::EventPriority;
use pumpkin::plugin::{BoxFuture, EventHandler};
use pumpkin_api_macros::{plugin_impl, plugin_method};

use lod_core::bytes::DecodeError;
use lod_core::cache::{BlobStore, LodCache, MemoryStore, SectionKey};
use lod_core::sqlite::SqliteStore;
use lod_core::message::{self, ClientMessage, Section};
use lod_core::throttle::{BandwidthLimiter, ThrottlePolicy};
pub mod commands;
pub mod generator;

use lod_core::transfer::RequestQueue;
use lod_core::{CHANNEL, DH_VERSION, PROTOCOL};

impl ArchiDistantVoxyPlugin {
    /// Plugin load hook; collected by `#[plugin_impl]` into the generated
    /// `Plugin` trait implementation. Must be expanded before the struct.
    #[allow(clippy::unnecessary_wraps, clippy::needless_pass_by_value)]
    #[plugin_method]
    fn on_load(&self, context: Arc<Context>) -> Result<(), String> {
        context.register_event(Arc::clone(&self.handler), EventPriority::Normal, false);
        commands::register(&self.handler.state, &context);
        let db_path = context.get_data_folder().join("lod-cache.db");
        match SqliteStore::open(&db_path) {
            Ok(store) => {
                self.handler.state.set_persistent(store);
                tracing::info!(
                    target: "archidistantvoxy",
                    "persistent LOD cache at {}",
                    db_path.display()
                );
            }
            Err(e) => {
                tracing::warn!(
                    target: "archidistantvoxy",
                    "cannot open persistent LOD cache at {}: {e}; staying in-memory only",
                    db_path.display()
                );
            }
        }
        tracing::info!(
            target: "archidistantvoxy",
            "loaded: DH {DH_VERSION} (protocol {PROTOCOL}) support on channel '{CHANNEL}'"
        );
        Ok(())
    }
}

/// The plugin entrypoint.
///
/// The `#[plugin_impl]` macro generates the `Plugin` trait implementation from
/// the `#[plugin_method]` functions defined in the impl block above.
#[plugin_impl]
pub struct ArchiDistantVoxyPlugin {
    /// Registered event handler for DH custom payloads.
    handler: Arc<DhPayloadHandler>,
}

impl ArchiDistantVoxyPlugin {
    /// Creates the plugin.
    #[must_use]
    pub fn new() -> Self {
        Self {
            handler: Arc::new(DhPayloadHandler {
                state: Arc::new(SharedState::new()),
            }),
        }
    }
}

impl Default for ArchiDistantVoxyPlugin {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-player DH session: bandwidth limiter and in-flight transfer queue.
struct DhSession {
    /// Bandwidth limiter for this player.
    limiter: BandwidthLimiter,
    /// In-flight LOD transfers for this player.
    queue: RequestQueue,
    /// Set by `/lod resend`: ignore client timestamps until the next session.
    resend_all: bool,
}

impl DhSession {
    /// Creates a session with the default throttle policy.
    #[must_use]
    fn new() -> Self {
        Self {
            limiter: BandwidthLimiter::new(ThrottlePolicy::default()),
            queue: RequestQueue::new(),
            resend_all: false,
        }
    }
}

/// A cache miss waiting for off-tick generation.
struct PendingGeneration {
    /// Cache key of the requested section.
    key: SectionKey,
    /// Client tracker id for the originating request.
    tracker: i32,
    /// The requested section coordinates.
    section: Section,
}

/// Shared plugin state handed to the event handler and command executors.
pub(crate) struct SharedState {
    /// Per-player sessions.
    sessions: std::sync::Mutex<HashMap<String, DhSession>>,
    /// LOD section cache over a swappable persistent store.
    cache: std::sync::Mutex<SwappableCache>,
    /// Per-player queue of sections waiting for generation.
    pending: std::sync::Mutex<HashMap<String, Vec<PendingGeneration>>>,
}

/// Cache wrapper whose persistent layer can be replaced at runtime (the
/// SQLite store is opened in `on_load`, after the handler is constructed).
struct SwappableCache {
    /// RAM layer plus the current persistent store.
    inner: LodCache<Box<dyn BlobStore>>,
}

impl SwappableCache {
    fn new() -> Self {
        Self {
            inner: LodCache::new(Box::new(MemoryStore::new()), 1024),
        }
    }

    fn set_persistent(&mut self, store: SqliteStore) {
        self.inner = LodCache::new(Box::new(store), 1024);
    }

    fn get(&mut self, key: &SectionKey) -> Option<Vec<u8>> {
        self.inner.get(key).ok().flatten()
    }
}

impl SharedState {
    #[must_use]
    fn new() -> Self {
        Self {
            sessions: std::sync::Mutex::new(HashMap::new()),
            cache: std::sync::Mutex::new(SwappableCache::new()),
            pending: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Pops the next pending generation for `player`, if any.
    fn pending_generation(&self, player: &str) -> Option<(SectionKey, i32, Section)> {
        let mut pending = self.pending.lock().expect("pending lock poisoned");
        let queue = pending.get_mut(player)?;
        let next = queue.pop()?;
        if queue.is_empty() {
            pending.remove(player);
        }
        Some((next.key, next.tracker, next.section))
    }

    /// Stores a freshly generated LOD section in the cache.
    fn store_generated(&self, key: &SectionKey, lod: Vec<u8>) {
        if let Err(e) = self
            .cache
            .lock()
            .expect("cache lock poisoned")
            .inner
            .put(key, lod)
        {
            tracing::warn!(target: "archidistantvoxy", "cannot store generated LOD: {e}");
        }
    }

    /// Human-readable status for `/lod status` (EF-8).
    fn status_report(&self) -> String {
        let cache = self.cache.lock().expect("cache lock poisoned");
        let stats = cache.inner.stats();
        let persistent = cache.inner.persistent_len().unwrap_or_default();
        let sessions = self.sessions.lock().expect("sessions lock poisoned");
        let pending: usize = self
            .pending
            .lock()
            .expect("pending lock poisoned")
            .values()
            .map(Vec::len)
            .sum();
        format!(
            "ArchiDistantVoxy — {}\n  cache: {persistent} sections on disk, {} in RAM, hit-rate {:.1}% ({}/{} lookups)\n  sessions: {} DH clients\n  queued transfers: {}\n  pending generations: {pending}",
            commands::protocol_line(),
            cache.inner.ram_bytes() / 1024,
            stats.hit_rate() * 100.0,
            stats.hits + stats.ram_hits,
            stats.lookups,
            sessions.len(),
            sessions.values().map(|s| s.queue.len()).sum::<usize>(),
        )
    }

    /// Queues a bounded prefill of the cache around spawn (EF-6). Sections are
    /// generated by the existing off-tick pending pipeline, prioritized before
    /// player requests, and the queue is capped to keep the prefill bounded.
    fn pregen_spawn(&self, radius: i32) -> usize {
        // One LOD section = 4 chunks; a radius in chunks maps to a square of
        // sections around the origin.
        let sections_per_side = (radius * 2 + 3) / 4;
        let mut queued = 0;
        let mut pending = self.pending.lock().expect("pending lock poisoned");
        let queue = pending.entry("__pregen__".to_string()).or_default();
        let start = -(sections_per_side / 2);
        for x in start..start + sections_per_side {
            for z in start..start + sections_per_side {
                let key = SectionKey {
                    dimension: "overworld".to_string(),
                    x,
                    z,
                };
                let cached = self
                    .cache
                    .lock()
                    .expect("cache lock poisoned")
                    .inner
                    .get(&key)
                    .ok()
                    .flatten()
                    .is_some();
                if cached {
                    continue;
                }
                queue.push(PendingGeneration {
                    key,
                    tracker: -1,
                    section: Section::encode(x, z),
                });
                queued += 1;
            }
        }
        queued
    }

    /// Marks that the next requests from `player` should bypass the client's
    /// cached timestamps (used by `/lod resend`).
    fn mark_resend(&self, player: &str) {
        let mut sessions = self.sessions.lock().expect("sessions lock poisoned");
        if let Some(session) = sessions.get_mut(player) {
            session.resend_all = true;
        }
    }

    /// Consumes the resend flag: when set, in-flight transfers were cancelled
    /// and the next request from this player must be answered fully.
    fn take_resend(&self, player: &str) -> bool {
        let mut sessions = self.sessions.lock().expect("sessions lock poisoned");
        sessions
            .get_mut(player)
            .is_some_and(|session| std::mem::take(&mut session.resend_all))
    }

    /// Wipes the persistent cache and the RAM layer.
    fn purge_cache(&self) -> Result<usize, String> {
        let mut cache = self.cache.lock().expect("cache lock poisoned");
        let count = cache.inner.persistent_len()?;
        cache.inner.clear()?;
        Ok(count)
    }

    /// Serves a generated LOD to its requesting player: enqueues the transfer
    /// and pumps as many fragments as their bandwidth allows, returning the
    /// messages to send.
    fn enqueue_and_pump(&self, name: &str, tracker: i32, lod: Vec<u8>) -> Vec<Vec<u8>> {
        let mut sessions = self.sessions.lock().expect("sessions lock poisoned");
        let session = sessions
            .entry(name.to_string())
            .or_insert_with(DhSession::new);
        session
            .queue
            .push(lod_core::transfer::Transfer::new(tracker, tracker, lod));
        let now = std::time::Instant::now();
        session.queue.pump_all(&mut session.limiter, now)
    }

    fn set_persistent(&self, store: SqliteStore) {
        self.cache
            .lock()
            .expect("cache lock poisoned")
            .set_persistent(store);
    }

    /// Processes one inbound payload, returning the messages to send back.
    fn process(&self, player: &str, data: &[u8]) -> Option<Vec<Vec<u8>>> {
        let decoded = match message::decode(data) {
            Ok(m) => m,
            Err(DecodeError::IncompatibleProtocol(p)) => {
                tracing::warn!(
                    target: "archidistantvoxy",
                    "DH client {player} uses unsupported protocol {p} (expected {PROTOCOL})"
                );
                return Some(vec![message::close_session(&format!(
                    "Server supports DH protocol {PROTOCOL} (DH {DH_VERSION})"
                ))]);
            }
            Err(e) => {
                tracing::warn!(target: "archidistantvoxy", "malformed DH payload from {player}: {e}");
                return None;
            }
        };

        match decoded {
            ClientMessage::Init { dimension } => {
                tracing::info!(target: "archidistantvoxy", "DH session init for '{dimension}' from {player}");
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or_default();
                Some(vec![message::level_init(
                    &dimension,
                    "archidistantvoxy",
                    &dimension,
                    now,
                )])
            }
            ClientMessage::Config(config) => {
                tracing::info!(target: "archidistantvoxy", "DH session config from {player}: sync distance {} chunks", config.sync_distance);
                Some(vec![message::session_config(config)])
            }
            ClientMessage::Request {
                tracker,
                level,
                section,
                ..
            } => {
                tracing::info!(target: "archidistantvoxy", "DH LOD request from {player} for {level} section ({}, {}) tracker {tracker}", section.x, section.z);
                let key = SectionKey {
                    dimension: level,
                    x: section.x,
                    z: section.z,
                };
                let cached = self
                    .cache
                    .lock()
                    .ok()?
                    .get(&key);
                if let Some(lod) = cached {
                        let mut sessions = self.sessions.lock().ok()?;
                        let session = sessions
                            .entry(player.to_string())
                            .or_insert_with(DhSession::new);
                        session
                            .queue
                            .push(lod_core::transfer::Transfer::new(tracker, tracker, lod));
                        let now = std::time::Instant::now();
                    Some(session.queue.pump_all(&mut session.limiter, now))
                } else {
                    // M1: no generator yet; M2 wires pumpkin-world here.
                    tracing::debug!(target: "archidistantvoxy", "LOD miss at {}/{}/{}", key.dimension, key.x, key.z);
                    Some(vec![message::reject(tracker, "LOD not generated yet", 2)])
                }
            }
            ClientMessage::Cancel { tracker } => {
                if let Ok(mut sessions) = self.sessions.lock() {
                    if let Some(session) = sessions.get_mut(player) {
                        session.queue.cancel(tracker);
                    }
                }
                None
            }
            ClientMessage::Close => {
                tracing::info!(target: "archidistantvoxy", "DH session closed by {player}");
                if let Ok(mut sessions) = self.sessions.lock() {
                    sessions.remove(player);
                }
                None
            }
        }
    }
}

/// Handles inbound DH plugin-channel payloads from players.
struct DhPayloadHandler {
    /// Shared plugin state.
    state: Arc<SharedState>,
}

impl EventHandler<PlayerCustomPayloadEvent> for DhPayloadHandler {
    fn handle<'a>(
        &'a self,
        server: &'a Arc<pumpkin::server::Server>,
        event: &'a PlayerCustomPayloadEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if event.channel != CHANNEL {
                return;
            }
            let player = Arc::clone(&event.player);
            let name = player.gameprofile.name.clone();
            if let Some(messages) = self.state.process(&name, &event.data) {
                for message in messages {
                    send(&player, &message).await;
                }
            }
            if let Some((key, tracker, section)) = self.state.pending_generation(&name) {
                let world = player.world();
                let server = Arc::clone(server);
                // Off-tick generation (ENF-2): the work runs on the blocking
                // pool; this handler only awaits and never blocks the tick.
                let state = Arc::clone(&self.state);
                let generated = tokio::task::spawn_blocking(move || {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or_default();
                    generator::generate_section(&server, &world, &section, now)
                })
                .await
                .unwrap_or_else(|e| {
                    Err(generator::GenerationError::Unavailable(format!(
                        "generation task failed: {e}"
                    )))
                });
                match generated {
                    Ok(lod) => {
                        state.store_generated(&key, lod.clone());
                        let messages = state.enqueue_and_pump(&name, tracker, lod);
                        for message in messages {
                            send(&player, &message).await;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(target: "archidistantvoxy", "generation failed: {e}");
                        send(
                            &player,
                            &message::reject(tracker, "generation failed", 2),
                        )
                        .await;
                    }
                }
            }
        })
    }
}

/// Sends a DH server message to a player's Java client.
async fn send(player: &Arc<Player>, payload: &[u8]) {
    if let Some(java) = player.as_java() {
        java.send_custom_payload(CHANNEL, payload).await;
    }
}

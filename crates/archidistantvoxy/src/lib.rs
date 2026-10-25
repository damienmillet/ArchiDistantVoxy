//! `ArchiDistantVoxy`: native Pumpkin plugin streaming server-generated LODs to
//! Distant Horizons clients (protocol 16, DH 3.3.x).
//!
//! SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::cast_possible_truncation)]

use std::collections::HashMap;
use std::sync::Arc;

use pumpkin::entity::player::Player;
use pumpkin::plugin::api::context::Context;
use pumpkin::plugin::api::events::player::player_custom_payload::PlayerCustomPayloadEvent;
use pumpkin::plugin::api::events::EventPriority;
use pumpkin::plugin::{BoxFuture, EventHandler};
use pumpkin_api_macros::{plugin_impl, plugin_method};

use lod_core::bytes::DecodeError;
use lod_core::cache::{LodCache, MemoryStore, SectionKey};
use lod_core::message::{self, ClientMessage};
use lod_core::throttle::{BandwidthLimiter, ThrottlePolicy};
use lod_core::transfer::RequestQueue;
use lod_core::{CHANNEL, DH_VERSION, PROTOCOL};

impl ArchiDistantVoxyPlugin {
    /// Plugin load hook; collected by `#[plugin_impl]` into the generated
    /// `Plugin` trait implementation. Must be expanded before the struct.
    #[allow(clippy::unnecessary_wraps)]
    #[plugin_method]
    fn on_load(&self, context: Arc<Context>) -> Result<(), String> {
        context.register_event(Arc::clone(&self.handler), EventPriority::Normal, false);
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
}

impl DhSession {
    /// Creates a session with the default throttle policy.
    #[must_use]
    fn new() -> Self {
        Self {
            limiter: BandwidthLimiter::new(ThrottlePolicy::default()),
            queue: RequestQueue::new(),
        }
    }
}

/// Shared plugin state handed to the event handler.
struct SharedState {
    /// Per-player sessions.
    sessions: std::sync::Mutex<HashMap<String, DhSession>>,
    /// LOD section cache; the SQLite-backed store lands with M2.
    cache: std::sync::Mutex<LodCache<MemoryStore>>,
}

impl SharedState {
    #[must_use]
    fn new() -> Self {
        Self {
            sessions: std::sync::Mutex::new(HashMap::new()),
            cache: std::sync::Mutex::new(LodCache::new(MemoryStore::new(), 1024)),
        }
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
                let cached = self.cache.lock().ok()?.get(&key).ok().flatten();
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
        _server: &'a Arc<pumpkin::server::Server>,
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
        })
    }
}

/// Sends a DH server message to a player's Java client.
async fn send(player: &Arc<Player>, payload: &[u8]) {
    if let Some(java) = player.as_java() {
        java.send_custom_payload(CHANNEL, payload).await;
    }
}

//! `ArchiDistantVoxy`: native Pumpkin plugin streaming server-generated LODs to
//! Distant Horizons clients (protocol 16, DH 3.3.x).
//!
//! SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::cast_possible_truncation)]

use std::sync::Arc;

use pumpkin::entity::player::Player;
use pumpkin::plugin::api::context::Context;
use pumpkin::plugin::api::events::player::player_custom_payload::PlayerCustomPayloadEvent;
use pumpkin::plugin::api::events::EventPriority;
use pumpkin::plugin::api::Plugin;
use pumpkin::plugin::api::PluginFuture;
use pumpkin::plugin::EventHandler;
use pumpkin_api_macros::plugin_impl;

use lod_core::bytes::DecodeError;
use lod_core::message::{self, ClientMessage};
use lod_core::{CHANNEL, DH_VERSION, PROTOCOL};

/// The plugin entrypoint.
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
            handler: Arc::new(DhPayloadHandler),
        }
    }
}

impl Default for ArchiDistantVoxyPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl Plugin for ArchiDistantVoxyPlugin {
    fn on_load(&self, context: Arc<Context>) -> PluginFuture<'_, Result<(), String>> {
        let handler = Arc::clone(&self.handler);
        Box::pin(async move {
            context.register_event(handler, EventPriority::Normal, false);
            tracing::info!(
                target: "archidistantvoxy",
                "loaded: DH {DH_VERSION} (protocol {PROTOCOL}) support on channel '{CHANNEL}'"
            );
            Ok(())
        })
    }
}

/// Handles inbound DH plugin-channel payloads from players.
struct DhPayloadHandler;

impl EventHandler<PlayerCustomPayloadEvent> for DhPayloadHandler {
    fn handle<'a>(
        &'a self,
        _server: &'a Arc<pumpkin::server::Server>,
        event: &'a PlayerCustomPayloadEvent,
    ) -> pumpkin::plugin::BoxFuture<'a, ()> {
        Box::pin(async move {
            if event.channel != CHANNEL {
                return;
            }
            handle_payload(Arc::clone(&event.player), &event.data).await;
        })
    }
}

/// Handles one decoded DH message from a player.
async fn handle_payload(player: Arc<Player>, data: &[u8]) {
    let decoded = match message::decode(data) {
        Ok(m) => m,
        Err(DecodeError::IncompatibleProtocol(p)) => {
            tracing::warn!(
                target: "archidistantvoxy",
                "DH client {} uses unsupported protocol {p} (expected {PROTOCOL}); closing session",
                player.gameprofile.name
            );
            player
                .send_custom_payload(
                    CHANNEL,
                    &message::close_session(&format!(
                        "Server supports DH protocol {PROTOCOL} (DH {DH_VERSION})"
                    )),
                )
                .await;
            return;
        }
        Err(e) => {
            tracing::warn!(target: "archidistantvoxy", "malformed DH payload: {e}");
            return;
        }
    };

    match decoded {
        ClientMessage::Init { dimension } => {
            tracing::info!(
                target: "archidistantvoxy",
                "DH session init for dimension '{dimension}' from {}",
                player.gameprofile.name
            );
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or_default();
            player
                .send_custom_payload(CHANNEL, &message::level_init(&dimension, "archidistantvoxy", &dimension, now))
                .await;
        }
        ClientMessage::Config(config) => {
            tracing::info!(
                target: "archidistantvoxy",
                "DH session config from {}: sync distance {} chunks, bandwidth {} KB/s",
                player.gameprofile.name, config.sync_distance, config.bandwidth_kbps
            );
            player
                .send_custom_payload(CHANNEL, &message::session_config(config))
                .await;
        }
        ClientMessage::Request {
            tracker,
            level,
            section,
            ..
        } => {
            tracing::info!(
                target: "archidistantvoxy",
                "DH LOD request from {} for {level} section ({}, {}) tracker {tracker}",
                player.gameprofile.name, section.x, section.z
            );
            // M1: reject until the LodBuilder pipeline is wired to pumpkin-world.
            player
                .send_custom_payload(
                    CHANNEL,
                    &message::reject(tracker, "LOD generation not yet available", 2),
                )
                .await;
        }
        ClientMessage::Cancel { tracker } => {
            tracing::debug!(
                target: "archidistantvoxy",
                "DH request {tracker} cancelled by {}",
                player.gameprofile.name
            );
        }
        ClientMessage::Close => {
            tracing::info!(
                target: "archidistantvoxy",
                "DH session closed by {}",
                player.gameprofile.name
            );
        }
    }
}

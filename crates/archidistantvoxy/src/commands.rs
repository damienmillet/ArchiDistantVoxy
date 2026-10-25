//! `/lod` admin commands (EF-6, EF-8, EF-10).
//!
//! - `/lod status`: cache stats, generation queues, bandwidth, protocol target
//! - `/lod pregen <radius>`: bounded, interruptible cache prefill around spawn
//! - `/lod resend <player>`: re-push cached sections to a connected DH client
//! - `/lod purge-cache`: wipe the persistent LOD cache
//!
//! SPDX-License-Identifier: GPL-3.0-or-later.

use std::sync::Arc;

use pumpkin::command::argument_builder::{argument, literal, ArgumentBuilder};
use pumpkin::command::argument_types::core::integer::IntegerArgumentType;
use pumpkin::command::argument_types::core::string::StringArgumentType;
use pumpkin::command::context::command_context::CommandContext;
use pumpkin::command::CommandSource;
use pumpkin::command::node::CommandExecutor;
use pumpkin::command::node::CommandExecutorResult;
use pumpkin::plugin::api::context::Context;
use pumpkin_util::text::TextComponent;

use crate::{SharedState, DH_VERSION, PROTOCOL};

/// Maximum `/lod pregen` radius in chunks, to keep the prefill bounded (EF-6).
pub const MAX_PREGEN_RADIUS: i32 = 1024;

/// Registers all `/lod` subcommands with their permissions (EF-10).
pub(crate) fn register(state: &Arc<SharedState>, context: &Arc<Context>) {
    context.register_command(
        literal("lod").then(literal("status").executes(StatusExecutor {
            state: Arc::clone(state),
        })),
        "lod.admin",
    );

    context.register_command(
        literal("lod").then(literal("pregen").then(
            argument("radius", IntegerArgumentType::new(1, MAX_PREGEN_RADIUS)).executes(
                PregenExecutor {
                    state: Arc::clone(state),
                },
            ),
        )),
        "lod.pregen",
    );

    context.register_command(
        literal("lod").then(literal("resend").then(
            argument("player", StringArgumentType::SingleWord).executes(ResendExecutor {
                state: Arc::clone(state),
            }),
        )),
        "lod.admin",
    );

    context.register_command(
        literal("lod").then(
            literal("purge-cache").executes(PurgeCacheExecutor {
                state: Arc::clone(state),
            }),
        ),
        "lod.admin",
    );
}

/// `/lod status`: reports cache, queues and bandwidth (EF-8).
struct StatusExecutor {
    state: Arc<SharedState>,
}

impl CommandExecutor for StatusExecutor {
    fn execute(&self, context: &CommandContext) -> CommandExecutorResult {
        let report = self.state.status_report();
        context.source.send_message(TextComponent::text(report));
        Ok(())
    }
}

/// `/lod pregen <radius>`: pre-fills the cache around spawn (EF-6).
struct PregenExecutor {
    state: Arc<SharedState>,
}

impl CommandExecutor for PregenExecutor {
    fn execute(&self, context: &CommandContext) -> CommandExecutorResult {
        let radius = IntegerArgumentType::get(context, "radius")?;
        if radius <= 0 || radius > MAX_PREGEN_RADIUS {
            context.source.send_message(TextComponent::text(format!(
                "radius must be between 1 and {MAX_PREGEN_RADIUS} chunks"
            )));
            return Ok(());
        }
        let sections = self.state.pregen_spawn(radius);
        context.source.send_message(TextComponent::text(format!(
            "pregen queued {sections} LOD sections around spawn (radius {radius} chunks)"
        )));
        Ok(())
    }
}

/// `/lod resend <player>`: re-pushes cached sections to a DH client.
struct ResendExecutor {
    state: Arc<SharedState>,
}

impl CommandExecutor for ResendExecutor {
    fn execute(&self, context: &CommandContext) -> CommandExecutorResult {
        let player_name =
            StringArgumentType::get::<CommandSource>(context, "player")?.to_string();
        if context
            .source
            .server()
            .get_player_by_name(&player_name)
            .is_none()
        {
            context
                .source
                .send_message(TextComponent::text(format!("{player_name} is not online")));
            return Ok(());
        }
        context.source.send_message(TextComponent::text(format!(
            "resend requested for {player_name}: cached sections will be re-pushed on their next requests"
        )));
        self.state.mark_resend(&player_name);
        Ok(())
    }
}

/// `/lod purge-cache`: wipes the persistent LOD cache.
struct PurgeCacheExecutor {
    state: Arc<SharedState>,
}

impl CommandExecutor for PurgeCacheExecutor {
    fn execute(&self, context: &CommandContext) -> CommandExecutorResult {
        match self.state.purge_cache() {
            Ok(count) => context
                .source
                .send_message(TextComponent::text(format!("purged {count} LOD sections"))),
            Err(e) => context
                .source
                .send_message(TextComponent::text(format!("purge failed: {e}"))),
        }
        Ok(())
    }
}

/// Protocol target line used in status messages.
#[must_use]
pub fn protocol_line() -> String {
    format!("DH {DH_VERSION} (protocol {PROTOCOL})")
}

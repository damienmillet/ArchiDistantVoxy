//! LOD invalidation on chunk modifications (EF-7).
//!
//! Loaded-chunk modifications (block place/break, explosions) invalidate the
//! LOD section covering the modified position, per a configurable policy:
//! immediate, periodic (batched through a dirty set) or disabled.
//!
//! SPDX-License-Identifier: GPL-3.0-or-later.

use std::collections::HashSet;
use std::sync::Arc;

use pumpkin::plugin::api::events::block::{BlockBreakEvent, BlockPlaceEvent};
use pumpkin::plugin::api::events::EventPriority;
use pumpkin::plugin::{BoxFuture, EventHandler};

use lod_core::cache::SectionKey;

use crate::SharedState;

/// Invalidation policy (EF-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationPolicy {
    /// Invalidate the covering section as soon as a block changes.
    Immediate,
    /// Record dirty sections; invalidate them when the batch is flushed.
    Periodic,
    /// Never invalidate; sections stay as generated until purged.
    Disabled,
}

/// Maps a block position to the LOD section covering it (detail 6 = 64 blocks).
#[must_use]
pub fn section_of(x: i32, z: i32) -> (i32, i32) {
    (x >> 6, z >> 6)
}

/// Shared dirty-section tracking for the periodic policy.
pub(crate) struct InvalidationWorker {
    state: Arc<SharedState>,
    policy: InvalidationPolicy,
    dirty: std::sync::Mutex<HashSet<SectionKey>>,
}

impl InvalidationWorker {
    /// Creates the worker with the configured policy.
    #[must_use]
    pub fn new(state: Arc<SharedState>, policy: InvalidationPolicy) -> Self {
        Self {
            state,
            policy,
            dirty: std::sync::Mutex::new(HashSet::new()),
        }
    }

    fn record(&self, position: (i32, i32)) {
        if self.policy == InvalidationPolicy::Disabled {
            return;
        }
        let (x, z) = position;
        let key = SectionKey {
            dimension: "overworld".to_string(),
            x,
            z,
        };
        match self.policy {
            InvalidationPolicy::Immediate => {
                if let Err(e) = self.state.invalidate(&key) {
                    tracing::warn!(target: "archidistantvoxy", "invalidation failed: {e}");
                }
            }
            InvalidationPolicy::Periodic => {
                self.dirty
                    .lock()
                    .expect("dirty lock poisoned")
                    .insert(key);
            }
            InvalidationPolicy::Disabled => {}
        }
    }

    /// Flushes the dirty set; called by the periodic flush task.
    ///
    /// # Panics
    /// Panics only if the dirty lock is poisoned.
    pub fn flush(&self) {
        let dirty: Vec<SectionKey> = self
            .dirty
            .lock()
            .expect("dirty lock poisoned")
            .drain()
            .collect();
        for key in dirty {
            if let Err(e) = self.state.invalidate(&key) {
                tracing::warn!(target: "archidistantvoxy", "invalidation failed: {e}");
            }
        }
    }
}

/// Handles block placements.
pub(crate) struct BlockPlaceHandler {
    worker: Arc<InvalidationWorker>,
}

impl EventHandler<BlockPlaceEvent> for BlockPlaceHandler {
    fn handle<'a>(
        &'a self,
        _server: &'a Arc<pumpkin::server::Server>,
        event: &'a BlockPlaceEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.worker
                .record(section_of(event.block_position.0.x, event.block_position.0.z));
        })
    }
}

/// Handles block breaks.
pub(crate) struct BlockBreakHandler {
    worker: Arc<InvalidationWorker>,
}

impl EventHandler<BlockBreakEvent> for BlockBreakHandler {
    fn handle<'a>(
        &'a self,
        _server: &'a Arc<pumpkin::server::Server>,
        event: &'a BlockBreakEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.worker
                .record(section_of(event.block_position.0.x, event.block_position.0.z));
        })
    }
}

/// Registers the invalidation handlers for the configured policy, spawning the
/// periodic flush task when the policy is `Periodic`.
///
/// # Panics
/// Panics only when a lock is poisoned; never in normal operation.
pub(crate) fn register(
    state: &Arc<SharedState>,
    context: &Arc<pumpkin::plugin::api::context::Context>,
    policy: InvalidationPolicy,
) {
    if policy == InvalidationPolicy::Disabled {
        return;
    }
    let worker = Arc::new(InvalidationWorker::new(Arc::clone(state), policy));
    context.register_event(
        Arc::new(BlockPlaceHandler {
            worker: Arc::clone(&worker),
        }),
        EventPriority::Low,
        false,
    );
    context.register_event(
        Arc::new(BlockBreakHandler {
            worker: Arc::clone(&worker),
        }),
        EventPriority::Low,
        false,
    );
    if policy == InvalidationPolicy::Periodic {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                worker.flush();
            }
        });
    }
}

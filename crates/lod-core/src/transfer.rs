//! Per-player LOD section transfer state machine (EF-2/EF-5).
//!
//! Splits an encoded LOD section into bounded fragments, releases them under
//! the per-player bandwidth limiter, and emits the final `transfer_response`.
//! SPDX-License-Identifier: GPL-3.0-or-later.

use crate::message::{transfer_fragment, transfer_response, unchanged};
use crate::throttle::BandwidthLimiter;
use crate::TRANSFER_PACKET_BYTES;

/// Outcome of one pump of a pending transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PumpOutcome {
    /// Messages to send now (possibly empty when throttled).
    Messages(Vec<Vec<u8>>),
    /// The transfer completed with the final response.
    Completed(Vec<Vec<u8>>),
}

/// One in-flight LOD section transfer to a player.
#[derive(Debug)]
pub struct Transfer {
    tracker: i32,
    buffer: i32,
    lod: Vec<u8>,
    offset: usize,
    sent_any: bool,
}

impl Transfer {
    /// Starts a transfer for `tracker` with the encoded `lod` section.
    #[must_use]
    pub fn new(tracker: i32, buffer: i32, lod: Vec<u8>) -> Self {
        Self {
            tracker,
            buffer,
            lod,
            offset: 0,
            sent_any: false,
        }
    }

    /// Client tracker id.
    #[must_use]
    pub fn tracker(&self) -> i32 {
        self.tracker
    }

    /// Remaining bytes to send.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.lod.len() - self.offset
    }

    /// Whether the transfer is complete.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.offset >= self.lod.len()
    }

    /// Pumps as many fragments as the limiter allows right now.
    ///
    /// # Panics
    /// Never panics: offsets stay within the LOD slice bounds.
    #[must_use]
    pub fn pump(&mut self, limiter: &mut BandwidthLimiter, now: std::time::Instant) -> PumpOutcome {
        let mut messages = Vec::new();
        while self.offset < self.lod.len() {
            let fragment = transfer_fragment(self.buffer, &self.lod, self.offset);
            let msg_len = fragment.len();
            if !limiter.try_acquire(msg_len, now) {
                break;
            }
            messages.push(fragment);
            self.sent_any = true;
            self.offset += TRANSFER_PACKET_BYTES.min(self.lod.len() - self.offset);
        }
        if self.offset >= self.lod.len() {
            messages.push(transfer_response(self.tracker, self.buffer));
            PumpOutcome::Completed(messages)
        } else {
            PumpOutcome::Messages(messages)
        }
    }
}

/// Tracks the in-flight transfer and handles an update-only request.
#[derive(Debug)]
pub struct RequestQueue {
    pending: Vec<Transfer>,
}

impl RequestQueue {
    /// Creates an empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    /// Enqueues a new transfer.
    pub fn push(&mut self, transfer: Transfer) {
        self.pending.push(transfer);
    }

    /// Removes and abandons the transfer for `tracker` (client cancel).
    pub fn cancel(&mut self, tracker: i32) {
        self.pending.retain(|t| t.tracker() != tracker);
    }

    /// Number of in-flight transfers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether no transfer is in flight.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Pumps all in-flight transfers under `limiter`, returning the messages
    /// to send and removing completed ones.
    #[must_use]
    pub fn pump_all(
        &mut self,
        limiter: &mut BandwidthLimiter,
        now: std::time::Instant,
    ) -> Vec<Vec<u8>> {
        let mut messages = Vec::new();
        self.pending.retain_mut(|transfer| {
            match transfer.pump(limiter, now) {
                PumpOutcome::Messages(msgs) => {
                    messages.extend(msgs);
                    true
                }
                PumpOutcome::Completed(msgs) => {
                    messages.extend(msgs);
                    false
                }
            }
        });
        messages
    }
}

impl Default for RequestQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Answers an update-only request: `unchanged` when the client's cached
/// timestamp is at least as recent as ours, a fresh transfer otherwise.
#[must_use]
pub fn answer_update_request(
    tracker: i32,
    client_timestamp: Option<u64>,
    ours_timestamp: u64,
    lod: Option<Vec<u8>>,
    buffer: i32,
) -> TransferOrUnchanged {
    match lod {
        Some(lod) => {
            if client_timestamp.is_some_and(|ts| ts >= ours_timestamp) {
                TransferOrUnchanged::Unchanged(unchanged(tracker))
            } else {
                TransferOrUnchanged::Transfer(Transfer::new(tracker, buffer, lod))
            }
        }
        None => TransferOrUnchanged::Unchanged(unchanged(tracker)),
    }
}

/// Result of answering an update request.
#[derive(Debug)]
pub enum TransferOrUnchanged {
    /// A new transfer should be enqueued.
    Transfer(Transfer),
    /// The client copy is current; send this message.
    Unchanged(Vec<u8>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::throttle::{BandwidthLimiter, ThrottlePolicy};
    use std::time::{Duration, Instant};

    fn unlimited() -> BandwidthLimiter {
        BandwidthLimiter::new(ThrottlePolicy {
            login_sync_bytes_per_sec: 0,
            regular_bytes_per_sec: 0,
            login_sync_duration: Duration::from_secs(0),
        })
    }

    fn limited(per_sec: usize) -> BandwidthLimiter {
        BandwidthLimiter::new(ThrottlePolicy {
            login_sync_bytes_per_sec: per_sec,
            regular_bytes_per_sec: per_sec,
            login_sync_duration: Duration::from_secs(0),
        })
    }

    #[test]
    fn transfer_completes_in_one_pump_when_unlimited() {
        let lod = vec![0x55; 70_000];
        let mut transfer = Transfer::new(7, 3, lod);
        let mut limiter = unlimited();
        let now = Instant::now();
        let PumpOutcome::Completed(messages) = transfer.pump(&mut limiter, now) else {
            panic!("expected completion");
        };
        // 70_000 bytes = 3 fragments (30k, 30k, 10k) + final response.
        assert_eq!(messages.len(), 4);
        assert!(transfer.is_complete());
    }

    #[test]
    fn transfer_throttles_and_resumes() {
        let lod = vec![0x55; 70_000];
        let mut transfer = Transfer::new(7, 3, lod);
        // Budget only covers the first fragment (~30 KB + overhead).
        let mut limiter = limited(30_100);
        let now = Instant::now();
        let PumpOutcome::Messages(first) = transfer.pump(&mut limiter, now) else {
            panic!("expected throttled partial pump");
        };
        assert_eq!(first.len(), 1);
        assert!(!transfer.is_complete());
        assert_eq!(transfer.remaining(), 40_000);

        let later = now + Duration::from_secs(2);
        let PumpOutcome::Completed(rest) = transfer.pump(&mut limiter, later) else {
            panic!("expected completion after refill");
        };
        assert_eq!(rest.len(), 3); // 2 fragments + response
    }

    #[test]
    fn queue_pumps_and_drops_completed() {
        let mut queue = RequestQueue::new();
        assert!(queue.is_empty());
        queue.push(Transfer::new(1, 0, vec![1; 40_000]));
        queue.push(Transfer::new(2, 1, vec![2; 40_000]));
        assert_eq!(queue.len(), 2);

        let mut limiter = unlimited();
        let messages = queue.pump_all(&mut limiter, Instant::now());
        // 2 transfers x (2 fragments + response).
        assert_eq!(messages.len(), 6);
        assert!(queue.is_empty());
    }

    #[test]
    fn cancel_removes_pending_transfer() {
        let mut queue = RequestQueue::new();
        queue.push(Transfer::new(1, 0, vec![1; 40_000]));
        queue.push(Transfer::new(2, 1, vec![2; 40_000]));
        queue.cancel(1);
        assert_eq!(queue.len(), 1);
        let mut limiter = unlimited();
        let messages = queue.pump_all(&mut limiter, Instant::now());
        assert_eq!(messages.len(), 3);
    }

    #[test]
    fn update_request_compares_timestamps() {
        let lod = vec![3; 100];
        let TransferOrUnchanged::Transfer(t) =
            answer_update_request(5, Some(100), 200, Some(lod), 9)
        else {
            panic!("expected a fresh transfer");
        };
        assert_eq!(t.tracker(), 5);

        let TransferOrUnchanged::Unchanged(msg) =
            answer_update_request(5, Some(200), 200, Some(vec![3; 100]), 9)
        else {
            panic!("expected unchanged");
        };
        assert_eq!(&msg[..4], &[0, 16, 0, 8]);

        let TransferOrUnchanged::Unchanged(_) =
            answer_update_request(5, None, 200, None, 9)
        else {
            panic!("expected unchanged when no LOD is available");
        };
    }
}

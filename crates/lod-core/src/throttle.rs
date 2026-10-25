//! Per-player bandwidth throttling for LOD transfers (EF-5 / ENF-6).
//!
//! Token-bucket with independent budgets for login-sync and regular requests.
//! Modeled on DH Support throttling. SPDX-License-Identifier: GPL-3.0-or-later.
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::time::{Duration, Instant};

/// Throttle budgets: 0 means unlimited for a field, matching DH Support behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottlePolicy {
    /// Max bytes per second for the login-sync burst phase (0 = unlimited).
    pub login_sync_bytes_per_sec: usize,
    /// Max bytes per second for regular LOD traffic (0 = unlimited).
    pub regular_bytes_per_sec: usize,
    /// Duration of the login-sync phase after a player joins.
    pub login_sync_duration: Duration,
}

impl Default for ThrottlePolicy {
    fn default() -> Self {
        Self {
            login_sync_bytes_per_sec: 500 * 1024,
            regular_bytes_per_sec: 200 * 1024,
            login_sync_duration: Duration::from_secs(120),
        }
    }
}

/// Token-bucket bandwidth limiter for a single player.
#[derive(Debug)]
pub struct BandwidthLimiter {
    policy: ThrottlePolicy,
    login_started: Instant,
    tokens: f64,
    last_refill: Instant,
}

impl BandwidthLimiter {
    /// Creates a limiter for a player who just joined.
    #[must_use]
    pub fn new(policy: ThrottlePolicy) -> Self {
        let now = Instant::now();
        let tokens = if policy.login_sync_bytes_per_sec == 0 {
            f64::INFINITY
        } else {
            policy.login_sync_bytes_per_sec as f64
        };
        Self {
            policy,
            login_started: now,
            tokens,
            last_refill: now,
        }
    }

    /// Whether the player is still in the login-sync phase.
    #[must_use]
    pub fn in_login_sync(&self) -> bool {
        self.login_started.elapsed() < self.policy.login_sync_duration
    }

    fn budget(&self) -> Option<usize> {
        let per_sec = if self.in_login_sync() {
            self.policy.login_sync_bytes_per_sec
        } else {
            self.policy.regular_bytes_per_sec
        };
        (per_sec != 0).then_some(per_sec)
    }

    fn refill(&mut self, now: Instant) {
        if let Some(per_sec) = self.budget() {
            let elapsed = now.saturating_duration_since(self.last_refill).as_secs_f64();
            self.tokens = (self.tokens + elapsed * per_sec as f64)
                .min(per_sec as f64 * 2.0);
            self.last_refill = now;
        }
    }

    /// Reports whether `bytes` may be sent now, without consuming budget.
    #[must_use]
    pub fn try_acquire(&mut self, bytes: usize, now: Instant) -> bool {
        self.refill(now);
        match self.budget() {
            None => true,
            Some(per_sec) => {
                if self.tokens >= bytes as f64 {
                    self.tokens -= bytes as f64;
                    true
                } else {
                    let _ = per_sec;
                    false
                }
            }
        }
    }

    /// Time to wait before `bytes` can be sent, or `None` when sendable now.
    #[must_use]
    pub fn wait_for(&mut self, bytes: usize, now: Instant) -> Option<Duration> {
        self.refill(now);
        let per_sec = self.budget()?;
        let missing = bytes as f64 - self.tokens;
        if missing <= 0.0 {
            return None;
        }
        Some(Duration::from_secs_f64(missing / per_sec as f64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_policy_never_blocks() {
        let policy = ThrottlePolicy {
            login_sync_bytes_per_sec: 0,
            regular_bytes_per_sec: 0,
            login_sync_duration: Duration::from_secs(0),
        };
        let mut limiter = BandwidthLimiter::new(policy);
        let now = Instant::now();
        assert!(limiter.try_acquire(10_000_000, now));
        assert!(limiter.try_acquire(10_000_000, now));
    }

    #[test]
    fn limited_budget_depletes_and_refills() {
        let policy = ThrottlePolicy {
            login_sync_bytes_per_sec: 1000,
            regular_bytes_per_sec: 1000,
            login_sync_duration: Duration::from_secs(0),
        };
        let mut limiter = BandwidthLimiter::new(policy);
        let t0 = Instant::now();
        assert!(limiter.try_acquire(600, t0));
        assert!(!limiter.try_acquire(600, t0), "bucket should be empty");

        let wait = limiter.wait_for(600, t0).unwrap();
        assert!(wait > Duration::from_secs(0));

        let t1 = t0 + Duration::from_millis(2000);
        assert!(limiter.try_acquire(600, t1), "refilled after wait");
    }

    #[test]
    fn login_sync_uses_distinct_budget() {
        let policy = ThrottlePolicy {
            login_sync_bytes_per_sec: 10_000,
            regular_bytes_per_sec: 100,
            login_sync_duration: Duration::from_secs(60),
        };
        let mut limiter = BandwidthLimiter::new(policy);
        let t0 = Instant::now();
        assert!(limiter.in_login_sync());
        assert!(limiter.try_acquire(5_000, t0));

        let mut after = BandwidthLimiter::new(policy);
        after.login_started = t0.checked_sub(Duration::from_secs(120)).unwrap_or(t0);
        let t2 = t0 + Duration::from_millis(0);
        assert!(!after.in_login_sync());
        assert!(!after.try_acquire(5_000, t2), "regular budget is lower");
    }
}

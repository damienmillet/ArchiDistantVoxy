//! Plugin configuration (`config.toml` in the plugin data folder).
//!
//! Mirrors the spec's `plugins/lod/config.toml` intent; since native plugins
//! get `plugins/data/archidistantvoxy/` as their data folder, the file lives
//! there. SPDX-License-Identifier: GPL-3.0-or-later.

use std::path::Path;

use serde::Deserialize;

use crate::invalidation::InvalidationPolicy;

/// Root configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Bandwidth and throttling settings.
    #[serde(default)]
    pub bandwidth: BandwidthConfig,
    /// Generation settings.
    #[serde(default)]
    pub generation: GenerationConfig,
    /// Invalidation policy (EF-7).
    #[serde(default)]
    pub invalidation: InvalidationConfig,
}

/// Bandwidth settings (EF-5, ENF-6).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BandwidthConfig {
    /// Max bytes per second per player during login-sync (0 = unlimited).
    #[serde(default = "default_login_sync")]
    pub login_sync_bytes_per_sec: usize,
    /// Max bytes per second per player after login-sync (0 = unlimited).
    #[serde(default = "default_regular")]
    pub regular_bytes_per_sec: usize,
    /// Login-sync phase duration in seconds.
    #[serde(default = "default_login_duration")]
    pub login_sync_duration_secs: u64,
}

impl Default for BandwidthConfig {
    fn default() -> Self {
        Self {
            login_sync_bytes_per_sec: default_login_sync(),
            regular_bytes_per_sec: default_regular(),
            login_sync_duration_secs: default_login_duration(),
        }
    }
}

fn default_login_sync() -> usize {
    500 * 1024
}

fn default_regular() -> usize {
    200 * 1024
}

fn default_login_duration() -> u64 {
    120
}

/// Generation settings (ENF-2).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationConfig {
    /// Blocking generation threads.
    #[serde(default = "default_threads")]
    pub threads: usize,
    /// Blocks sampled per capture pump.
    #[serde(default = "default_budget")]
    pub sample_budget: u64,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            threads: default_threads(),
            sample_budget: default_budget(),
        }
    }
}

fn default_threads() -> usize {
    2
}

fn default_budget() -> u64 {
    4096
}

/// Invalidation settings (EF-7).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvalidationConfig {
    /// `immediate`, `periodic` or `disabled`.
    #[serde(default = "default_policy")]
    pub policy: String,
}

impl Default for InvalidationConfig {
    fn default() -> Self {
        Self {
            policy: default_policy(),
        }
    }
}

fn default_policy() -> String {
    "immediate".to_string()
}

impl Config {
    /// Parses the invalidation policy from its string form.
    #[must_use]
    pub fn invalidation_policy(&self) -> InvalidationPolicy {
        match self.invalidation.policy.as_str() {
            "periodic" => InvalidationPolicy::Periodic,
            "disabled" => InvalidationPolicy::Disabled,
            _ => InvalidationPolicy::Immediate,
        }
    }

    /// Loads the configuration from `path`, falling back to defaults when the
    /// file is missing. Returns the config and an optional warning when the
    /// file could not be parsed.
    ///
    /// # Errors
    /// Never errors: a broken file yields defaults plus a warning.
    #[must_use]
    pub fn load(path: &Path) -> (Self, Option<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(config) => (config, None),
                Err(e) => (
                    Self::default_file(),
                    Some(format!("invalid {}: {e}; using defaults", path.display())),
                ),
            },
            Err(_) => (Self::default_file(), None),
        }
    }

    fn default_file() -> Self {
        Self {
            bandwidth: BandwidthConfig::default(),
            generation: GenerationConfig::default(),
            invalidation: InvalidationConfig::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::default_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let text = r#"
[bandwidth]
login_sync_bytes_per_sec = 1000
regular_bytes_per_sec = 500
login_sync_duration_secs = 60

[generation]
threads = 4
sample_budget = 8192

[invalidation]
policy = "periodic"
"#;
        let config: Config = toml::from_str(text).expect("valid config");
        assert_eq!(config.bandwidth.login_sync_bytes_per_sec, 1000);
        assert_eq!(config.generation.threads, 4);
        assert_eq!(config.invalidation_policy(), InvalidationPolicy::Periodic);
    }

    #[test]
    fn defaults_when_empty() {
        let config: Config = toml::from_str("").expect("empty config yields defaults");
        assert_eq!(config.bandwidth.login_sync_bytes_per_sec, 500 * 1024);
        assert_eq!(config.generation.threads, 2);
        assert_eq!(config.invalidation_policy(), InvalidationPolicy::Immediate);
    }

    #[test]
    fn unknown_policy_falls_back_to_immediate() {
        let config: Config = toml::from_str("[invalidation]\npolicy = \"nonsense\"")
            .expect("parseable");
        assert_eq!(config.invalidation_policy(), InvalidationPolicy::Immediate);
    }
}

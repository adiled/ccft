//! Runtime config + canonical filesystem paths.
//!
//!   ~/.config/ccft/ccft.json (or $CCFT_CONFIG)
//!     {
//!       "host":            "127.0.0.1",
//!       "port":            7178,
//!       "system_override": "",
//!       "pain":            false,
//!       "ledger":          true,
//!       "service_label":   "com.ccft"
//!     }
//!
//! Missing → defaults. Malformed → log + defaults.

use serde_json::Value;
use std::path::PathBuf;
use tracing::*;

/// Default reverse-DNS label used by the launchd plist / systemd unit
/// when the user hasn't overridden it via config or env.
pub const DEFAULT_SERVICE_LABEL: &str = "com.ccft";

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub system_override: String,
    pub pain_enabled: bool,
    pub ledger_enabled: bool,
    pub highway_enabled: bool,
    pub openai_enabled: bool,
    pub openai_targets: Vec<String>,
    /// Reverse-DNS-style identifier used for the user-mode service unit:
    /// `<label>.plist` on macOS, `<label>.service` on Linux. Defaults to
    /// `com.ccft`. Override per-install via `ccft install --label …` or
    /// the `CCFT_LABEL` env var.
    pub service_label: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 7178,
            system_override: String::new(),
            pain_enabled: false,
            ledger_enabled: true,
            highway_enabled: true,
            openai_enabled: true,
            openai_targets: Vec::new(),
            service_label: DEFAULT_SERVICE_LABEL.into(),
        }
    }
}

impl Config {
    pub fn load() -> Self {
        Self::load_from(paths::config())
    }

    pub fn load_dev() -> Self {
        Self::load_from(paths::dev_config())
    }

    pub fn load_from(path: PathBuf) -> Self {
        let mut cfg = Config::default();

        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                info!("[ccft] no config at {} — using defaults", path.display());
                return cfg;
            }
            Err(e) => {
                warn!("[ccft] config read failed at {}: {}", path.display(), e);
                return cfg;
            }
        };

        let parsed: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(e) => {
                warn!("[ccft] config parse failed: {}", e);
                return cfg;
            }
        };

        if let Some(s) = parsed.get("host").and_then(Value::as_str) {
            cfg.host = s.to_string();
        }
        if let Some(p) = parsed.get("port").and_then(Value::as_u64) {
            if (1..=u16::MAX as u64).contains(&p) {
                cfg.port = p as u16;
            }
        }
        if let Some(s) = parsed.get("system_override").and_then(Value::as_str) {
            cfg.system_override = s.to_string();
        }
        if let Some(b) = parsed.get("pain").and_then(Value::as_bool) {
            cfg.pain_enabled = b;
        }
        if let Some(b) = parsed.get("ledger").and_then(Value::as_bool) {
            cfg.ledger_enabled = b;
        }
        if let Some(b) = parsed.get("highway").and_then(Value::as_bool) {
            cfg.highway_enabled = b;
        }
        if let Some(b) = parsed.get("openai").and_then(Value::as_bool) {
            cfg.openai_enabled = b;
        }
        if let Some(arr) = parsed.get("openai_targets").and_then(Value::as_array) {
            let mut targets = Vec::new();
            for v in arr {
                if let Some(s) = v.as_str() {
                    if !s.trim().is_empty() {
                        targets.push(s.trim().to_string());
                    }
                }
            }
            cfg.openai_targets = targets;
        }
        if let Some(s) = parsed.get("service_label").and_then(Value::as_str) {
            if !s.trim().is_empty() {
                cfg.service_label = s.trim().to_string();
            }
        }

        info!(
            "[ccft] config loaded ({}): host={} port={} pain={} ledger={} highway={} label={} override={}chars openai={} targets={}",
            path.display(),
            cfg.host,
            cfg.port,
            cfg.pain_enabled,
            cfg.ledger_enabled,
            cfg.highway_enabled,
            cfg.service_label,
            cfg.system_override.len(),
            cfg.openai_enabled,
            cfg.openai_targets.join(","),
        );
        cfg
    }
}

/// Canonical filesystem layout. Single source of truth for every path the
/// binary reads or writes; install/lifecycle/dev/trust all reference these.
///
/// **Isolation:** when `CCFT_PREFIX` is set, every path is rooted under that
/// prefix instead of `$HOME`. This is the test-isolation knob — running with
/// `CCFT_PREFIX=/tmp/ccft-smoke ccft install` will install entirely into
/// `/tmp/ccft-smoke/...` and skip launchctl operations (see `is_isolated()`).
/// Production state is untouched.
pub mod paths {
    use std::path::PathBuf;

    pub fn home() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .expect("HOME unset")
    }

    /// Root directory for everything ccft owns. Defaults to `$HOME`; override
    /// with `CCFT_PREFIX` for isolated test installs.
    pub fn root() -> PathBuf {
        std::env::var_os("CCFT_PREFIX")
            .map(PathBuf::from)
            .unwrap_or_else(home)
    }

    /// True when CCFT_PREFIX is set — caller should skip launchctl mutations.
    pub fn is_isolated() -> bool {
        std::env::var_os("CCFT_PREFIX").is_some()
    }

    pub fn ca_dir() -> PathBuf {
        root().join(".cc-flytrap")
    }
    pub fn ca_pem() -> PathBuf {
        ca_dir().join("ca.pem")
    }
    pub fn ca_key() -> PathBuf {
        ca_dir().join("ca.key")
    }

    pub fn env_file() -> PathBuf {
        ca_dir().join("ccft.env")
    }

    pub fn config_dir() -> PathBuf {
        root().join(".config").join("ccft")
    }
    pub fn config() -> PathBuf {
        if let Some(p) = std::env::var_os("CCFT_CONFIG") {
            return PathBuf::from(p);
        }
        config_dir().join("ccft.json")
    }
    pub fn dev_config() -> PathBuf {
        if let Some(p) = std::env::var_os("CCFT_CONFIG") {
            return PathBuf::from(p);
        }
        config_dir().join("dev.json")
    }

    pub fn share_dir() -> PathBuf {
        root().join(".local").join("share").join("ccft")
    }
    pub fn ledger() -> PathBuf {
        if let Some(p) = std::env::var_os("CCFT_LEDGER") {
            return PathBuf::from(p);
        }
        share_dir().join("ledger.jsonl")
    }
    pub fn state() -> PathBuf {
        let mut p = ledger();
        p.set_file_name("state.jsonl");
        p
    }
    pub fn log_dir() -> PathBuf {
        share_dir().join("logs")
    }
    pub fn launchd_log() -> PathBuf {
        log_dir().join("launchd.log")
    }

    pub fn install_bin_dir() -> PathBuf {
        root().join(".local").join("bin")
    }
    pub fn install_bin() -> PathBuf {
        install_bin_dir().join("ccft")
    }
}

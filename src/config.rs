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

/// Known model-provider / OpenAI-compatible hosts ccft flytraps by default.
/// Each entry is `host` or `host:port`; a bare host implies the protocol's
/// default port (443 — flytrap only touches TLS CONNECT tunnels).
///
/// Only hosts with **dedicated, exclusive** ports are included. Shared ports
/// (8080, 8000, 5000) are used by many unrelated services, so they're left
/// out to avoid intercepting traffic that isn't an OpenAI-format endpoint.
/// Add a host here only when it owns its port.
pub const DEFAULT_HOSTS: &[&str] = &[
    // Anthropic / OpenAI first-party coding agents.
    "api.anthropic.com", // Anthropic /v1/messages
    "api.openai.com",    // OpenAI API / Codex (chat.completions)
    // Local OpenAI-compatible servers with dedicated default ports.
    "127.0.0.1:11434", // Ollama
    "0.0.0.0:11434",   // Ollama (all-interfaces listen)
    "127.0.0.1:1234",  // LM Studio
    "127.0.0.1:1337",  // Jan
    "127.0.0.1:4891",  // GPT4All
];

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub system_override: String,
    pub pain_enabled: bool,
    pub ledger_enabled: bool,
    pub highway_enabled: bool,
    /// `host[:port]` CONNECT targets to flytrap (intercept + decrypt + re-sign).
    /// A bare host implies the protocol's default port (443 — flytrap only
    /// touches TLS CONNECT tunnels). Only hosts with dedicated, exclusive
    /// ports belong here: shared ports (8080, 8000, 5000) would intercept
    /// unrelated services. Everything not listed gets a raw passthrough tunnel.
    pub hosts: Vec<String>,
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
            hosts: DEFAULT_HOSTS.iter().map(|s| s.to_string()).collect(),
            // In dev mode the default service unit is com.ccft.dev so the
            // parallel dev system never collides with production.
            service_label: if paths::is_dev() {
                "com.ccft.dev".into()
            } else {
                DEFAULT_SERVICE_LABEL.into()
            },
        }
    }
}

/// Env vars that point an OpenAI-compatible / Anthropic client at a specific
/// endpoint. ccft reads these so a locally-configured server (e.g. Ollama via
/// OLLAMA_HOST) is auto-discovered and added to the `hosts` flytrap list.
const HOST_ENV_VARS: &[&str] = &[
    "OLLAMA_HOST",
    "OPENAI_BASE_URL",
    "OPENAI_API_BASE",
    "ANTHROPIC_BASE_URL",
];

/// Parse an env value like `127.0.0.1:11434`, `http://127.0.0.1:11434`, or
/// `http://127.0.0.1:11434/v1` into a `host[:port]` flytrap entry. A bare host
/// keeps no port — the flytrap match applies the 443 default.
pub(crate) fn host_from_env(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    let v = v.split("://").nth(1).unwrap_or(v);
    let authority = v.split('/').next().unwrap_or(v);
    if authority.is_empty() {
        return None;
    }
    Some(authority.to_string())
}

/// Seed `cfg.hosts` from `*_HOST` / `*_BASE_URL` env vars (union, dedup).
fn env_hosts(cfg: &mut Config) {
    for var in HOST_ENV_VARS {
        if let Ok(v) = std::env::var(var) {
            if let Some(h) = host_from_env(&v) {
                if !cfg.hosts.iter().any(|e| e == &h) {
                    cfg.hosts.push(h);
                }
            }
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
        if let Some(arr) = parsed.get("hosts").and_then(Value::as_array) {
            let mut hosts = Vec::new();
            for v in arr {
                if let Some(s) = v.as_str() {
                    if !s.trim().is_empty() {
                        hosts.push(s.trim().to_string());
                    }
                }
            }
            // Empty array → flytrap nothing. Absent → DEFAULT_HOSTS.
            cfg.hosts = hosts;
        }
        if let Some(s) = parsed.get("service_label").and_then(Value::as_str) {
            if !s.trim().is_empty() {
                cfg.service_label = s.trim().to_string();
            }
        }

        // Union in hosts discovered from `*_HOST` / `*_BASE_URL` env vars
        // (e.g. OLLAMA_HOST), so a locally-configured server is flytrapped
        // even when it isn't in the config file.
        env_hosts(&mut cfg);

        info!(
             "[ccft] config loaded ({}): host={} port={} pain={} ledger={} highway={} hosts={} label={} override={}chars",
            path.display(),
            cfg.host,
            cfg.port,
            cfg.pain_enabled,
            cfg.ledger_enabled,
            cfg.highway_enabled,
            cfg.hosts.join(","),
            cfg.service_label,
            cfg.system_override.len(),
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
///
/// **Dev mode:** when `CCFT_DEV=1` is set, the whole binary operates as a
/// parallel dev system — dev config, dev ledger, dev log, and a separate
/// `com.ccft.dev` service unit. Production state is untouched. This is what
/// `ccft dev` sets so the same binary/commands run independently of the main
/// install.
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

    /// True when CCFT_DEV is set — operate on the parallel dev system.
    pub fn is_dev() -> bool {
        std::env::var_os("CCFT_DEV").is_some()
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
        if is_dev() {
            config_dir().join("dev.json")
        } else {
            config_dir().join("ccft.json")
        }
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
        if is_dev() {
            share_dir().join("dev").join("ledger.jsonl")
        } else {
            share_dir().join("ledger.jsonl")
        }
    }
    pub fn state() -> PathBuf {
        let mut p = ledger();
        p.set_file_name("state.jsonl");
        p
    }
    pub fn log_dir() -> PathBuf {
        if is_dev() {
            share_dir().join("dev").join("logs")
        } else {
            share_dir().join("logs")
        }
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

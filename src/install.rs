//! install / uninstall — copy the binary to `~/.local/bin/ccft`, set up CA
//! and config, and register with the platform's user-mode service manager
//! (launchd / systemd-user). Idempotent both directions.

use crate::config::{paths, DEFAULT_HOSTS, DEFAULT_SERVICE_LABEL};
use crate::service;
use crate::trust;
use std::fs;
#[cfg(target_os = "macos")]
use std::process::Command;

/// Validate a user-supplied service label. Reverse-DNS-style: dot-separated
/// alphanumeric segments with optional hyphens. Rejects values that would
/// produce a malformed plist Label or a systemd unit name with whitespace.
fn validate_label(s: &str) -> Result<(), String> {
    if s.is_empty() || s.len() > 200 {
        return Err(format!("label must be 1..200 chars (got {})", s.len()));
    }
    for ch in s.chars() {
        let ok = ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == '_';
        if !ok {
            return Err(format!(
                "label can only contain [A-Za-z0-9._-]; got '{}'",
                ch
            ));
        }
    }
    if s.starts_with('.') || s.ends_with('.') {
        return Err("label cannot start or end with '.'".into());
    }
    Ok(())
}

pub fn install(label: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let src = std::env::current_exe()?;
    let dst = paths::install_bin();

    fs::create_dir_all(paths::install_bin_dir())?;
    fs::create_dir_all(paths::log_dir())?;
    fs::create_dir_all(paths::config_dir())?;

    // 0. Resolve and validate the service label.
    //    Precedence: explicit --label flag > existing config > default.
    //    If --label was passed, persist it to config FIRST so write_unit /
    //    register pick it up via service::label().
    let cfg_path = paths::config();
    let resolved_label: String = match label {
        Some(l) => {
            validate_label(&l).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            // Read-modify-write the config to persist the chosen label.
            persist_service_label(&cfg_path, &l)?;
            println!("✓ service label set to '{}'", l);
            l
        }
        None => crate::config::Config::load().service_label,
    };

    // 1. Copy ourselves to the install location if running from elsewhere.
    if src != dst {
        let _ = service::bootout();
        if dst.exists() {
            fs::remove_file(&dst)?;
        }
        fs::copy(&src, &dst)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&dst)?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&dst, perms)?;
        }

        // macOS gotcha: overwriting a code-signed binary in place leaves
        // the kernel's path-cache flagging that location as "tampered" →
        // future invocations get SIGKILLed before main() runs. Re-sign
        // adhoc at the new path. Failure here is non-fatal.
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("codesign")
                .args(["--force", "--sign", "-", dst.to_string_lossy().as_ref()])
                .status();
        }
        println!("✓ installed binary {}", dst.display());
    } else {
        println!("✓ binary already at {}", dst.display());
    }

    // 2. Default config if none. Includes service_label so subsequent
    //    operations find the same service unit.
    if !cfg_path.exists() {
        // Dev mode installs into dev.json with the dev port so the parallel
        // system runs independently on 7179.
        let port = if paths::is_dev() { 7179 } else { 7178 };
        let mut default_cfg = serde_json::json!({
            "host":            "127.0.0.1",
            "port":            port,
            "system_override": "",
            "pain":            false,
            "ledger":          true,
            "service_label":   resolved_label,
        });
        if let Some(obj) = default_cfg.as_object_mut() {
            obj.insert(
                "hosts".into(),
                DEFAULT_HOSTS
                    .iter()
                    .map(|h| serde_json::json!(h))
                    .collect(),
            );
        }
        fs::write(&cfg_path, serde_json::to_string_pretty(&default_cfg)? + "\n")?;
        println!("✓ wrote default config {}", cfg_path.display());
    } else {
        println!("✓ config exists at {}", cfg_path.display());
    }

    // 3. CA on disk.
    trust::ensure_ca()?;

    // 4. Register with the platform's service manager (or skip on Windows).
    if service::supported() {
        service::write_unit(&dst)?;
        if paths::is_isolated() {
            println!(
                "✓ service unit written; {} skipped (CCFT_PREFIX={})",
                service::manager_name(),
                paths::root().display()
            );
        } else {
            service::register()?;
            println!(
                "✓ {} service '{}' registered",
                service::manager_name(),
                service::label()
            );
        }
    } else {
        println!(
            "ℹ service auto-start not implemented on this platform — run `ccft run` manually."
        );
    }

    println!();
    println!("ccft - an agentic self improvement tool — installed.");
    println!();
    if !paths::is_isolated() {
        trust::print_instructions();
    }
    Ok(())
}

/// Read the existing config (if any), update / insert `service_label`, and
/// write it back. Preserves any unrelated keys the user may have added.
fn persist_service_label(
    cfg_path: &std::path::Path,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut value: serde_json::Value = match fs::read_to_string(cfg_path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };
    if !value.is_object() {
        value = serde_json::json!({});
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "service_label".to_string(),
            serde_json::Value::String(label.to_string()),
        );
        // Backfill defaults if the file is brand new.
        let port = if paths::is_dev() { 7179 } else { 7178 };
        obj.entry("host").or_insert(serde_json::json!("127.0.0.1"));
        obj.entry("port").or_insert(serde_json::json!(port));
        obj.entry("system_override").or_insert(serde_json::json!(""));
        obj.entry("pain").or_insert(serde_json::json!(false));
        obj.entry("ledger").or_insert(serde_json::json!(true));
    }
    fs::write(cfg_path, serde_json::to_string_pretty(&value)? + "\n")?;
    let _ = DEFAULT_SERVICE_LABEL; // pin the constant as a referenced symbol
    Ok(())
}

pub fn uninstall() -> Result<(), Box<dyn std::error::Error>> {
    if service::supported() {
        service::unregister()?;
        if paths::is_isolated() {
            println!("✓ isolated mode — service unregister was a no-op");
        } else {
            println!("✓ {} service unregistered", service::manager_name());
        }
    }

    let bin = paths::install_bin();
    if bin.exists() {
        fs::remove_file(&bin)?;
        println!("✓ removed {}", bin.display());
    }

    println!();
    println!("ccft uninstalled.");
    println!("  CA cert kept at: {}", paths::ca_dir().display());
    println!("  Config kept at:  {}", paths::config().display());
    println!("  Ledger kept at:  {}", paths::ledger().display());
    println!("  (delete by hand if you want a full purge)");
    Ok(())
}

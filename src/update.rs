//! Update: explicit `ccft update` and startup auto-update.
//!
//! Distribution is via crates.io (`cargo install ccft`), so the update path
//! is: fetch the newest crate, build it into `~/.cargo/bin`, copy the fresh
//! binary into the install location (`~/.local/bin`), re-sign (macOS), re-apply
//! trust, and restart the service. Auto-update does the same at `ccft run`
//! startup and then exits so launchd / systemd relaunch the new binary.

use crate::config::{paths, Config};
use std::process::Command;

/// The crate name we install/update from on crates.io.
const CRATE: &str = "ccft";

/// The crates.io API URL for the latest version of `ccft`.
const CRATES_IO_URL: &str = "https://crates.io/api/v1/crates/ccft";

/// Read the version of the installed binary (the running executable).
fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Fetch the latest published version from crates.io. Returns None on any
/// network/parse error (auto-update must never take the machine down because
/// the registry was unreachable).
async fn latest_version() -> Option<String> {
    use http_body_util::BodyExt;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let client = Client::builder(TokioExecutor::new()).build_http();
    let req = hyper::Request::builder()
        .uri(CRATES_IO_URL)
        .header("User-Agent", concat!("ccft/", env!("CARGO_PKG_VERSION")))
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .ok()?;
    let resp = client.request(req).await.ok()?;
    let body = resp.into_body().collect().await.ok()?;
    let v: serde_json::Value = serde_json::from_slice(&body.to_bytes()).ok()?;
    let vers = v.pointer("/crate/max_version")?.as_str()?.to_string();
    Some(vers)
}

/// Compare versions `a` and `b` (semver-ish). Returns true if `a` > `b`.
fn newer(a: &str, b: &str) -> bool {
    fn parts(s: &str) -> Vec<u64> {
        s.trim()
            .split('.')
            .map(|p| p.split('-').next().unwrap_or("0").parse().unwrap_or(0))
            .collect()
    }
    let pa = parts(a);
    let pb = parts(b);
    pa.iter()
        .zip(pb.iter())
        .find(|(x, y)| x != y)
        .map(|(x, y)| x > y)
        .unwrap_or_else(|| pa.len() > pb.len())
}

/// Copy the running/installed binary into the install location and re-sign.
/// Reuses the same dance as `ccft install`: bootout first, replace, chmod 755,
/// adhoc re-sign on macOS (overwriting a code-signed binary in place leaves the
/// kernel's path-cache flagging that path as tampered → SIGKILL).
fn place_binary() -> Result<(), Box<dyn std::error::Error>> {
    let src = std::env::current_exe()?;
    let dst = paths::install_bin();
    std::fs::create_dir_all(paths::install_bin_dir())?;

    let _ = crate::service::bootout();
    if dst.exists() {
        std::fs::remove_file(&dst)?;
    }
    std::fs::copy(&src, &dst)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&dst)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&dst, perms)?;
    }

    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("codesign")
            .args(["--force", "--sign", "-", dst.to_string_lossy().as_ref()])
            .status();
    }
    Ok(())
}

/// Re-apply trust (env + shell RC sourcing + ~/.claude.json) and restart the
/// service so the new binary is the one launchd/systemd runs.
fn finish_update() -> Result<(), Box<dyn std::error::Error>> {
    crate::trust::apply_with(false)?;
    if crate::service::supported() {
        crate::service::register()?;
        println!("✓ {} service restarted on the new binary", crate::service::manager_name());
    }
    Ok(())
}

/// Explicit `ccft update`: pull + build the newest crate, install, re-apply
/// trust, restart.
pub fn update() -> Result<(), Box<dyn std::error::Error>> {
    println!("Updating {} from crates.io…", CRATE);
    let status = Command::new("cargo")
        .args(["install", CRATE, "--force"])
        .status()?;
    if !status.success() {
        return Err("cargo install failed — see output above".into());
    }

    // cargo install puts the fresh binary in ~/.cargo/bin/ccft; re-run it so
    // the install-location copy + trust + service restart happen from the new
    // executable.
    let new_bin = paths::home().join(".cargo").join("bin").join("ccft");
    if new_bin.exists() {
        let status = Command::new(&new_bin).args(["install"]).status()?;
        if !status.success() {
            return Err("fresh binary's install step failed".into());
        }
        return Ok(());
    }

    // Fallback: we may already BE the new binary (cargo install replaced us in
    // ~/.cargo/bin and re-invoked). Just place + trust + restart.
    place_binary()?;
    finish_update()?;
    println!("✓ ccft updated to {}", current_version());
    Ok(())
}

/// Startup auto-update, called from `ccft run`. If the registry has a newer
/// version, install it and exit so the service manager relaunches the new
/// binary. Never fatal — network hiccups just skip.
pub async fn maybe_auto_update(cfg: &Config) {
    if paths::is_isolated() {
        return;
    }
    // Only auto-update the production flytrap; dev mode is a moving target.
    if cfg.service_label != crate::config::DEFAULT_SERVICE_LABEL {
        return;
    }
    let cur = current_version();
    let latest = match latest_version().await {
        Some(v) => v,
        None => {
            tracing::info!("[ccft] update check skipped (registry unreachable)");
            return;
        }
    };
    if !newer(&latest, &cur) {
        tracing::info!("[ccft] up to date ({})", cur);
        return;
    }

    tracing::info!("[ccft] newer version {} available — updating", latest);
    // We're the running proxy; hand off to the update path and exit so
    // launchd/systemd restart us with the fresh binary.
    if update().is_ok() {
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_compare() {
        assert!(newer("1.11.0", "1.10.0"));
        assert!(newer("1.10.1", "1.10.0"));
        assert!(!newer("1.9.0", "1.10.0"));
        assert!(!newer("1.10.0", "1.10.0"));
        assert!(newer("2.0.0", "1.99.0"));
    }
}

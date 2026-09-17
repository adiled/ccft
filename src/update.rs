//! Update: explicit `ccft update` and startup auto-update.
//! Distribution is via crates.io (`cargo install ccft`); the fresh binary in
//! `~/.cargo/bin` then runs `ccft install` to re-place the service copy, re-apply
//! trust, and restart via launchd/systemd.
//!
//! The service's own auto-update never bootouts in-place from inside the job:
//! `install` boots out the launchd job, which SIGTERMs the whole job process
//! group — an updater running inside the job dies mid-copy, leaving the
//! install binary deleted and every relaunch SIGKILLed (taskgated invalid
//! signature on the tampered path). So the service spawns the installer as a
//! detached session leader and exits; a lock file plus a stale-binary guard
//! keep relaunches from re-entering the update race.

use crate::config::{paths, Config};
use std::process::Command;

const CRATE: &str = "ccft";
const CRATES_IO_URL: &str = "https://crates.io/api/v1/crates/ccft";
const UPDATE_LOCK: &str = "updating.lock";

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

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

/// True when this process is the launchd/systemd-managed service instance:
/// running from the install path with launchd (pid 1) as parent.
fn is_service_instance() -> bool {
    if paths::is_isolated() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    if exe != paths::install_bin() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::parent_id;
        return parent_id() == 1;
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn take_update_lock() -> bool {
    let dir = paths::share_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let lock = dir.join(UPDATE_LOCK);
    // A crashed updater can leave a stale lock; age it out after 30 min.
    if let Ok(meta) = std::fs::metadata(&lock) {
        if let Ok(modified) = meta.modified() {
            if modified.elapsed().map(|e| e.as_secs() > 1800).unwrap_or(false) {
                let _ = std::fs::remove_file(&lock);
            }
        }
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock)
        .is_ok()
}

fn release_update_lock() {
    let _ = std::fs::remove_file(paths::share_dir().join(UPDATE_LOCK));
}

/// Run `ccft install` from the freshly cargo-installed binary. Detached from
/// our process group when we're the service instance, so the installer's
/// bootout can't kill it; in-process otherwise (`ccft update` on a terminal).
fn run_installer(new_bin: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if is_service_instance() {
        let mut cmd = Command::new(new_bin);
        cmd.arg("install");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
        let _spawned = cmd.spawn()?;
        Ok(())
    } else {
        let status = Command::new(new_bin).arg("install").status()?;
        if !status.success() {
            return Err(format!("fresh binary's install step failed: {}", status).into());
        }
        Ok(())
    }
}

pub fn update() -> Result<(), Box<dyn std::error::Error>> {
    println!("Updating {} from crates.io…", CRATE);
    let status = Command::new("cargo")
        .args(["install", CRATE, "--force"])
        .status()?;
    if !status.success() {
        return Err("cargo install failed — see output above".into());
    }

    let new_bin = paths::home().join(".cargo").join("bin").join("ccft");
    if new_bin.exists() {
        run_installer(&new_bin)?;
        return Ok(());
    }

    let src = std::env::current_exe()?;
    let dst = paths::install_bin();
    crate::install::place_binary(&src, &dst)?;
    crate::trust::apply_with(false)?;
    if crate::service::supported() {
        crate::service::register()?;
        println!("✓ {} service restarted on the new binary", crate::service::manager_name());
    }
    println!("✓ ccft updated to {}", current_version());
    Ok(())
}

pub async fn maybe_auto_update(cfg: &Config) {
    if paths::is_isolated() {
        return;
    }
    if cfg.service_label != crate::config::DEFAULT_SERVICE_LABEL {
        return;
    }

    // Stale relaunch: an update already replaced the install binary with a
    // newer file than the one we're executing from (we were relaunched off a
    // bootout). Don't re-enter the update race — die and let the fresh
    // service take over.
    if let Ok(exe) = std::env::current_exe() {
        let dst = paths::install_bin();
        if dst.exists() && dst != exe {
            if let (Ok(a), Ok(b)) = (std::fs::metadata(&exe), std::fs::metadata(&dst)) {
                if let (Ok(at), Ok(bt)) = (a.modified(), b.modified()) {
                    if bt > at {
                        tracing::info!("[ccft] stale copy — install binary is newer, exiting");
                        std::process::exit(0);
                    }
                }
            }
        }
    }

    if !take_update_lock() {
        tracing::info!("[ccft] update already in progress, skipping");
        return;
    }

    let cur = current_version();
    let latest = match latest_version().await {
        Some(v) => v,
        None => {
            release_update_lock();
            tracing::info!("[ccft] update check skipped (registry unreachable)");
            return;
        }
    };
    if !newer(&latest, &cur) {
        release_update_lock();
        tracing::info!("[ccft] up to date ({})", cur);
        return;
    }

    tracing::info!("[ccft] newer version {} available — updating", latest);
    if update().is_ok() {
        // Lock stays held: the detached installer clears it on completion.
        std::process::exit(0);
    }
    release_update_lock();
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
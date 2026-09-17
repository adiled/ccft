//! Lifecycle: start / stop / restart / status.
//!
//! Goes through the platform's service manager (launchd on macOS, systemd-user
//! on Linux). One `state()` function is the source of truth for the printed
//! status; start/stop/restart are thin wrappers around the service module.

use crate::config::{paths, Config};
use crate::service;
use std::net::TcpStream;
use std::process::Command;
use std::time::Duration;

#[derive(Debug)]
pub enum State {
    /// Service-managed and bound to the configured port.
    Running { pid: u32 },
    /// Service registered but not currently running.
    Idle,
    /// Something else is bound to our port (foreign proxy or stale process).
    PortBoundForeign,
    /// Not installed at all.
    NotInstalled,
}

pub fn state(cfg: &Config) -> State {
    let registered = service::is_registered();

    if !registered && !service::unit_path().exists() && !paths::install_bin().exists() {
        return State::NotInstalled;
    }

    let bound_pid = port_bound(&cfg.host, cfg.port);

    if registered {
        if let Some(pid) = bound_pid {
            return State::Running { pid };
        }
        return State::Idle;
    }

    if bound_pid.is_some() {
        return State::PortBoundForeign;
    }
    State::NotInstalled
}

pub fn print_status(cfg: &Config) {
    match state(cfg) {
        State::Running { pid } => {
            println!(
                "ccft running on {}:{} (pid {}, {})",
                cfg.host,
                cfg.port,
                pid,
                service::manager_name()
            );
        }
        State::Idle => {
            println!(
                "ccft installed ({}) but not bound to {}:{} — kick with `ccft start`",
                service::manager_name(),
                cfg.host,
                cfg.port
            );
        }
        State::PortBoundForeign => {
            println!(
                "ccft NOT installed via {}, but {}:{} is in use by another process",
                service::manager_name(),
                cfg.host,
                cfg.port
            );
        }
        State::NotInstalled => {
            println!("ccft not installed. Run: `ccft install`");
        }
    }
}

pub fn start(cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
    if !service::is_registered() {
        return Err("ccft is not installed. Run: ccft install".into());
    }
    if paths::is_isolated() {
        return Err("isolated mode — start/stop are no-ops; use `ccft run` directly".into());
    }
    crate::install::ensure_installed()?;
    service::kickstart()?;
    println!("✓ kicked {} service", service::manager_name());
    for _ in 0..50 {
        if port_bound(&cfg.host, cfg.port).is_some() {
            println!("✓ bound on {}:{}", cfg.host, cfg.port);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!(
        "service kicked but not bound to {}:{} after 5s — check `ccft logs`",
        cfg.host, cfg.port
    )
    .into())
}

pub fn stop(_cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
    if !service::is_registered() {
        println!("ccft not installed — nothing to stop");
        return Ok(());
    }
    if paths::is_isolated() {
        println!("isolated mode — nothing to stop");
        return Ok(());
    }
    service::bootout()?;
    println!("✓ stopped (will restart on next login unless you `ccft uninstall`)");
    Ok(())
}

pub fn restart(cfg: &Config) -> Result<(), Box<dyn std::error::Error>> {
    if !service::is_registered() {
        return Err("ccft is not installed. Run: ccft install".into());
    }
    if paths::is_isolated() {
        return Err("isolated mode — restart is a no-op; use `ccft run` directly".into());
    }
    service::kickstart()?;
    for _ in 0..20 {
        if port_bound(&cfg.host, cfg.port).is_some() {
            println!("✓ restarted, bound on {}:{}", cfg.host, cfg.port);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("kickstart issued but port not bound after 2s".into())
}

fn port_bound(host: &str, port: u16) -> Option<u32> {
    // TCP probe — if connect succeeds, something's listening. Then ask `lsof`
    // (terse mode, listening sockets only) for the pid. Not load-bearing for
    // state correctness — used for display.
    let addr = format!("{}:{}", host, port);
    if TcpStream::connect_timeout(&addr.parse().ok()?, Duration::from_millis(50)).is_err() {
        return None;
    }
    let out = Command::new("lsof")
        .args(["-t", "-nP", &format!("-iTCP:{}", port), "-sTCP:LISTEN"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().next().and_then(|l| l.trim().parse().ok())
}

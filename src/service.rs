//! Wraps each platform's "run this binary at login, restart on exit" mechanism

use crate::config::Config;
use std::path::{Path, PathBuf};

pub fn label() -> String {
    Config::load().service_label
}

pub fn unit_path() -> PathBuf {
    platform::unit_path()
}

pub fn write_unit(bin: &Path) -> Result<(), Box<dyn std::error::Error>> {
    platform::write_unit(bin)
}

pub fn register() -> Result<(), Box<dyn std::error::Error>> {
    platform::register()
}

pub fn unregister() -> Result<(), Box<dyn std::error::Error>> {
    platform::unregister()
}

pub fn kickstart() -> Result<(), Box<dyn std::error::Error>> {
    platform::kickstart()
}

pub fn bootout() -> Result<(), Box<dyn std::error::Error>> {
    platform::bootout()
}

pub fn is_registered() -> bool {
    platform::is_registered()
}

pub fn supported() -> bool {
    platform::SUPPORTED
}

pub fn manager_name() -> &'static str {
    platform::MANAGER_NAME
}

// ─── macOS: launchd ──────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use crate::config::paths;
    use std::fs;
    use std::io::Write;
    use std::process::Command;

    pub const SUPPORTED: bool = true;
    pub const MANAGER_NAME: &str = "launchd";

    pub fn unit_path() -> PathBuf {
        paths::root()
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", super::label()))
    }

    fn unit_dir() -> PathBuf {
        unit_path().parent().unwrap().to_path_buf()
    }

    pub fn write_unit(bin: &Path) -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(unit_dir())?;
        let log = paths::launchd_log();
        let env_block = if paths::is_dev() {
            "    <key>EnvironmentVariables</key>\n    <dict>\n        <key>CCFT_DEV</key>\n        <string>1</string>\n    </dict>\n"
        } else {
            ""
        };
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{label}</string>
{env}    <key>ProgramArguments</key>
    <array>
        <string>{bin}</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>{log}</string>
    <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
            label = super::label(),
            env = env_block,
            bin = bin.display(),
            log = log.display(),
        );
        let mut f = fs::File::create(unit_path())?;
        f.write_all(plist.as_bytes())?;
        Ok(())
    }

    pub fn register() -> Result<(), Box<dyn std::error::Error>> {
        if paths::is_isolated() {
            return Ok(());
        }
        let _ = bootout();
        let target = launchctl_user_target();
        let status = Command::new("launchctl")
            .args(["bootstrap", &target, unit_path().to_string_lossy().as_ref()])
            .status()?;
        if !status.success() {
            return Err(format!("launchctl bootstrap failed: {}", status).into());
        }
        Ok(())
    }

    pub fn unregister() -> Result<(), Box<dyn std::error::Error>> {
        if !paths::is_isolated() {
            let _ = bootout();
        }
        if unit_path().exists() {
            fs::remove_file(unit_path())?;
        }
        Ok(())
    }

    pub fn kickstart() -> Result<(), Box<dyn std::error::Error>> {
        let target = format!("{}/{}", launchctl_user_target(), super::label());
        let status = Command::new("launchctl")
            .args(["kickstart", "-k", &target])
            .status()?;
        if !status.success() {
            return Err(format!("launchctl kickstart failed: {}", status).into());
        }
        Ok(())
    }

    pub fn bootout() -> Result<(), Box<dyn std::error::Error>> {
        let target = format!("{}/{}", launchctl_user_target(), super::label());
        let _ = Command::new("launchctl")
            .args(["bootout", &target])
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status();
        Ok(())
    }

    pub fn is_registered() -> bool {
        if paths::is_isolated() {
            return unit_path().exists() && paths::install_bin().exists();
        }
        let target = format!("{}/{}", launchctl_user_target(), super::label());
        Command::new("launchctl")
            .args(["print", &target])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn launchctl_user_target() -> String {
        format!("gui/{}", libc_uid())
    }

    fn libc_uid() -> u32 {
        unsafe extern "C" {
            safe fn getuid() -> u32;
        }
        getuid()
    }
}

// ─── Linux: systemd-user ─────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    use crate::config::paths;
    use std::fs;
    use std::process::Command;

    pub const SUPPORTED: bool = true;
    pub const MANAGER_NAME: &str = "systemd";

    pub fn unit_path() -> PathBuf {
        paths::root()
            .join(".config")
            .join("systemd")
            .join("user")
            .join(format!("{}.service", super::label()))
    }

    fn unit_dir() -> PathBuf {
        unit_path().parent().unwrap().to_path_buf()
    }

    pub fn write_unit(bin: &Path) -> Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(unit_dir())?;
        let env_line = if paths::is_dev() {
            "Environment=CCFT_DEV=1\n"
        } else {
            ""
        };
        let unit = format!(
            r#"[Unit]
Description=ccft
After=network-online.target

[Service]
Type=simple
{env}ExecStart={bin} run
Restart=always
RestartSec=2

[Install]
WantedBy=default.target
"#,
            env = env_line,
            bin = bin.display(),
        );
        fs::write(unit_path(), unit)?;
        Ok(())
    }

    pub fn register() -> Result<(), Box<dyn std::error::Error>> {
        if paths::is_isolated() {
            return Ok(());
        }
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .status();
        let label = super::label();
        let status = Command::new("systemctl")
            .args(["--user", "enable", "--now", &label])
            .status()?;
        if !status.success() {
            return Err(format!("systemctl --user enable failed: {}", status).into());
        }
        Ok(())
    }

    pub fn unregister() -> Result<(), Box<dyn std::error::Error>> {
        if !paths::is_isolated() {
            let label = super::label();
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "--now", &label])
                .stderr(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .status();
        }
        if unit_path().exists() {
            fs::remove_file(unit_path())?;
        }
        if !paths::is_isolated() {
            let _ = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status();
        }
        Ok(())
    }

    pub fn kickstart() -> Result<(), Box<dyn std::error::Error>> {
        let label = super::label();
        let status = Command::new("systemctl")
            .args(["--user", "restart", &label])
            .status()?;
        if !status.success() {
            return Err(format!("systemctl --user restart failed: {}", status).into());
        }
        Ok(())
    }

    pub fn bootout() -> Result<(), Box<dyn std::error::Error>> {
        let label = super::label();
        let _ = Command::new("systemctl")
            .args(["--user", "stop", &label])
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status();
        Ok(())
    }

    pub fn is_registered() -> bool {
        if paths::is_isolated() {
            return unit_path().exists() && paths::install_bin().exists();
        }
        let label = super::label();
        Command::new("systemctl")
            .args(["--user", "is-enabled", &label])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

// ─── Windows: not yet implemented ────────────────────────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use super::*;

    pub const SUPPORTED: bool = false;
    pub const MANAGER_NAME: &str = "(none)";

    pub fn unit_path() -> PathBuf {
        PathBuf::new()
    }

    pub fn write_unit(_bin: &Path) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    pub fn register() -> Result<(), Box<dyn std::error::Error>> {
        eprintln!("Note: ccft service auto-start is not yet implemented on Windows.");
        eprintln!("      Run `ccft run` manually, or wrap with NSSM / sc.exe.");
        Ok(())
    }

    pub fn unregister() -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    pub fn kickstart() -> Result<(), Box<dyn std::error::Error>> {
        Err("ccft service mode not supported on Windows yet".into())
    }

    pub fn bootout() -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }

    pub fn is_registered() -> bool {
        false
    }
}

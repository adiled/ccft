//! ccft - an agentic self improvement tool.
//!
//! Single-binary streaming flytrap on top of hudsucker. Listens between
//! a coding agent and its API endpoint, mutates the request system prompt
//! per ~/.config/ccft/ccft.json, and writes a per-response token ledger
//! while preserving the upstream stream byte-for-byte to the client.

mod brainrot;
mod config;
mod handler;
mod install;
mod ledger;
mod ledger_read;
mod lifecycle;
mod perf;
mod flytrap;
mod seed;
mod service;
mod session;
mod sse_tap;
mod theme;
mod trust;
mod tui;

use clap::{Parser, Subcommand};
use config::Config;

#[derive(Parser)]
#[command(name = "ccft", version, about = "ccft - an agentic self improvement tool")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the interactive TUI (default when invoked with no args at a tty).
    Tui {
        #[arg(long)]
        dev: bool,
    },
    /// Run the flytrap in the foreground with the production config.
    /// (This is what launchd invokes after `ccft install`.)
    Run,
    /// Run the flytrap in the foreground with the dev config (port 7179, isolated ledger).
    Dev,
    /// Install: copy this binary, generate CA, write launchd plist, bootstrap.
    Install {
        /// Reverse-DNS-style identifier for the installed service unit
        /// (`<label>.plist` on macOS, `<label>.service` on Linux). Defaults
        /// to `com.ccft`. Persisted to config so subsequent `ccft start /
        /// stop / restart / uninstall` find the same unit.
        #[arg(long)]
        label: Option<String>,
    },
    /// Uninstall: bootout, remove plist + installed binary. Keeps CA + ledger.
    Uninstall,
    /// Show whether ccft is installed, loaded, and bound.
    Status,
    /// Kick the launchd service.
    Start,
    /// Bootout from launchd.
    Stop,
    /// Bootout + bootstrap.
    Restart,
    /// Print env vars to route any coding agent through ccft, or apply/revoke.
    Trust {
        /// Write HTTPS_PROXY + NODE_EXTRA_CA_CERTS into ~/.cc-flytrap/ccft.env and source it from every shell RC found in $HOME (with backup).
        #[arg(long)]
        apply: bool,
        /// Remove the sourced flytrap env block from every shell RC found in $HOME (with backup).
        #[arg(long)]
        revoke: bool,
        /// Dump the CA cert PEM to stdout.
        #[arg(long)]
        ca: bool,
        #[arg(long)]
        dev: bool,
    },
    /// Tail the launchd output log.
    Logs {
        /// Number of lines from the end to start with.
        #[arg(short, long, default_value_t = 50)]
        n: usize,
    },
    /// Time-series vibe analyzer over the ledger (today, score, ...).
    Brainrot {
        /// Subcommand and args, e.g. `today`, `score 24h`.
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        #[arg(long)]
        dev: bool,
    },
    /// Perf observability: is ccft slowing requests down?
    Perf {
        /// Range, e.g. `today`, `7d`, `24h`. Default: today.
        #[arg(trailing_var_arg = true)]
        range: Vec<String>,
    },
    /// Seed the ledger from a coding agent's local session JSONLs at
    /// ~/.claude/projects/. Semantics: **session is the unit of
    /// replacement.** For each affected session (selected via --session
    /// or by date range with --since/--until — applied to the session's
    /// START date, not per-turn), every existing ledger row for that
    /// session is dropped, and one fresh row is inserted per
    /// user→assistant turn pair found in the JSONL. Ledger rows for
    /// sessions NOT being seeded are preserved untouched. Original
    /// ledger backed up to ledger.jsonl.bak.<unix-ts> before any write.
    Seed {
        /// Agent whose local session JSONLs to seed from. Only `claude-code` is supported today.
        #[arg(value_name = "harness", default_value = "claude-code")]
        harness: String,
        /// Seed only this session id. Mutually exclusive with --since/--until.
        #[arg(long)]
        session: Option<String>,
        /// ISO date (YYYY-MM-DD) or epoch seconds, lower bound (inclusive).
        #[arg(long)]
        since: Option<String>,
        /// ISO date (YYYY-MM-DD) or epoch seconds, upper bound (inclusive).
        #[arg(long)]
        until: Option<String>,
        /// Show what would change without writing.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    // Decide tracing destination based on subcommand. The TUI owns the
    // alternate screen; any tracing writes to stdout will smash through the
    // ratatui frame and corrupt the display. So:
    //   - Tui   → swallow logs (we don't have a file logger plumbed yet).
    //   - Run   → stdout (launchd captures it via plist).
    //   - else  → stdout, info level.
    let no_subcommand = cli.command.is_none();
    let going_to_tui = matches!(cli.command, Some(Cmd::Tui { .. }))
        || (no_subcommand && std::io::IsTerminal::is_terminal(&std::io::stdout()));
    if !going_to_tui {
        init_tracing();
    }

    let cmd = cli.command.unwrap_or_else(|| {
        // No subcommand: open TUI when stdout is a tty (interactive use).
        // When stdout is NOT a tty (CI, scripts, launchd before the plist
        // gets updated), fall back to running the flytrap. The plist passes
        // "run" explicitly so launchd never relies on this branch.
        if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            Cmd::Tui { dev: false }
        } else {
            Cmd::Run
        }
    });

    match cmd {
        Cmd::Tui { dev } => {
            if dev {
                set_dev_ledger_env();
            }
            tui::run()
        }
        Cmd::Run => run_flytrap(Config::load()),
        Cmd::Dev => {
            let mut cfg = Config::load_dev();
            // Force isolated port + ledger if dev.json doesn't override them.
            if cfg.port == Config::default().port {
                cfg.port = 7179;
            }
            // Re-export CCFT_LEDGER for the ledger module to pick up.
            set_dev_ledger_env();
            run_flytrap(cfg)
        }
        Cmd::Install { label } => install::install(label),
        Cmd::Uninstall => install::uninstall(),
        Cmd::Status => {
            lifecycle::print_status(&Config::load());
            Ok(())
        }
        Cmd::Start => lifecycle::start(&Config::load()),
        Cmd::Stop => lifecycle::stop(&Config::load()),
        Cmd::Restart => lifecycle::restart(&Config::load()),
        Cmd::Trust { apply, revoke, ca, dev } => {
            if ca {
                trust::print_ca()
            } else if apply {
                trust::apply_with(dev)
            } else if revoke {
                trust::revoke_with(dev)
            } else {
                trust::print_instructions_with(dev);
                Ok(())
            }
        }
        Cmd::Logs { n } => tail_logs(n),
        Cmd::Brainrot { args, dev } => {
            if dev {
                set_dev_ledger_env();
            }
            brainrot::run(&args)
        }
        Cmd::Perf { range } => perf::run(&range.join(" ")),
        Cmd::Seed { harness, session, since, until, dry_run } => {
            seed::run(seed::Args { harness, session, since, until, dry_run })
        }
    }
}

fn run_flytrap(cfg: Config) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(flytrap::run(cfg))
}

fn set_dev_ledger_env() {
    std::env::set_var(
        "CCFT_LEDGER",
        config::paths::share_dir().join("dev").join("ledger.jsonl"),
    );
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,hudsucker=warn,hyper=warn".into()),
        )
        .init();
}

fn tail_logs(n: usize) -> Result<(), Box<dyn std::error::Error>> {
    // macOS launchd writes stdout/stderr to a file; tail it.
    #[cfg(target_os = "macos")]
    {
        let path = config::paths::launchd_log();
        if !path.exists() {
            return Err(format!("no log file at {}", path.display()).into());
        }
        let raw = std::fs::read_to_string(&path)?;
        let lines: Vec<&str> = raw.lines().collect();
        let start = lines.len().saturating_sub(n);
        for line in &lines[start..] {
            println!("{}", line);
        }
        return Ok(());
    }

    // Linux systemd-user captures to journald. Shell out.
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let label = service::label();
        let status = Command::new("journalctl")
            .args([
                "--user",
                "-u",
                &label,
                "-n",
                &n.to_string(),
                "--no-pager",
            ])
            .status()?;
        if !status.success() {
            return Err(format!("journalctl failed: {}", status).into());
        }
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = n;
        Err("ccft logs not implemented on this platform yet".into())
    }
}

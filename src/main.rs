//! ccft. an agentic self improvement tool.

mod brainrot;
mod config;
mod flytrap;
mod handler;
mod install;
mod ledger;
mod ledger_read;
mod lifecycle;
mod perf;
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
#[command(
    name = "ccft",
    version,
    about = "ccft - an agentic self improvement tool"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the interactive TUI.
    Tui {
        #[arg(long)]
        dev: bool,
    },
    /// Run the flytrap with production config. Invoked by launchd after `ccft install`.
    Run,
    /// Install parallel dev system (com.ccft.dev on port 7179, isolated ledger).
    Dev,
    /// Install: copy binary, generate CA, write service unit, bootstrap.
    Install {
        #[arg(long)]
        label: Option<String>,
    },
    /// Uninstall: bootout, remove unit + binary. Keeps CA + ledger.
    Uninstall,
    /// Show install/load/bind status.
    Status,
    /// Kick the service.
    Start,
    /// Stop the service.
    Stop,
    /// Restart.
    Restart,
    /// Print env vars to route agent through ccft, or apply/revoke.
    Trust {
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        revoke: bool,
        #[arg(long)]
        ca: bool,
        #[arg(long)]
        dev: bool,
    },
    /// Tail service logs.
    Logs {
        #[arg(short, long, default_value_t = 50)]
        n: usize,
    },
    /// Time-series ledger analyzer.
    Brainrot {
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
        #[arg(long)]
        dev: bool,
    },
    /// Perf observability: is ccft slowing requests?
    Perf {
        #[arg(trailing_var_arg = true)]
        range: Vec<String>,
    },
    /// Seed ledger from agent's local session JSONLs. Session is the unit of replacement.
    Seed {
        #[arg(value_name = "harness", default_value = "claude-code")]
        harness: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let no_subcommand = cli.command.is_none();
    let going_to_tui = matches!(cli.command, Some(Cmd::Tui { .. }))
        || (no_subcommand && std::io::IsTerminal::is_terminal(&std::io::stdout()));
    if !going_to_tui {
        init_tracing();
    }

    let cmd = cli.command.unwrap_or_else(|| {
        if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            Cmd::Tui { dev: false }
        } else {
            Cmd::Run
        }
    });

    match cmd {
        Cmd::Tui { dev } => {
            if dev {
                set_dev_env();
            }
            tui::run()
        }
        Cmd::Run => run_flytrap(Config::load()),
        Cmd::Dev => {
            std::env::set_var("CCFT_DEV", "1");
            install::install(None)
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
        Cmd::Trust {
            apply,
            revoke,
            ca,
            dev,
        } => {
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
                set_dev_env();
            }
            brainrot::run(&args)
        }
        Cmd::Perf { range } => perf::run(&range.join(" ")),
        Cmd::Seed {
            harness,
            session,
            since,
            until,
            dry_run,
        } => seed::run(seed::Args {
            harness,
            session,
            since,
            until,
            dry_run,
        }),
    }
}

fn run_flytrap(cfg: Config) -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(flytrap::run(cfg))
}

fn set_dev_env() {
    std::env::set_var("CCFT_DEV", "1");
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = if config::paths::is_dev() {
        EnvFilter::try_new("ccft=debug,hudsucker=warn,hyper=warn")
            .unwrap_or_else(|_| EnvFilter::new("ccft=debug"))
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,hudsucker=warn,hyper=warn".into())
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn tail_logs(n: usize) -> Result<(), Box<dyn std::error::Error>> {
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

    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let label = service::label();
        let status = Command::new("journalctl")
            .args(["--user", "-u", &label, "-n", &n.to_string(), "--no-pager"])
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

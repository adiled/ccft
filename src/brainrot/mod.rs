//! `ccft brainrot` — time-series vibe analyzer for the ledger.
//!
//! Subcommands:
//!   today (default) — daily dashboard
//!   score           — one-line bot/driver score
//!   week            — 7d rollup     (TODO: stub)
//!   replay          — animated      (TODO: stub)
//!   diff A B        — compare       (TODO: stub)
//!   session [sid]   — drill-in      (TODO: stub)
//!   split           — bot/driver turn split  (TODO: stub)

pub mod aggregate;
mod debug_scores;
mod diff;
mod replay;
mod score;
mod session;
mod split;
mod today;
mod week;

pub fn run(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let (sub, rest): (&str, &[String]) = match args.split_first() {
        Some((s, r)) => (s.as_str(), r),
        None => ("today", &[]),
    };
    let spec = rest.join(" ");
    match sub {
        "today" => today::run(&spec),
        "week" => week::run(),
        "score" => score::run(&spec),
        "split" => split::run(rest),
        "session" => session::run(rest),
        "replay" => replay::run(rest),
        "diff" => diff::run(rest),
        "debug-scores" => debug_scores::run(&spec),
        "help" | "--help" | "-h" => {
            println!("{}", USAGE);
            Ok(())
        }
        // Pass unknown subcommands through to today (treat as a range).
        _ => {
            let mut all = vec![sub.to_string()];
            all.extend(rest.iter().cloned());
            today::run(&all.join(" "))
        }
    }
}

const USAGE: &str = "\
ccft brainrot — time-series vibe analyzer

usage: ccft brainrot [subcommand] [args]

  (no args)        today's dashboard
  today [range]    dashboard for a range
  week             7-day rollup
  score [range]    one-line bot/driver score (good for status bars)
  split [range]    driver vs bot turn split (learned gap model; --det = old 5s rule)
  session [sid]    list sessions today, or drill into one
  replay [range]   animated playback  [--speed N]  [--follow]
  diff A B         compare two ranges

ranges:  today, yesterday, 24h, 7d, prev 7d, all, YYYY-MM-DD, Nh, Nd
";

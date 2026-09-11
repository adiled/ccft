//! ccft's brainrot aggregate/scoring, re-exported from the `ccft-brainrot`
//! crate. The math (Record aggregation, EM gap mixture, bot/driver scores,
//! baseline fingerprint, signal) lives in `crates/ccft-brainrot` and is
//! published standalone. This file is a thin re-export so the binary's
//! brainrot subcommands and TUI panels keep working unchanged.

pub use ccft_brainrot::*;

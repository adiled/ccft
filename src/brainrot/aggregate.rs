//! ccft's brainrot aggregate/scoring, re-exported from the `ccft_brainrot`
//! module. The math (Record aggregation, EM gap mixture, bot/driver scores,
//! baseline fingerprint, signal) lives in `src/ccft_brainrot` as part of this
//! single crate. This file is a thin re-export so the binary's brainrot
//! subcommands and TUI panels keep working unchanged.

pub use crate::ccft_brainrot::*;

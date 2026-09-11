//! ccft's ledger read-surface, re-exported from the `ccft-ledger` crate.
//!
//! The generic primitives (Record, TailReader, parse_range/percentile,
//! coverage, top-N/window readers) live in `crates/ccft-ledger` and are
//! published as a standalone crate. What stays here is the ccft-specific
//! glue: computing the live+archive ledger paths from `config::paths`.
//! Everything else is a thin re-export so the rest of the binary is
//! unchanged.

pub use ccft_ledger::{
    compute_coverage, now_secs, parse_range, percentile, read_records_since_from, Coverage, Range,
    Record, StateEvent, TailReader,
};

use ccft_ledger::{
    iter_records as iter_records_crate, load_state_events as load_state_crate,
    load_top_records as load_top_crate, newest_record_ts as newest_crate,
};
use std::path::PathBuf;
use std::time::UNIX_EPOCH;

/// Return the ledger files (archive + live) ccft reads, oldest-first.
pub fn ledger_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let archive = crate::config::paths::ledger()
        .parent()
        .unwrap_or(&crate::config::paths::share_dir())
        .join("archive");
    if archive.is_dir() {
        if let Ok(rd) = std::fs::read_dir(&archive) {
            let mut a: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|d| d.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("ledger_") && n.ends_with(".jsonl"))
                        .unwrap_or(false)
                })
                .collect();
            a.sort();
            files.extend(a);
        }
    }
    let live = crate::config::paths::ledger();
    if live.exists() {
        files.push(live);
    }
    files
}

/// Return the mtime (in seconds since epoch) of all ledger files combined.
/// For caching baseline: only recompute when the latest record's timestamp
/// changes, meaning new data was appended.
pub fn ledger_files_mtime() -> u64 {
    let files = ledger_files();
    files
        .iter()
        .filter_map(|p| {
            p.metadata().ok().map(|m| {
                m.modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or(std::time::Duration::ZERO)
                    .as_secs() as i64
            })
        })
        .max()
        .unwrap_or(0) as u64
}

/// Parse the ccft state-event file (`state.jsonl`) into sorted `StateEvent`s.
pub fn load_state_events() -> Vec<StateEvent> {
    load_state_crate(&crate::config::paths::state())
}

/// Load exactly the top-N (newest-first) records from the ledger.
pub fn load_top_records(n: usize) -> Vec<Record> {
    load_top_crate(&ledger_files(), n)
}

/// Iterate all records in `[since, until]` (chronological) across the ledger.
pub fn iter_records(since: Option<f64>, until: Option<f64>) -> impl Iterator<Item = Record> {
    iter_records_crate(ledger_files(), since, until)
}

/// Return the newest record's timestamp, used to detect new ledger entries.
pub fn newest_record_ts() -> Option<f64> {
    newest_crate(&ledger_files())
}

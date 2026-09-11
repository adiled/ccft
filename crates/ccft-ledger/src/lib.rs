//! `ccft-ledger` — append/tail/range-read a growing JSONL ledger.
//!
//! Split out of the `ccft` binary so the read-side primitives are usable on
//! their own. Everything here is generic: it works on *any* JSONL file whose
//! lines carry a `ts` field, no ccft-specific paths or identity required.
//!
//! ## What's here
//! - [`Record`] — a parsed ledger line (tokens, latency, lexical fingerprint).
//! - [`TailReader`] — incremental tail of a growing file, returns only complete
//!   lines, anchors at a time-window start via binary search.
//! - [`parse_range`]/[`Range`] — human time ranges (`today`, `24h`, `7d`, `Nh`,
//!   `Nd`, `YYYY-MM-DD`, …).
//! - [`percentile`] — p-th percentile of a `u64` slice.
//! - [`compute_coverage`]/[`Coverage`]/[`StateEvent`] — on/off ledger coverage
//!   over a window from a state-event stream.
//! - top-N / full-window / newest-ts readers for one-shot callers.

mod range;
mod tail;

use serde_json::Value;
use std::path::PathBuf;

pub use range::{parse_range, now_secs, percentile, Range};
pub use tail::{TailReader, read_records_since_from};

/// A single parsed ledger record (read side).
///
/// Field names mirror the JSON lines written by ccft's append side, but any
/// JSONL with `ts`/`te`/`in`/`out`/`lat`/`model`/`sid` will parse fine.
#[derive(Debug, Clone, Default)]
pub struct Record {
    pub ts: f64,
    pub te: f64,
    pub model: Option<String>,
    pub sid: Option<String>,
    pub r#in: u64,
    pub out: u64,
    pub tot: u64,
    pub lat: u64,
    pub cr: u64,
    pub cc: u64,
    pub c_us: Option<u64>,
    #[allow(dead_code)] // written to ledger ("ref") but not yet read back by consumers
    pub reference: Option<String>,
    pub u_ch: u64,
    pub tr_ch: u64,
    pub th_ch: u64,
    pub lex_div: f64,
    pub fn_word_frac: f64,
    pub ngram_entropy: f64,
    pub nvt: f64,
}

impl Record {
    pub fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let f = |k: &str| obj.get(k).and_then(Value::as_f64).unwrap_or(0.0);
        let u = |k: &str| obj.get(k).and_then(Value::as_u64).unwrap_or(0);
        let s = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_string);
        let opt_u = |k: &str| obj.get(k).and_then(Value::as_u64);
        Some(Record {
            ts: f("ts"),
            te: f("te"),
            model: s("model"),
            sid: s("sid"),
            reference: s("ref"),
            r#in: u("in"),
            out: u("out"),
            tot: u("tot"),
            lat: u("lat"),
            cr: u("cr"),
            cc: u("cc"),
            c_us: opt_u("c_us"),
            u_ch: u("u_ch"),
            tr_ch: u("tr_ch"),
            th_ch: u("th_ch"),
            lex_div: f("lxd"),
            fn_word_frac: f("fnw"),
            ngram_entropy: f("nge"),
            nvt: f("nvt"),
        })
    }
}

/// Oldest sane epoch: records with `ts` below this are junk (1970-ish).
pub const MIN_TS: f64 = 1_262_304_000.0; // 2010-01-01

// ─── File enumeration / top-N / newest-ts / window readers ───────────────────
//
// These are the one-shot (non-live) helpers. The live TUI path uses
// `TailReader` instead. They take an explicit list of files so callers keep
// control of where the ledger lives (ccft computes live+archive paths itself).

/// Read the last `n` lines from a text file, newest-first, no partial lines.
///
/// Opens a fresh handle each call so there's no stale `BufReader` state; reads
/// backward in chunks until it has at least `n` complete lines.
fn read_last_n_lines(path: &std::path::Path, n: usize) -> Vec<String> {
    if n == 0 {
        return Vec::new();
    }
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let size = match f.metadata().ok() {
        Some(m) => m.len(),
        None => return Vec::new(),
    };
    if size == 0 {
        return Vec::new();
    }
    use std::io::{Read, Seek};
    let mut buf = Vec::new();
    let mut cursor = size;
    let chunk_size = 8192u64;
    loop {
        let read_size = std::cmp::min(chunk_size, cursor);
        cursor -= read_size;
        let mut r = match std::fs::File::open(path) {
            Ok(r) => r,
            Err(_) => break,
        };
        if r.seek(std::io::SeekFrom::Start(cursor)).is_err() {
            break;
        }
        let mut chunk = vec![0u8; read_size as usize];
        if r.read_exact(&mut chunk).is_err() {
            break;
        }
        buf.extend(chunk);
        let text = String::from_utf8_lossy(&buf);
        let count = text.lines().filter(|l| !l.trim().is_empty()).count();
        if count >= n {
            let lines: Vec<String> = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_string)
                .collect();
            let start = lines.len().saturating_sub(n);
            let mut result: Vec<String> = lines[start..].to_vec();
            result.reverse();
            return result;
        }
        if cursor == 0 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    lines.reverse();
    lines
}

/// Load exactly the top-`n` (newest-first) records across `files`, newest
/// file checked first. Skips junk (`ts < MIN_TS`) and unparseable lines.
pub fn load_top_records(files: &[PathBuf], n: usize) -> Vec<Record> {
    if files.is_empty() {
        return Vec::new();
    }
    for file in files.iter().rev() {
        let lines = read_last_n_lines(file, n * 2);
        let mut out = Vec::with_capacity(n);
        for raw in lines {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(r) = Record::from_value(&v) else {
                continue;
            };
            if r.ts < MIN_TS {
                continue;
            }
            out.push(r);
            if out.len() >= n {
                return out;
            }
        }
    }
    Vec::new()
}

/// Iterate all records in `[since, until]` (chronological) across `files`.
///
/// Takes ownership of `files` so the returned iterator is self-contained
/// (`'static`) — the caller doesn't have to keep the file list alive.
pub fn iter_records(
    files: Vec<PathBuf>,
    since: Option<f64>,
    until: Option<f64>,
) -> impl Iterator<Item = Record> {
    files.into_iter().flat_map(move |p| {
        use std::io::BufRead;
        let f = match std::fs::File::open(p) {
            Ok(f) => f,
            Err(_) => return Vec::new().into_iter(),
        };
        let reader = std::io::BufReader::new(f);
        let mut out = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(r) = Record::from_value(&v) else {
                continue;
            };
            if r.ts < MIN_TS {
                continue;
            }
            if let Some(s) = since {
                if r.ts < s {
                    continue;
                }
            }
            if let Some(u) = until {
                if r.ts > u {
                    continue;
                }
            }
            out.push(r);
        }
        out.into_iter()
    })
}

/// Newest record's `ts` across `files` (newest file first).
pub fn newest_record_ts(files: &[PathBuf]) -> Option<f64> {
    use std::io::BufRead;
    for file in files.iter().rev() {
        let f = match std::fs::File::open(file) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = std::io::BufReader::new(f);
        for line in reader
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(ts) = v.as_object().and_then(|o| o.get("ts")).and_then(Value::as_f64) {
                return Some(ts);
            }
        }
    }
    None
}

// ─── State events + coverage ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StateEvent {
    pub ts: f64,
    pub event: String,
}

/// Parse a state-event JSONL file (each line `{"ts","event",...}`).
pub fn load_state_events(path: &std::path::Path) -> Vec<StateEvent> {
    use std::io::BufRead;
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<StateEvent> = std::io::BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| {
            let v: Value = serde_json::from_str(line.trim()).ok()?;
            let ts = v.get("ts")?.as_f64()?;
            let event = v.get("event")?.as_str()?.to_string();
            Some(StateEvent { ts, event })
        })
        .collect();
    out.sort_by(|a, b| a.ts.partial_cmp(&b.ts).unwrap_or(std::cmp::Ordering::Equal));
    out
}

#[derive(Debug, Default)]
pub struct Coverage {
    pub active_s: f64,
    pub total_s: f64,
    pub off_intervals: Vec<(f64, f64)>,
    pub currently_off: bool,
    pub last_event_ts: Option<f64>,
}

/// Compute ledger on/off coverage over `[since, until]` from state events.
///
/// Assumes state events are `ledger_on`/`ledger_off` toggles (anything whose
/// event is not `ledger_off` counts as on). Before `since`, the last event
/// decides the starting state (defaults to on when none).
pub fn compute_coverage(events: &[StateEvent], since: f64, until: f64) -> Coverage {
    let total_s = (until - since).max(0.0);

    let mut on = events
        .iter()
        .take_while(|e| e.ts <= since)
        .last()
        .map(|e| e.event != "ledger_off")
        .unwrap_or(true);

    let mut active = 0.0_f64;
    let mut off_intervals = Vec::new();
    let mut cursor = since;
    let mut last_event_ts: Option<f64> = None;

    for e in events.iter().filter(|e| e.ts > since && e.ts <= until) {
        if on {
            active += e.ts - cursor;
        } else {
            off_intervals.push((cursor, e.ts));
        }
        on = e.event != "ledger_off";
        cursor = e.ts;
        last_event_ts = Some(e.ts);
    }
    if on {
        active += until - cursor;
    } else {
        off_intervals.push((cursor, until));
    }

    let currently_off = !on;
    Coverage {
        active_s: active,
        total_s,
        off_intervals,
        currently_off,
        last_event_ts: last_event_ts.or_else(|| events.last().map(|e| e.ts)),
    }
}

//! Incremental tail reader for a growing JSONL ledger + time-anchored binary
//! search. The crown piece of `ccft-ledger`: ditto-exact copies of a file
//! being appended to concurrently.

use super::Record;
use crate::MIN_TS;
use std::fs;
use std::io::{Read, Seek};

/// Max bytes a single [`TailReader::read_new`] call consumes from the file.
const READ_CHUNK: u64 = 256 * 1024;
/// Max bytes each binary-search probe reads from a seek point.
const SEEK_CHUNK: u64 = 64 * 1024;

/// Incremental tail reader for a growing JSONL ledger.
///
/// Reads bottom-up from the last-read byte offset and only ever returns
/// *complete* lines appended since the previous call. A partial trailing
/// line (writer still mid-write) is held in a pending buffer and only
/// released once a newline closes it. This makes the accumulated output a
/// ditto-exact copy of the file — same records, same order, same count.
#[derive(Default)]
pub struct TailReader {
    pos: u64,
    pending: String,
}

impl TailReader {
    pub fn new() -> Self {
        Self {
            pos: 0,
            pending: String::new(),
        }
    }

    /// Create a reader anchored at a time-window start.
    ///
    /// Binary-searches the file for the byte offset of the first record whose
    /// `ts >= since_ts`, then tail-reads from there — so only records inside
    /// `[since_ts, now]` are ever read, not the whole file.
    pub fn new_anchored(path: &std::path::Path, since_ts: f64) -> Self {
        let mut tr = Self::new();
        match find_offset_for_ts(path, since_ts) {
            Some(off) => tr.pos = off,
            // No record in window — anchor past EOF so nothing is ever read.
            None => {
                if let Ok(f) = fs::File::open(path) {
                    if let Ok(m) = f.metadata() {
                        tr.pos = m.len();
                    }
                }
            }
        }
        tr
    }

    /// Return the complete JSONL lines appended since the last call,
    /// in file (chronological) order. Never returns partial lines.
    ///
    /// Bounded: reads at most `READ_CHUNK` bytes per call, so even a giant
    /// burst append between ticks can't stall the caller for one frame — the
    /// remainder is picked up on the next call.
    pub fn read_new(&mut self, path: &std::path::Path) -> Vec<String> {
        let mut f = match fs::File::open(path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };
        let size = match f.metadata() {
            Ok(m) => m.len(),
            Err(_) => return Vec::new(),
        };
        if size <= self.pos {
            return Vec::new();
        }
        let end = size.min(self.pos.saturating_add(READ_CHUNK));
        if f.seek(std::io::SeekFrom::Start(self.pos)).is_err() {
            return Vec::new();
        }
        let mut bytes = Vec::new();
        if f.take(end - self.pos).read_to_end(&mut bytes).is_err() {
            return Vec::new();
        }
        self.pos += bytes.len() as u64;

        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if !self.pending.is_empty() {
            text = std::mem::take(&mut self.pending) + &text;
        }

        let mut out = Vec::new();
        match text.rfind('\n') {
            Some(idx) => {
                out.extend(
                    text[..=idx]
                        .lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty()),
                );
                self.pending = text[idx + 1..].to_string();
            }
            None => {
                self.pending = text;
            }
        }
        out
    }

    /// Like [`read_new`](Self::read_new), but parse each complete line
    /// straight into a [`Record`], skipping junk/out-of-range lines.
    pub fn read_new_records(&mut self, path: &std::path::Path) -> Vec<Record> {
        let mut out = Vec::new();
        for line in self.read_new(path) {
            let v: serde_json::Value = match serde_json::from_str(&line) {
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
        }
        out
    }
}

/// Drain an anchored [`TailReader`] to completion, returning the complete
/// records in `[since, until]` (chronological). Used by live UIs to (re)build
/// a range aggregate from only the in-window bytes.
pub fn read_records_since_from(
    tr: &mut TailReader,
    path: &std::path::Path,
    since: f64,
    until: f64,
) -> Vec<Record> {
    let mut out = Vec::new();
    loop {
        for r in tr.read_new_records(path) {
            if r.ts >= since && r.ts <= until {
                out.push(r);
            }
        }
        let size = fs::metadata(path).ok().map(|m| m.len()).unwrap_or(0);
        if size <= tr.pos {
            break; // file exhausted
        }
        let before = tr.pos;
        let _ = tr.read_new_records(path);
        if tr.pos == before {
            break; // no progress (pending partial line) — avoid spin
        }
    }
    out
}

/// Byte offset of the first record with `ts >= since_ts`, aligned to a line
/// start (never mid-line). Returns `None` when no record is in window.
///
/// Records are appended chronologically and `ts` is non-decreasing, so this
/// is a monotone predicate → binary search over byte offsets. Every probe
/// reads at most `SEEK_CHUNK` bytes, so this is O(log n) seeks, not a
/// full-file read.
fn find_offset_for_ts(path: &std::path::Path, since_ts: f64) -> Option<u64> {
    let f = fs::File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    if size == 0 {
        return None;
    }

    // Newest record ts — if already older than the window, nothing matches.
    if let Some(ts) = last_line_ts(&f, size) {
        if ts < since_ts {
            return None;
        }
    }

    // Earliest record ts — if already inside the window, offset 0 matches.
    if let Some(ts) = line_ts_at(&f, 0, size) {
        if ts >= since_ts {
            return Some(0);
        }
    }

    // left = a line start with ts < since, right = a line start with ts >= since.
    let mut left = 0u64;
    let mut right = last_line_start(&f, size)?;

    loop {
        // Boundary found when right is the line immediately after left.
        if let Some(nxt) = next_line_start(&f, left, size) {
            if nxt == right {
                return Some(right);
            }
        }

        let mid = (left + right) / 2;
        let start = match next_line_start(&f, mid, size) {
            Some(s) => s,
            None => return Some(right),
        };
        let ts = match line_ts_at(&f, start, size) {
            Some(t) => t,
            None => return Some(right),
        };

        if ts < since_ts {
            // This line predates the window — boundary is after it.
            left = start;
        } else if start < right {
            // In-window line before current right — narrow down.
            right = start;
        } else {
            // Probe hit/passed right with no rightward progress.
            // Only the final few lines remain — scan forward from left.
            return first_in_window(&f, left, size, since_ts);
        }
    }
}

/// First line start after `from` whose ts >= since_ts (linear, small window).
fn first_in_window(f: &fs::File, from: u64, size: u64, since_ts: f64) -> Option<u64> {
    let mut cur = from;
    loop {
        let nxt = next_line_start(f, cur, size)?;
        let ts = line_ts_at(f, nxt, size)?;
        if ts >= since_ts {
            return Some(nxt);
        }
        cur = nxt;
    }
}

/// Read at most `SEEK_CHUNK` bytes from `pos` (or to EOF), returning the text.
fn read_chunk(f: &fs::File, pos: u64, size: u64) -> Option<String> {
    let mut r = std::io::BufReader::new(f.try_clone().ok()?);
    let end = size.min(pos.saturating_add(SEEK_CHUNK));
    r.seek(std::io::SeekFrom::Start(pos)).ok()?;
    let mut buf = Vec::new();
    let mut take = r.take(end - pos);
    take.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Timestamp of the first non-empty line at/after byte `pos` (bounded chunk).
fn line_ts_at(f: &fs::File, pos: u64, size: u64) -> Option<f64> {
    let s = read_chunk(f, pos, size)?;
    let line = s.lines().find(|l| !l.trim().is_empty())?;
    let v = serde_json::from_str::<serde_json::Value>(line.trim()).ok()?;
    v.get("ts")?.as_f64()
}

/// Timestamp of the last non-empty line in the file (bounded tail chunk).
fn last_line_ts(f: &fs::File, size: u64) -> Option<f64> {
    let s = read_chunk(f, size.saturating_sub(SEEK_CHUNK), size)?;
    let line = s.lines().rev().find(|l| !l.trim().is_empty())?;
    let v = serde_json::from_str::<serde_json::Value>(line.trim()).ok()?;
    v.get("ts")?.as_f64()
}

/// Byte offset of the start of the last complete line in the file.
fn last_line_start(f: &fs::File, size: u64) -> Option<u64> {
    let base = size.saturating_sub(SEEK_CHUNK);
    let s = read_chunk(f, base, size)?;
    s.rfind('\n').map(|i| base + i as u64 + 1)
}

/// Byte offset of the first complete line starting strictly after `pos`
/// (skips any partial first line). `None` if only a trailing partial line
/// remains — i.e. no complete line after `pos`.
fn next_line_start(f: &fs::File, pos: u64, size: u64) -> Option<u64> {
    let s = read_chunk(f, pos, size)?;
    s.find('\n').map(|i| pos + i as u64 + 1)
}

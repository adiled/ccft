use crate::config::paths;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::PathBuf;

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

/// Read the last N lines from a text file.
/// Seeks backwards byte-by-byte — safe, no line-boundary corruption.
fn read_last_n_lines(path: &std::path::Path, n: usize) -> Vec<String> {
    let f = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let metadata = match f.metadata() {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let file_size = metadata.len() as u64;
    if file_size == 0 {
        return Vec::new();
    }

    let mut lines: Vec<String> = Vec::with_capacity(n);
    let mut reader = std::io::BufReader::new(f);
    reader.seek(std::io::SeekFrom::End(0)).ok();
    let mut pos = reader.stream_position().unwrap_or(0);

    while pos > 0 && lines.len() < n {
        if pos > 1024 {
            pos -= 1024;
        } else {
            pos = 0;
        }
        reader.seek(std::io::SeekFrom::Start(pos)).ok();
        let mut buf = String::new();
        reader.read_to_string(&mut buf).ok();
        pos += buf.len() as u64;

        for line in buf.lines().rev() {
            lines.push(line.to_string());
            if lines.len() >= n {
                break;
            }
        }
    }

    // Remove trailing empty lines that were part of the file
    while lines.last().map_or(false, |l| l.trim().is_empty()) {
        lines.pop();
    }
    lines
}

/// Load exactly the top-N (newest-first) records from the ledger,
/// without parsing the entire file.
pub fn load_top_records(n: usize) -> Vec<Record> {
    let files = ledger_files();
    if files.is_empty() {
        return Vec::new();
    }

    // Check newest file first (live, then archive reversed)
    for file in files.iter().rev() {
        let lines = read_last_n_lines(file, n * 2);
        let mut records = Vec::with_capacity(n);
        for raw_line in lines {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let r = match Record::from_value(&v) {
                Some(r) => r,
                None => continue,
            };
            if r.ts < 1_262_304_000.0 {
                continue;
            }
            records.push(r);
            if records.len() >= n {
                return records;
            }
        }
    }

    Vec::new()
}

pub fn iter_records(since: Option<f64>, until: Option<f64>) -> impl Iterator<Item = Record> {
    let files = ledger_files();
    files.into_iter().flat_map(move |p| {
        let f = match fs::File::open(&p) {
            Ok(f) => f,
            Err(_) => return Vec::new().into_iter(),
        };
        let reader = BufReader::new(f);
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
            if r.ts < 1_262_304_000.0 {
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

/// Return the newest record's timestamp, used to detect new ledger entries.
pub fn newest_record_ts() -> Option<f64> {
    let files = ledger_files();
    // Check from newest file first (live then archive reversed)
    for file in files.iter().rev() {
        let f = match fs::File::open(file) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let reader = BufReader::new(f);
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
            if let Some(v) = v.as_object() {
                if let Some(ts) = v.get("ts").and_then(Value::as_f64) {
                    return Some(ts);
                }
            }
        }
    }
    None
}

fn ledger_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    let archive = paths::ledger()
        .parent()
        .unwrap_or(&paths::share_dir())
        .join("archive");
    if archive.is_dir() {
        if let Ok(rd) = fs::read_dir(&archive) {
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
    let live = paths::ledger();
    if live.exists() {
        files.push(live);
    }
    files
}

#[derive(Debug, Clone)]
pub struct StateEvent {
    pub ts: f64,
    pub event: String,
}

pub fn load_state_events() -> Vec<StateEvent> {
    let path = paths::state();
    if !path.exists() {
        return Vec::new();
    }
    let f = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<StateEvent> = BufReader::new(f)
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

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct Range {
    pub since: f64,
    pub until: f64,
    pub label: String,
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn today_start() -> f64 {
    let now = now_secs() as i64;
    let dt =
        time::OffsetDateTime::from_unix_timestamp(now).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let local = dt.to_offset(local_offset);
    let midnight = local.replace_time(time::Time::MIDNIGHT);
    midnight.unix_timestamp() as f64
}

/// 24h, prev 7d, all, Nh, Nd, YYYY-MM-DD.
pub fn parse_range(spec: &str) -> Result<Range, String> {
    let now = now_secs();
    let today = today_start();
    let s = spec.trim().to_lowercase();

    if s.is_empty() || s == "today" {
        return Ok(Range {
            since: today,
            until: now,
            label: "today".into(),
        });
    }
    if s == "yesterday" {
        return Ok(Range {
            since: today - 86400.0,
            until: today,
            label: "yesterday".into(),
        });
    }
    if s == "week" || s == "7d" {
        return Ok(Range {
            since: now - 7.0 * 86400.0,
            until: now,
            label: "last 7d".into(),
        });
    }
    if s == "this-week" {
        let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
        let now_dt = time::OffsetDateTime::from_unix_timestamp(now as i64)
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
            .to_offset(local_offset);
        let days_since_monday = (now_dt.weekday().number_from_monday() - 1) as i64;
        let monday_dt = now_dt - time::Duration::days(days_since_monday);
        let monday_midnight = monday_dt.replace_time(time::Time::MIDNIGHT);
        let since = monday_midnight.unix_timestamp() as f64;
        let until = since + 7.0 * 86400.0 - 1.0;
        return Ok(Range {
            since,
            until,
            label: "this week".into(),
        });
    }
    if s == "24h" {
        return Ok(Range {
            since: now - 86400.0,
            until: now,
            label: "last 24h".into(),
        });
    }
    if s == "prev 7d" {
        return Ok(Range {
            since: now - 14.0 * 86400.0,
            until: now - 7.0 * 86400.0,
            label: "prev 7d".into(),
        });
    }
    if s == "all" {
        return Ok(Range {
            since: 0.0,
            until: now,
            label: "all-time".into(),
        });
    }
    if let Some(stripped) = s.strip_suffix('h') {
        if let Ok(n) = stripped.parse::<u64>() {
            return Ok(Range {
                since: now - n as f64 * 3600.0,
                until: now,
                label: format!("last {}h", n),
            });
        }
    }
    if let Some(stripped) = s.strip_suffix('d') {
        if let Ok(n) = stripped.parse::<u64>() {
            return Ok(Range {
                since: now - n as f64 * 86400.0,
                until: now,
                label: format!("last {}d", n),
            });
        }
    }
    if let Some(stripped) = s.strip_suffix('w') {
        if let Ok(n) = stripped.parse::<u64>() {
            return Ok(Range {
                since: now - n as f64 * 7.0 * 86400.0,
                until: now,
                label: format!("last {}w", n),
            });
        }
    }
    if let Some(stripped) = s.strip_suffix("mo") {
        if let Ok(n) = stripped.parse::<u64>() {
            return Ok(Range {
                since: now - n as f64 * 30.0 * 86400.0,
                until: now,
                label: format!("last {}mo", n),
            });
        }
    }
    if s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        let fmt =
            time::format_description::parse("[year]-[month]-[day]").map_err(|e| e.to_string())?;
        let date = time::Date::parse(&s, &fmt).map_err(|e| e.to_string())?;
        let local_offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
        let dt = date
            .with_time(time::Time::MIDNIGHT)
            .assume_offset(local_offset);
        let start = dt.unix_timestamp() as f64;
        return Ok(Range {
            since: start,
            until: start + 86400.0,
            label: s,
        });
    }
    Err(format!(
        "don't understand range '{}'. Try: today, yesterday, 24h, 7d, all, Nh, Nd, YYYY-MM-DD",
        spec
    ))
}

pub fn percentile(values: &mut [u64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let k = (values.len() - 1) as f64 * p / 100.0;
    let f = k.floor() as usize;
    let c = (f + 1).min(values.len() - 1);
    let lo = values[f] as f64;
    let hi = values[c] as f64;
    lo + (hi - lo) * (k - f as f64)
}

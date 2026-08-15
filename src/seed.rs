//! `ccft seed` — replace ledger contents for selected sessions using a
//! coding agent's local session JSONLs. The `<harness>` selects which agent
//! to read; only `claude-code` (from `~/.claude/projects/`) is supported
//! today.
//!
//! Semantics: **session is the unit of replacement.** For each session the
//! user selects (via `--session` or by date range with `--since/--until`):
//!
//!   1. Drop every existing ledger row for that session.
//!   2. Insert one row per user→assistant pair found in the session JSONL,
//!      with all fields the JSONL provides (sid, ts, te, model, in/out/cr/
//!      cc, lat, u_ch, tr_ch). Network-side metadata (cip, pip, sip, c_us)
//!      is left at defaults — that's data only the live proxy can know.
//!
//! All ledger rows for sessions NOT being seeded are preserved untouched.
//! Final ledger is sorted chronologically by ts.
//!
//! Original ledger is always copied to `ledger.jsonl.bak.<unix-ts>` before
//! any write. Honors `--dry-run`.

use crate::ledger::ledger_path;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

pub struct Args {
    pub harness: String,
    pub session: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
struct Turn {
    sid: String,
    /// User message timestamp (request start).
    ts: f64,
    /// Assistant response timestamp (request end). None if no response in JSONL.
    te: Option<f64>,
    u_ch: u64,
    tr_ch: u64,
    model: Option<String>,
    in_tok: u64,
    out_tok: u64,
    cr: u64,
    cc: u64,
}

fn harness_projects_dir(harness: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| -> Box<dyn std::error::Error> { "no HOME env var".into() })?;
    match harness {
        "claude-code" => Ok(home.join(".claude/projects")),
        other => Err(format!("unsupported harness `{other}` (supported: claude-code)").into()),
    }
}

pub fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    if args.session.is_some() && (args.since.is_some() || args.until.is_some()) {
        return Err("--session is mutually exclusive with --since/--until".into());
    }
    if args.session.is_none() && args.since.is_none() && args.until.is_none() {
        return Err("provide --session SID or --since/--until DATE".into());
    }

    let projects_dir = harness_projects_dir(&args.harness)?;
    if !projects_dir.exists() {
        return Err(format!("no {} projects dir at {}", args.harness, projects_dir.display()).into());
    }

    let since = args.since.as_deref().map(parse_when).transpose()?;
    let until = args.until.as_deref().map(parse_when).transpose()?;

    println!("scanning session JSONLs at {}", projects_dir.display());
    let all_turns = collect_all_turns(&projects_dir, args.session.as_deref())?;
    println!("scanned {} turns total", all_turns.len());

    // Pick which sessions to operate on:
    //   --session SID:           that one session
    //   --since/--until:         every session whose START date (earliest
    //                            paired turn ts in that session) falls in
    //                            the range. Per-turn filtering is the
    //                            wrong granularity — a long-running
    //                            session that began before the range but
    //                            had turns in it would otherwise get half-
    //                            replaced. Whole-session is the unit.
    let session_starts: HashMap<String, f64> = {
        let mut m: HashMap<String, f64> = HashMap::new();
        for t in &all_turns {
            if t.te.is_none() {
                continue;
            }
            m.entry(t.sid.clone())
                .and_modify(|cur| {
                    if t.ts < *cur {
                        *cur = t.ts;
                    }
                })
                .or_insert(t.ts);
        }
        m
    };
    let affected: HashSet<String> = if let Some(sid) = args.session.as_deref() {
        std::iter::once(sid.to_string()).collect()
    } else {
        session_starts
            .iter()
            .filter(|(_, &start)| since.map(|s| start >= s).unwrap_or(true))
            .filter(|(_, &start)| until.map(|u| start <= u).unwrap_or(true))
            .map(|(sid, _)| sid.clone())
            .collect()
    };
    println!("affected sessions: {}", affected.len());
    if affected.is_empty() {
        println!("nothing to seed");
        return Ok(());
    }

    // Group all paired turns by session, restricted to affected sessions.
    // Lone user turns (no assistant response) are skipped — they don't
    // correspond to a completed API request.
    let mut new_rows_by_session: HashMap<String, Vec<Turn>> = HashMap::new();
    for t in all_turns.into_iter().filter(|t| t.te.is_some() && affected.contains(&t.sid)) {
        new_rows_by_session.entry(t.sid.clone()).or_default().push(t);
    }
    let new_total: usize = new_rows_by_session.values().map(|v| v.len()).sum();

    // SAFETY: never drop a session that has no replacement turns. A session
    // can have ledger rows but an empty/title-only JSONL (e.g. cancelled
    // sessions, sessions captured by ccft from non-`claude-code` clients).
    // Without this guard, --since covering such a session destroys real
    // data with nothing to replace it. Restrict the drop set to sessions
    // we actually have replacement data for.
    let drop_set: HashSet<String> = new_rows_by_session.keys().cloned().collect();
    let preserved_no_jsonl: HashSet<String> = affected.difference(&drop_set).cloned().collect();

    let lpath = ledger_path();
    let raw_existing = read_raw_lines(&lpath)?;
    let to_drop: usize = raw_existing
        .iter()
        .filter(|line| {
            sid_of_raw(line)
                .map(|s| drop_set.contains(&s))
                .unwrap_or(false)
        })
        .count();

    println!();
    println!("plan:");
    println!("  drop  {} existing ledger rows from {} sessions with replacement data", to_drop, drop_set.len());
    println!("  write {} fresh rows from JSONL", new_total);
    println!("  preserve {} unrelated rows untouched", raw_existing.len() - to_drop);
    if !preserved_no_jsonl.is_empty() {
        println!(
            "  skip   {} affected sessions whose JSONL has no paired turns (rows kept as-is)",
            preserved_no_jsonl.len()
        );
    }

    if args.dry_run {
        println!();
        println!("--dry-run; no changes written");
        return Ok(());
    }

    if to_drop == 0 && new_total == 0 {
        println!();
        println!("nothing to do");
        return Ok(());
    }

    // Backup
    let bak = lpath.with_extension(format!("jsonl.bak.{}", now_unix()));
    fs::copy(&lpath, &bak)?;
    println!();
    println!("backed up original to {}", bak.display());

    // Build new ledger: keep unrelated rows verbatim, plus newly synthesized
    // rows for affected sessions, then sort all by ts.
    let mut out_lines: Vec<String> = Vec::with_capacity(raw_existing.len() - to_drop + new_total);
    for raw in raw_existing {
        let drop = sid_of_raw(&raw)
            .map(|s| drop_set.contains(&s))
            .unwrap_or(false);
        if !drop {
            out_lines.push(raw);
        }
    }
    for (_sid, turns) in new_rows_by_session {
        for t in turns {
            out_lines.push(synthesize_record_json(&t));
        }
    }

    out_lines.sort_by(|a, b| {
        let ta = ts_of_raw(a).unwrap_or(0.0);
        let tb = ts_of_raw(b).unwrap_or(0.0);
        ta.partial_cmp(&tb).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut out = fs::File::create(&lpath)?;
    for line in &out_lines {
        out.write_all(line.as_bytes())?;
        out.write_all(b"\n")?;
    }
    out.flush()?;

    println!();
    println!("✓ wrote {} total rows to {}", out_lines.len(), lpath.display());
    println!("  (backup retained at {})", bak.display());
    Ok(())
}

fn read_raw_lines(p: &Path) -> std::io::Result<Vec<String>> {
    let f = fs::File::open(p)?;
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        let l = line.trim().to_string();
        if !l.is_empty() {
            out.push(l);
        }
    }
    Ok(out)
}

fn sid_of_raw(raw: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw).ok()?;
    v.get("sid").and_then(|s| s.as_str()).map(str::to_string)
}

fn ts_of_raw(raw: &str) -> Option<f64> {
    let v: Value = serde_json::from_str(raw).ok()?;
    v.get("ts").and_then(|t| t.as_f64())
}

/// Walk every JSONL under `dir`, pair each user event with the next
/// assistant event (in chronological order — events are NOT always
/// written in ts order in the JSONL, e.g. when the agent logs the
/// assistant response slightly before the corresponding user message
/// hits disk). Emits one Turn per pair, plus trailing lone user events
/// with te=None which the caller filters out.
fn collect_all_turns(
    dir: &Path,
    only_sid: Option<&str>,
) -> Result<Vec<Turn>, Box<dyn std::error::Error>> {
    let mut all_turns: Vec<Turn> = Vec::new();
    walk_jsonl(dir, &mut |p| {
        let Ok(f) = fs::File::open(p) else { return };
        let reader = BufReader::new(f);

        // First pass: collect every (ts, kind, value) we care about.
        let mut events: Vec<(f64, String, Value)> = Vec::new();
        for line in reader.lines().map_while(Result::ok) {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if kind != "user" && kind != "assistant" {
                continue;
            }
            let sid = v.get("sessionId").and_then(|s| s.as_str()).unwrap_or("");
            if sid.is_empty() {
                continue;
            }
            if let Some(want) = only_sid {
                if sid != want {
                    continue;
                }
            }
            let Some(ts) = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(parse_iso8601)
            else { continue };
            events.push((ts, kind.to_string(), v));
        }

        // Sort by ts so user→assistant pairing is correct even when the
        // file's line order doesn't match wall-clock order.
        events.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        // Second pass: pair user→next assistant within the same session.
        let mut pending_user: Option<Turn> = None;
        for (ts, kind, v) in events {
            let sid = v.get("sessionId").and_then(|s| s.as_str()).unwrap_or("").to_string();
            match kind.as_str() {
                "user" => {
                    if let Some(t) = pending_user.take() {
                        all_turns.push(t);
                    }
                    let content = v.get("message").and_then(|m| m.get("content"));
                    let (u_ch, tr_ch) = count_user_chars(content);
                    pending_user = Some(Turn {
                        sid,
                        ts,
                        te: None,
                        u_ch, tr_ch,
                        model: None,
                        in_tok: 0, out_tok: 0, cr: 0, cc: 0,
                    });
                }
                "assistant" => {
                    let Some(mut t) = pending_user.take() else { continue };
                    if t.sid != sid {
                        // Cross-session interleave — push the user as orphan
                        // and skip this assistant (it has no matching user).
                        all_turns.push(t);
                        continue;
                    }
                    let msg = v.get("message");
                    let usage = msg.and_then(|m| m.get("usage"));
                    t.te = Some(ts);
                    t.model = msg
                        .and_then(|m| m.get("model"))
                        .and_then(|m| m.as_str())
                        .map(str::to_string);
                    t.in_tok = usage.and_then(|u| u.get("input_tokens")).and_then(Value::as_u64).unwrap_or(0);
                    t.out_tok = usage.and_then(|u| u.get("output_tokens")).and_then(Value::as_u64).unwrap_or(0);
                    t.cr = usage.and_then(|u| u.get("cache_read_input_tokens")).and_then(Value::as_u64).unwrap_or(0);
                    t.cc = usage.and_then(|u| u.get("cache_creation_input_tokens")).and_then(Value::as_u64).unwrap_or(0);
                    all_turns.push(t);
                }
                _ => {}
            }
        }
        if let Some(t) = pending_user.take() {
            all_turns.push(t);
        }
    });
    Ok(all_turns)
}

fn walk_jsonl(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk_jsonl(&p, f);
        } else if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            f(&p);
        }
    }
}

/// Mirror of `handler.rs::extract_user_message_lex`. The JSONL stores the
/// same message structure as the API request body; we re-implement here
/// rather than share to keep the live request hot path free of any shared
/// parsing module.
///
/// As of the system-reminder strip fix: `<system-*>...</system-*>` blocks
/// injected by Claude Code's hooks are excluded from u_ch counts so the
/// driver kinetics signal reflects what the user actually typed.
fn count_user_chars(content: Option<&Value>) -> (u64, u64) {
    let Some(c) = content else { return (0, 0) };
    if let Some(s) = c.as_str() {
        return (count_user_text(s), 0);
    }
    let Some(blocks) = c.as_array() else { return (0, 0) };
    let mut text = 0u64;
    let mut tool = 0u64;
    for b in blocks {
        let kind = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match kind {
            "text" => {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    text += count_user_text(t);
                }
            }
            "tool_result" => {
                if let Some(c) = b.get("content") {
                    if let Some(s) = c.as_str() {
                        tool += s.chars().count() as u64;
                    } else if let Some(arr) = c.as_array() {
                        for inner in arr {
                            if let Some(t) = inner.get("text").and_then(|t| t.as_str()) {
                                tool += t.chars().count() as u64;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    (text, tool)
}

fn count_user_text(s: &str) -> u64 {
    if s.trim_start().starts_with("This session is being continued from a previous conversation") {
        return 0;
    }
    strip_system_blocks(s).chars().count() as u64
}

fn strip_system_blocks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(open_at) = rest.find("<system-") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..open_at]);
        let after_open = &rest[open_at + 1..];
        let Some(name_end) = after_open.find('>') else {
            out.push_str(&rest[open_at..]);
            break;
        };
        let tag_name = &after_open[..name_end];
        let close_pat = format!("</{}>", tag_name);
        let after_tag = &after_open[name_end + 1..];
        match after_tag.find(&close_pat) {
            Some(close_at) => {
                rest = &after_tag[close_at + close_pat.len()..];
            }
            None => {
                out.push_str(&rest[open_at..]);
                break;
            }
        }
    }
    out
}

fn synthesize_record_json(t: &Turn) -> String {
    let te = t.te.unwrap_or(t.ts);
    let lat_ms = ((te - t.ts) * 1000.0).max(0.0) as u64;
    let dt = format_local_dt(t.ts);
    json!({
        "ts": t.ts,
        "te": te,
        "dt": dt,
        "human": std::env::var("USER").unwrap_or_else(|_| "unknown".into()),
        "agent": "seed",
        "sid": t.sid,
        "cip": null,
        "pip": null,
        "sip": null,
        "reg": null,
        "model": t.model.as_deref().unwrap_or("unknown"),
        "in": t.in_tok,
        "out": t.out_tok,
        "tot": t.in_tok + t.out_tok,
        "lat": lat_ms,
        "cr": t.cr,
        "cc": t.cc,
        "c_us": 0,
        "u_ch": t.u_ch,
        "tr_ch": t.tr_ch,
        "th_ch": 0,
        "lxd": 0.0,
        "fnw": 0.0,
        "nge": 0.0,
        "nvt": 0.0,
    }).to_string()
}

fn parse_iso8601(s: &str) -> Option<f64> {
    let dt = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    Some(dt.unix_timestamp() as f64 + dt.nanosecond() as f64 / 1e9)
}

fn parse_when(s: &str) -> Result<f64, Box<dyn std::error::Error>> {
    if let Ok(n) = s.parse::<f64>() {
        return Ok(n);
    }
    let fmt = time::format_description::parse("[year]-[month]-[day]")
        .map_err(|e| -> Box<dyn std::error::Error> { format!("bad format desc: {}", e).into() })?;
    let date = time::Date::parse(s, &fmt)
        .map_err(|e| -> Box<dyn std::error::Error> { format!("bad date {s}: {e}").into() })?;
    let local = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let dt = time::PrimitiveDateTime::new(date, time::Time::MIDNIGHT).assume_offset(local);
    Ok(dt.unix_timestamp() as f64)
}

fn format_local_dt(ts: f64) -> String {
    let secs = ts as i64;
    let dt = time::OffsetDateTime::from_unix_timestamp(secs)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let local = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    let dt = dt.to_offset(local);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        dt.year(), u8::from(dt.month()), dt.day(),
        dt.hour(), dt.minute(), dt.second()
    )
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

//! Ledger writer — appends one JSONL record per /v1/messages response.
//! Schema mirrors cc-flytrap/ledger.py with two additions:
//!
//!   { ts, te, dt, human, agent, sid, cip, pip, sip, ep, reg, model,
//!     in, out, tot, lat, cr, cc, c_us, u_ch, tr_ch, lxd, fnw, nge }
//!
//! `u_ch`/`tr_ch` are the char counts of the LAST user message in the
//! request body, split by content type:
//!   * u_ch  — chars when the message is plain text (fresh human input)
//!   * tr_ch — chars when the message is a tool_result (bot continuation)
//! `lxd`/`fnw`/`nge` are lexical stats (type-token ratio, function-word
//! fraction, bigram entropy) of the counted user text — a second,
//! content-free "wordology" axis for detecting bot-driven prompting.
//! `nvt` is the cross-turn momentum axis: how much of the text's content
//! bigrams were already seen earlier in the session (template reuse ⇒ a
//! bot driving the prompt). Computed against an in-memory per-session set;
//! only the fraction is persisted, never the bigrams themselves.
//! Old records lacking these fields default to 0 on read; the driver
//! score function refuses to compute against an empty u_ch baseline.
//!
//! Path: ~/.local/share/ccft/ledger.jsonl, override via $CCFT_LEDGER.
//! State path: ledger.jsonl's parent / state.jsonl.
//!
//! Public IP fetched once and cached for an hour.

use once_cell::sync::Lazy;
use serde_json::json;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::warn;

pub static AGENT_ID: Lazy<String> = Lazy::new(|| {
    let host = gethostname::gethostname().to_string_lossy().to_string();
    // 8 hex chars from current nanos; cheap, unique-enough per process.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let suffix = format!("{:08x}", (nanos as u32));
    format!("{host}-{suffix}")
});

pub static HUMAN: Lazy<String> = Lazy::new(|| {
    std::env::var("USER").unwrap_or_else(|_| "unknown".into())
});

static PUBLIC_IP: Lazy<Mutex<Option<(String, Instant)>>> = Lazy::new(|| Mutex::new(None));

const PUBLIC_IP_TTL: Duration = Duration::from_secs(3600);

/// Best-effort, blocking, ~5s timeout. Caches result for one hour.
pub fn public_ip() -> Option<String> {
    {
        let guard = PUBLIC_IP.lock().ok()?;
        if let Some((ip, t)) = guard.as_ref() {
            if t.elapsed() < PUBLIC_IP_TTL {
                return Some(ip.clone());
            }
        }
    }
    let ip = fetch_public_ip()?;
    let mut guard = PUBLIC_IP.lock().ok()?;
    *guard = Some((ip.clone(), Instant::now()));
    Some(ip)
}

fn fetch_public_ip() -> Option<String> {
    use std::io::Read;
    use std::net::TcpStream;
    let mut sock = TcpStream::connect_timeout(
        &"104.16.93.20:80".parse().ok()?, // api.ipify.org IPv4 (no DNS dep)
        Duration::from_secs(3),
    )
    .ok()?;
    sock.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    sock.write_all(
        b"GET / HTTP/1.0\r\nHost: api.ipify.org\r\nConnection: close\r\n\r\n",
    )
    .ok()?;
    let mut buf = String::new();
    sock.read_to_string(&mut buf).ok()?;
    let body = buf.split("\r\n\r\n").nth(1)?.trim();
    if body.parse::<std::net::IpAddr>().is_ok() {
        Some(body.to_string())
    } else {
        None
    }
}

pub fn ledger_path() -> PathBuf {
    crate::config::paths::ledger()
}

pub fn state_path() -> PathBuf {
    crate::config::paths::state()
}

#[derive(Debug)]
pub struct LedgerRecord<'a> {
    pub timestamp_start: f64,
    pub timestamp_end: f64,
    pub session_id: Option<&'a str>,
    pub client_ip: Option<&'a str>,
    pub server_ip: Option<&'a str>,
    pub region: Option<&'a str>,
    pub model: Option<&'a str>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency_ms: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub ccft_us: u64,
    /// Chars in the LAST user message of the request, when that message is
    /// plain text (fresh human input). 0 when last user message is a
    /// tool_result. See driver-kinetics scoring in brainrot/aggregate.rs.
    pub user_text_chars: u64,
    /// Chars in the LAST user message when it's a tool_result (bot-loop
    /// continuation feedback). Counterpart to user_text_chars.
    pub tool_result_chars: u64,
    /// Type-token ratio of the counted user text (lexical-diversity axis).
    /// 0.0 when absent/too-short — see the wordology classifier in
    /// brainrot/aggregate.rs.
    pub lex_div: f64,
    /// Function-word fraction of the counted user text.
    pub fn_word_frac: f64,
    /// Bigram Shannon entropy of the counted user text (repetition axis).
    pub ngram_entropy: f64,
    /// Cross-turn novelty fraction (0..1) of the counted user text: how much
    /// of its content bigrams were already seen earlier in the session.
    /// High = template reuse ⇒ a bot driving the prompt; low = novel ⇒ human.
    /// 0.0 = no signal (no session / too short / older schema).
    pub novelty: f64,
}

pub fn append(rec: &LedgerRecord) {
    let path = ledger_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    let dt = format_local_dt(rec.timestamp_start);

    let line = json!({
        "ts": rec.timestamp_start,
        "te": rec.timestamp_end,
        "dt": dt,
        "human": HUMAN.as_str(),
        "agent": AGENT_ID.as_str(),
        "sid": rec.session_id,
        "cip": rec.client_ip,
        "pip": public_ip(),
        "sip": rec.server_ip,
        "reg": rec.region,
        "model": rec.model.unwrap_or("unknown"),
        "in": rec.input_tokens,
        "out": rec.output_tokens,
        "tot": rec.input_tokens + rec.output_tokens,
        "lat": rec.latency_ms,
        "cr": rec.cache_read,
        "cc": rec.cache_creation,
        "c_us": rec.ccft_us,
        "u_ch": rec.user_text_chars,
        "tr_ch": rec.tool_result_chars,
        "lxd": rec.lex_div,
        "fnw": rec.fn_word_frac,
        "nge": rec.ngram_entropy,
        "nvt": rec.novelty,
    })
    .to_string();

    if let Err(e) = append_line(&path, &line) {
        warn!("[ccft] ledger write failed: {}", e);
    }
}

pub fn record_state(event: &str, pain: bool) {
    let path = state_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let dt = format_local_dt(now);
    let line = json!({
        "ts": now,
        "dt": dt,
        "event": event,
        "human": HUMAN.as_str(),
        "agent": AGENT_ID.as_str(),
        "pain": pain,
    })
    .to_string();
    if let Err(e) = append_line(&path, &line) {
        warn!("[ccft] state write failed: {}", e);
    }
}

fn append_line(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", line)?;
    Ok(())
}

fn format_local_dt(epoch_secs: f64) -> String {
    // UTC formatting. cc-flytrap.py used local time, but Python's local-time
    // tripped users with timezone-confused dashboards (-6h drift was reported).
    // UTC is unambiguous; downstream tools can re-localize.
    let secs = epoch_secs as i64;
    let dt = time::OffsetDateTime::from_unix_timestamp(secs)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    let fmt = time::format_description::parse(
        "[year]-[month]-[day] [hour]:[minute]:[second]",
    )
    .expect("valid format");
    dt.format(&fmt).unwrap_or_default()
}

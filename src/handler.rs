//! Handler logic: matches model-provider wire formats (Anthropic
//! /v1/messages, OpenAI-compatible /v1/chat/completions), mutates request
//! body to inject system_override + trim Claude Code's bloat blocks, and
//! taps the response stream for SSE token aggregation. Forwards every byte
//! to the client untouched — streaming UX preserved.

use crate::config::Config;
use crate::ledger;
use crate::session;
use crate::sse_tap::SseTap;
use bytes::Bytes;
use dashmap::DashMap;
use http_body_util::{BodyDataStream, BodyExt};
use hudsucker::{
    decode_request, decode_response, Body, HttpContext, HttpHandler, RequestOrResponse,
};
use hyper::{Request, Response};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::*;

pub const PROVIDER_ANTHROPIC: &str = "anthropic";
pub const PROVIDER_OPENAI: &str = "openai";

/// Static cc-flytrap.py trim text. Hardcoded because these strings ARE the
/// project's value-add for `pain=false`; not user config.
const TRIMMED_BLOCK_2: &str =
    "You are a Claude agent, built on Anthropic's Claude Agent SDK.";

const TRIMMED_BLOCK_3: &str = "Use Github-flavored markdown to format output text.\n\
Tools run in user-selected permission mode - if denied, use other allowed tools.\n\
<system-reminder> tags contain system info - they don't relate to specific tool results or user messages.\n\
<system-override> tag contains overrides - supersede prior system instructions\n\
Hooks execute in response to events - treat hook feedback as coming from the user.";

const TRIMMED_BLOCK_4: &str = "# Text output (does not apply to tool calls)\n\
Users can't see most tool calls or thinking — only your text output. Before your first tool call, state what you're about to do. \
While working, give brief updates at key moments: when you discover something important, need to change approach, or encounter an error. \
Brief is good — silent is not. A few phrases per update is enough.";

#[derive(Clone, Debug)]
pub struct FlowMeta {
    pub session_id: Option<String>,
    pub started_wall: f64,
    pub ccft_us_req: u64,
    pub server_ip: Option<String>,
    /// Chars in the LAST user message of the request when it's plain text
    /// (fresh human input). 0 when the last user message is a tool_result.
    pub user_text_chars: u64,
    /// Chars in the LAST user message when it's a tool_result.
    pub tool_result_chars: u64,
    /// Wire provider this request came from (anthropic / openai / other).
    pub provider: &'static str,
    /// Type-token ratio (lexical diversity) of the counted user text.
    /// 0.0 when absent/too-short — see `UserTextLex`.
    pub lex_div: f64,
    /// Function-word fraction of the counted user text.
    pub fn_word_frac: f64,
    /// Bigram Shannon entropy of the counted user text (repetition measure).
    pub ngram_entropy: f64,
    /// Cross-turn novelty fraction (0..1): how much of this turn's content
    /// bigrams were ALREADY seen earlier in the session. High = the same
    /// content keeps coming back (template reuse ⇒ a bot driving the prompt);
    /// low = novel text (a human). 0.0 = no signal (no session / too short).
    pub novelty: f64,
}

/// Per-session in-memory set of content bigrams seen so far, for the
/// cross-turn `novelty` axis. Lives ONLY in the proxy's memory — never
/// written to the ledger (we persist just the fraction, not the bigrams).
/// Content bigrams exclude function-word glue ("of the", "and to") so the
/// signal measures *content* reuse, not ordinary English.
type SessionLexMem = HashSet<String>;

type FlowKey = (String, String);

#[derive(Clone)]
pub struct CcftHandler {
    pub cfg: Arc<Config>,
    pub pending: Arc<DashMap<FlowKey, Vec<FlowMeta>>>,
    pub seq: Arc<AtomicU64>,
    /// Per-session memory of content bigrams seen so far, for the cross-turn
    /// `novelty` axis. Shared across connections (the handler is a single
    /// instance for every connection), keyed by `session_id`. In-memory only;
    /// never persisted — we store just the novelty *fraction*.
    pub session_lex: Arc<DashMap<String, SessionLexMem>>,
}

impl CcftHandler {
    pub fn new(cfg: Arc<Config>) -> Self {
        Self {
            cfg,
            pending: Arc::new(DashMap::new()),
            seq: Arc::new(AtomicU64::new(0)),
            session_lex: Arc::new(DashMap::new()),
        }
    }
}

fn classify_post(req: &Request<Body>) -> Option<&'static str> {
    if req.method() != hyper::Method::POST {
        return None;
    }
    let path = req.uri().path();
    if path.ends_with("/v1/messages") {
        return Some(PROVIDER_ANTHROPIC);
    }
    if path.ends_with("/v1/chat/completions") {
        return Some(PROVIDER_OPENAI);
    }
    None
}

fn flow_key(client: &str, uri: &hyper::Uri) -> FlowKey {
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    (client.to_string(), path.to_string())
}

fn now_wall_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// Lexical statistics of the counted user text, plus the char counts.
/// `chars` = u_ch (fresh-human plain-text chars), `tool_chars` = tr_ch
/// (tool_result continuation chars). `lex_div` (type-token ratio),
/// `fn_word_frac` (function-word fraction) and `ngram_entropy` (bigram
/// Shannon entropy) are the "wordology" axis: cheap, no-model stylometric
/// features of the counted text. `novelty` is the cross-turn momentum axis:
/// how much of this text's content bigrams were already seen earlier in the
/// session (template reuse). All are 0.0 when text is absent or too short to
/// analyze — callers treat 0.0 as "no signal", never as a real value.
#[derive(Default, Debug, Clone, Copy)]
pub struct UserTextLex {
    pub chars: u64,
    pub tool_chars: u64,
    pub lex_div: f64,
    pub fn_word_frac: f64,
    pub ngram_entropy: f64,
    /// Cross-turn novelty fraction (see `FlowMeta.novelty`).
    pub novelty: f64,
}

/// Inspect the LAST user message of an Anthropic /v1/messages request body.
/// When the message is plain text we count the (system-block-stripped)
/// chars AND compute lexical statistics; when it's a tool_result we only
/// count the continuation chars (no lexical signal — it's bot feedback by
/// construction). We use the text/tool_result distinction to drive the
/// kinetics-only "driver" score downstream — gap-based heuristics conflate
/// bot tool-loops with humans pushing hard, but the message-role payload
/// tells us exactly which it is. The lexical axis is a second, content-free
/// signal for "another bot controlling the prompting" — plain text that is
/// machine-flat (low lexical diversity / low n-gram entropy) even when it
/// paces like a human.
///
/// Returns `(UserTextLex::default(), [])` (all zeros) if parsing fails or
/// the schema doesn't match — caller treats the absence as "unknown" rather
/// than zero pressure. `seen` is the session's content-bigram memory; the
/// returned `Vec<String>` is this text's bigrams to merge into it.
fn extract_user_message_lex(
    body_bytes: &[u8],
    seen: Option<&HashSet<String>>,
) -> (UserTextLex, Vec<String>) {
    let none = || (UserTextLex::default(), Vec::new());
    let Ok(data): Result<Value, _> = serde_json::from_slice(body_bytes) else {
        return none();
    };
    let Some(messages) = data.get("messages").and_then(|m| m.as_array()) else {
        return none();
    };
    // Find the last message with role=user.
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"));
    let Some(msg) = last_user else { return none() };
    let Some(content) = msg.get("content") else { return none() };

    // content can be a string (older form) or an array of content blocks.
    let mut parts: Vec<String> = Vec::new();
    let mut tool = 0u64;
    let mut text_block_sizes: Vec<usize> = Vec::new();
    if let Some(s) = content.as_str() {
        let clean = clean_user_text(s);
        if !clean.is_empty() {
            parts.push(clean);
        }
        text_block_sizes.push(s.chars().count());
    } else if let Some(blocks) = content.as_array() {
        for b in blocks {
            let kind = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match kind {
                "text" => {
                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                        let clean = clean_user_text(t);
                        if !clean.is_empty() {
                            parts.push(clean);
                        }
                        text_block_sizes.push(t.chars().count());
                    }
                }
                "tool_result" => {
                    // tool_result.content is itself either a string or an
                    // array of {type:"text", text:"..."} (or image) blocks.
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
        // Debug: when the result is suspiciously large for "user text" (a
        // human can't type 5000 chars/request), dump the block structure so
        // we can see what's being counted. Temporary.
        let text_chars: u64 = parts.iter().map(|s| s.chars().count() as u64).sum();
        if text_chars > 5000 {
            let n_blocks = blocks.len();
            warn!(
                "[ccft][uch-big] text={} (raw_blocks={}, text_block_sizes={:?})",
                text_chars, n_blocks, text_block_sizes
            );
            // Also dump first 500 chars of the last text block we counted
            for b in blocks.iter().rev() {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                        let preview: String = t.chars().take(300).collect();
                        warn!("[ccft][uch-big] last-text-preview: {:?}", preview);
                        break;
                    }
                }
            }
        }
    } else {
        return none();
    }

    let clean = parts.join(" ");
    let (lex_div, fnw, nge) = lexical_stats(&clean);
    let (novelty, to_merge) = novelty_fraction(&clean, seen);
    let chars: u64 = parts.iter().map(|s| s.chars().count() as u64).sum();
    (
        UserTextLex {
            chars,
            tool_chars: tool,
            lex_div,
            fn_word_frac: fnw,
            ngram_entropy: nge,
            novelty,
        },
        to_merge,
    )
}

/// Count chars of a user-text block, EXCLUDING content that didn't come
/// from the user actually typing:
///   * `<system-*>...</system-*>` blocks (Claude Code hooks inject these
///     into the user message — system-reminder, system-override, etc.)
///   * Conversation-continuation summaries — when a session runs out of
///     context, Claude Code starts the next one with a 10-20k-char
///     auto-generated summary in a "user" message. The text is
///     unmistakable: it always opens with "This session is being
///     continued from a previous conversation".
///
/// Both patterns inflate the driver-kinetics signal by hundreds-to-tens-
/// of-thousands of chars per turn that the user never actually typed.
/// The user-authored portion of a message block: strips injected
/// `<system-*>` blocks and auto-generated continuation summaries, returning
/// the text the user (or another bot driving the prompt) actually wrote.
fn clean_user_text(s: &str) -> String {
    if s.trim_start().starts_with("This session is being continued from a previous conversation") {
        return String::new();
    }
    strip_system_blocks(s)
}

/// Closed set of high-frequency function words used for the fn-word axis.
const FUNCTION_WORDS: &[&str] = &[
    "the", "a", "an", "of", "in", "on", "at", "to", "for", "with", "from", "by", "and", "or",
    "but", "not", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "can", "could", "should", "may", "might", "must",
    "i", "you", "he", "she", "it", "we", "they", "me", "my", "your", "his", "her", "our",
    "their", "this", "that", "these", "those", "there", "here", "then", "now", "as", "if",
    "than", "so", "about", "into", "after", "before", "between", "over", "under", "again",
    "once", "off", "up", "down", "out", "very", "just", "more", "most", "less", "least",
    "no", "yes", "what", "which", "when", "where", "how", "who",
];

/// Cap on content bigrams remembered per session. Bounded memory in a long
/// session: on overflow we reset the window, so novelty becomes "recently
/// seen" rather than "ever seen". A heuristic trade-off, not a precision one.
const SESSION_LEX_CAP: usize = 50_000;

/// Cheap, no-model stylometric features of the counted user text:
///   * `lex_div`   — type-token ratio (unique / total tokens). Repetitive,
///     machine-flat text scores lower; varied human text scores higher.
///   * `fnw`       — fraction of tokens that are function words (closed set).
///   * `nge`       — Shannon entropy over consecutive bigrams (bits). Low =
///     heavy repetition (machine-like); high = diverse (human-like).
///
/// All three are 0.0 when the text is too short to analyze (fewer than
/// ~8 tokens), which callers treat as "no lexical signal" — short prompts
/// have inflated TTR and low n-gram entropy regardless of author.
fn lexical_stats(text: &str) -> (f64, f64, f64) {
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let n = tokens.len();
    if n < 8 {
        return (0.0, 0.0, 0.0);
    }
    let mut uniq: HashSet<&str> = HashSet::new();
    uniq.extend(tokens.iter().copied());
    let ttr = uniq.len() as f64 / n as f64;
    let fnw = tokens
        .iter()
        .filter(|t| FUNCTION_WORDS.contains(t))
        .count() as f64
        / n as f64;
    let mut counts: HashMap<(String, String), f64> = HashMap::new();
    for w in tokens.windows(2) {
        *counts.entry((w[0].to_string(), w[1].to_string())).or_default() += 1.0;
    }
    let total: f64 = counts.values().sum();
    let nge = if total > 0.0 {
        counts
            .values()
            .map(|c| {
                let p = c / total;
                -p * p.log2()
            })
            .sum()
    } else {
        0.0
    };
    (ttr, fnw, nge)
}

/// Content bigrams of the (already-cleaned) text: lowercase, alphanumeric,
/// excluding bigrams where BOTH tokens are function words. Excluding the
/// glue means the cross-turn signal measures *content* reuse ("review
/// following", "the file") rather than ordinary English ("of the", "and to").
fn content_bigrams(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let mut out = Vec::new();
    for w in tokens.windows(2) {
        if !FUNCTION_WORDS.contains(&w[0]) || !FUNCTION_WORDS.contains(&w[1]) {
            out.push(format!("{} {}", w[0], w[1]));
        }
    }
    out
}

/// Cross-turn novelty: fraction of this text's content bigrams already in the
/// session's `seen` memory. Returns `(novelty, to_merge)` where `to_merge`
/// are the bigrams NOT already seen (caller adds them to the session memory).
/// `seen = None` (no session / first turn) → novelty 0, merge everything.
/// Empty text → (0.0, []).
fn novelty_fraction(text: &str, seen: Option<&HashSet<String>>) -> (f64, Vec<String>) {
    let bigs = content_bigrams(text);
    if bigs.is_empty() {
        return (0.0, Vec::new());
    }
    let total = bigs.len() as f64;
    let mut seen_count = 0.0f64;
    let mut to_merge = Vec::new();
    match seen {
        Some(s) => {
            for b in &bigs {
                if s.contains(b) {
                    seen_count += 1.0;
                } else {
                    to_merge.push(b.clone());
                }
            }
        }
        None => {
            to_merge.reserve(bigs.len());
            for b in bigs {
                to_merge.push(b);
            }
        }
    }
    (seen_count / total, to_merge)
}

/// Strip every `<system-XXX>...</system-XXX>` block from the input text.
/// Tolerant of nesting/unclosed by simple sequential scan: find an opening
/// tag, find the matching closing tag, drop the whole span. Anything we
/// can't pair just stays in.
fn strip_system_blocks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(open_at) = rest.find("<system-") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..open_at]);
        // Find tag name end (`>` after the opening `<system-`)
        let after_open = &rest[open_at + 1..]; // strip the `<`
        let Some(name_end) = after_open.find('>') else {
            // Malformed; bail and keep the rest as-is.
            out.push_str(&rest[open_at..]);
            break;
        };
        let tag_name = &after_open[..name_end]; // e.g. "system-reminder"
        // Look for matching closing tag.
        let close_pat = format!("</{}>", tag_name);
        let after_tag = &after_open[name_end + 1..];
        match after_tag.find(&close_pat) {
            Some(close_at) => {
                // Skip past the closing tag.
                rest = &after_tag[close_at + close_pat.len()..];
            }
            None => {
                // Unclosed; bail and keep the rest as-is.
                out.push_str(&rest[open_at..]);
                break;
            }
        }
    }
    out
}

/// Mutate Anthropic request body. Returns a new body if mutated, or `None`.
fn mutate_messages_body(body_bytes: &[u8], cfg: &Config) -> Option<Bytes> {
    let mut data: Value = match serde_json::from_slice(body_bytes) {
        Ok(d) => d,
        Err(e) => {
            warn!("[ccft] anthropic body not parseable ({}), passing through", e);
            return None;
        }
    };
    let system = match data.get_mut("system").and_then(|s| s.as_array_mut()) {
        Some(s) => s,
        None => {
            warn!("[ccft] anthropic body has no `system` array, passing through");
            return None;
        }
    };

    let mut notes: Vec<&str> = Vec::new();
    let mut mutated = false;

    if !cfg.system_override.is_empty() {
        system.push(serde_json::json!({
            "type": "text",
            "text": cfg.system_override,
        }));
        notes.push("Override:+1block");
        mutated = true;
    }

    if !cfg.pain_enabled {
        for (idx, replacement) in
            [(1usize, TRIMMED_BLOCK_2), (2, TRIMMED_BLOCK_3), (3, TRIMMED_BLOCK_4)]
        {
            if let Some(block) = system.get_mut(idx).and_then(|b| b.as_object_mut()) {
                if block.contains_key("text") {
                    block.insert("text".into(), Value::String(replacement.into()));
                    match idx {
                        1 => notes.push("Block2"),
                        2 => notes.push("Block3"),
                        3 => notes.push("Block4"),
                        _ => {}
                    }
                    mutated = true;
                }
            }
        }
    }

    if !mutated {
        return None;
    }

    let new_body = match serde_json::to_vec(&data) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "[ccft] failed to re-serialize mutated anthropic body ({}), passing through original",
                e
            );
            return None;
        }
    };
    info!(
        "[ccft] modified: {} (body {} -> {} bytes)",
        notes.join(","),
        body_bytes.len(),
        new_body.len()
    );
    Some(Bytes::from(new_body))
}

fn mutate_openai_body(body_bytes: &[u8], cfg: &Config) -> Option<Bytes> {
    if cfg.system_override.is_empty() {
        return None;
    }
    let mut data: Value = match serde_json::from_slice(body_bytes) {
        Ok(d) => d,
        Err(e) => {
            warn!("[ccft] openai body not parseable ({}), passing through", e);
            return None;
        }
    };
    let messages = match data.get_mut("messages").and_then(|m| m.as_array_mut()) {
        Some(m) => m,
        None => {
            warn!("[ccft] openai body has no `messages` array, passing through");
            return None;
        }
    };

    let mut injected = false;
    for m in messages.iter_mut() {
        if m.get("role").and_then(Value::as_str) == Some("system") {
            if let Some(content) = m.get("content").and_then(Value::as_str) {
                let combined = if content.is_empty() {
                    cfg.system_override.clone()
                } else {
                    format!("{}\n\n{}", content, cfg.system_override)
                };
                m["content"] = Value::String(combined);
                injected = true;
            }
            break;
        }
    }
    if !injected {
        messages.insert(
            0,
            serde_json::json!({ "role": "system", "content": cfg.system_override }),
        );
    }

    let new_body = match serde_json::to_vec(&data) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "[ccft] failed to re-serialize mutated openai body ({}), passing through original",
                e
            );
            return None;
        }
    };
    info!(
        "[ccft] openai modified: +system ({} -> {} bytes)",
        body_bytes.len(),
        new_body.len()
    );
    Some(Bytes::from(new_body))
}

const FLYTRAP_HOSTS: &[&str] = &["api.anthropic.com"];

fn should_flytrap_host(cfg: &Config, host: &str, port: u16) -> bool {
    if FLYTRAP_HOSTS.contains(&host) {
        return true;
    }
    let authority = format!("{}:{}", host, port);
    cfg.openai_targets.iter().any(|t| t == &authority)
}

impl HttpHandler for CcftHandler {
    async fn should_intercept(
        &mut self,
        _ctx: &HttpContext,
        req: &Request<Body>,
    ) -> bool {
        let host = req.uri().host().unwrap_or("");
        let port = req.uri().port_u16().unwrap_or(443);
        should_flytrap_host(&self.cfg, host, port)
    }

    async fn handle_request(
        &mut self,
        ctx: &HttpContext,
        req: Request<Body>,
    ) -> RequestOrResponse {
        let Some(provider) = classify_post(&req) else {
            return req.into();
        };

        let t0 = Instant::now();
        let req = match decode_request(req) {
            Ok(r) => r,
            Err(_) => {
                return Response::builder()
                    .status(500)
                    .body(Body::empty())
                    .unwrap()
                    .into()
            }
        };

        let (parts, body) = req.into_parts();
        let collected = match body.collect().await {
            Ok(c) => c.to_bytes(),
            Err(e) => {
                warn!("[ccft] body collect failed: {}", e);
                return Response::builder()
                    .status(502)
                    .body(Body::empty())
                    .unwrap()
                    .into();
            }
        };

        let session_id = session::extract(&parts.headers, Some(&collected));
        // Wordology (lex stats + cross-turn novelty) is captured only on the
        // Anthropic wire shape we know how to parse. Other providers (OpenAI
        // etc.) yield a default (all-zero) signal until their message schema
        // is mapped — the openai-shape diagnosis is tracked separately.
        let (ux, to_merge) = if provider == PROVIDER_ANTHROPIC {
            match &session_id {
                Some(sid) => match self.session_lex.get(sid) {
                    Some(guard) => extract_user_message_lex(&collected, Some(&*guard)),
                    None => extract_user_message_lex(&collected, None),
                },
                None => extract_user_message_lex(&collected, None),
            }
        } else {
            (UserTextLex::default(), Vec::new())
        };
        if let Some(sid) = &session_id {
            if !to_merge.is_empty() {
                let mut mem = self.session_lex.entry(sid.clone()).or_default();
                for b in to_merge {
                    mem.insert(b);
                }
                if mem.len() > SESSION_LEX_CAP {
                    mem.clear();
                }
            }
        }

        let new_body = match provider {
            PROVIDER_ANTHROPIC => mutate_messages_body(&collected, &self.cfg).unwrap_or(collected),
            PROVIDER_OPENAI => mutate_openai_body(&collected, &self.cfg).unwrap_or(collected),
            _ => collected,
        };

        let _ = self.seq.fetch_add(1, Ordering::Relaxed);
        let key = flow_key(&ctx.client_addr.to_string(), &parts.uri);
        let meta = FlowMeta {
            session_id,
            started_wall: now_wall_secs(),
            ccft_us_req: t0.elapsed().as_micros() as u64,
            server_ip: None,
            user_text_chars: ux.chars,
            tool_result_chars: ux.tool_chars,
            provider,
            lex_div: ux.lex_div,
            fn_word_frac: ux.fn_word_frac,
            ngram_entropy: ux.ngram_entropy,
            novelty: ux.novelty,
        };
        self.pending.entry(key).or_default().push(meta);

        let mut new_req = Request::from_parts(parts, Body::from(new_body.clone()));
        new_req
            .headers_mut()
            .insert(hyper::header::CONTENT_LENGTH, new_body.len().into());
        new_req.headers_mut().remove(hyper::header::CONTENT_ENCODING);

        if self.cfg.highway_enabled {
            if let Some(ua) = new_req.headers_mut().get("user-agent") {
                if let Ok(ua_str) = ua.to_str() {
                    if ua_str.contains("sdk-cli") {
                        let new_ua = ua_str.replace("sdk-cli", "cli");
                        new_req.headers_mut().insert("user-agent", new_ua.parse().unwrap());
                    }
                }
            }
        }

        new_req.into()
    }

    async fn handle_response(
        &mut self,
        ctx: &HttpContext,
        res: Response<Body>,
    ) -> Response<Body> {
        let is_messages = res
            .headers()
            .get(hyper::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false);

        if !is_messages || !self.cfg.ledger_enabled {
            return res;
        }

        let res = match decode_response(res) {
            Ok(r) => r,
            Err(e) => {
                warn!("[ccft] decode_response failed: {}", e);
                return Response::builder().status(502).body(Body::empty()).unwrap();
            }
        };

        let client_key_prefix = ctx.client_addr.to_string();
        let candidate = self
            .pending
            .iter()
            .find(|kv| kv.key().0 == client_key_prefix)
            .map(|kv| kv.key().clone());

        let mut meta: Option<FlowMeta> = None;
        if let Some(k) = candidate {
            if let Some(mut q) = self.pending.get_mut(&k) {
                if !q.is_empty() {
                    meta = Some(q.remove(0));
                }
            }
            self.pending.remove_if(&k, |_, v| v.is_empty());
        }

        let Some(meta) = meta else {
            return res;
        };

        let label = client_key_prefix;
        let (parts, body) = res.into_parts();
        let tapped = SseTap::new(body, label, meta);
        let stream = BodyDataStream::new(tapped);
        Response::from_parts(parts, Body::from_stream(stream))
    }
}

// Convenience for ledger entries.
pub fn record_state_on_startup(cfg: &Config) {
    let event = if cfg.ledger_enabled { "ledger_on" } else { "ledger_off" };
    ledger::record_state(event, cfg.pain_enabled);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Machine-flat text should read as more repetitive (lower n-gram entropy,
    /// lower TTR) than varied human prose — the direction `machine_likeness`
    /// bets on downstream.
    #[test]
    fn lexical_stats_direction() {
        let flat = "run the test run the test run the test check the output run the test again check the log file run the test";
        let varied = "I need to think carefully about how we might approach this problem differently and whether the existing design actually holds up under load";
        let (ttr_flat, fnw_flat, nge_flat) = lexical_stats(flat);
        let (ttr_var, fnw_var, nge_var) = lexical_stats(varied);
        assert!(nge_flat < nge_var, "machine-flat text should repeat (low entropy), got flat={nge_flat} varied={nge_var}");
        assert!(ttr_flat < ttr_var, "machine-flat text should be less lexically diverse, got flat={ttr_flat} varied={ttr_var}");
    }

    /// Too-short text must yield the 0.0 sentinel, never a real value.
    #[test]
    fn lexical_stats_too_short_is_zero() {
        let (ttr, fnw, nge) = lexical_stats("fix the bug now");
        assert_eq!(ttr, 0.0);
        assert_eq!(fnw, 0.0);
        assert_eq!(nge, 0.0);
    }

    /// A bot repeating the same template across turns should read as HIGH
    /// novelty (already-seen), while genuinely novel text should read LOW.
    /// This is the momentum axis the classifier bets on.
    #[test]
    fn novelty_tracks_template_reuse() {
        let template = "please review and report on the following file then verify the build passes before we proceed";
        // First turn: seed the session memory with the template's bigrams.
        let (first_novelty, to_merge) = novelty_fraction(template, None);
        assert_eq!(first_novelty, 0.0, "first turn has nothing to repeat");
        let mut mem: HashSet<String> = HashSet::new();
        for b in to_merge {
            mem.insert(b);
        }
        // Second turn: the SAME template should be mostly already-seen.
        let (second_novelty, _) = novelty_fraction(template, Some(&mem));
        assert!(
            second_novelty > 0.5,
            "repeated template should read as already-seen, got {second_novelty}"
        );
        // A genuinely new message should read as mostly novel.
        let fresh = "we need to reconsider the caching layer because the eviction policy is causing cold starts";
        let (fresh_novelty, _) = novelty_fraction(fresh, Some(&mem));
        assert!(
            fresh_novelty < 0.3,
            "novel text should be mostly new, got {fresh_novelty}"
        );
    }

    /// Content bigrams must exclude function-word glue, so ordinary English
    /// ("of the", "and to") doesn't inflate the novelty signal.
    #[test]
    fn content_bigrams_skip_function_word_glue() {
        let bigs = content_bigrams("of the and to be a in for it");
        assert!(bigs.is_empty(), "glue-only text has no content bigrams");
    }
}

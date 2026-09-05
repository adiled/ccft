use crate::config::Config;
use crate::ledger;
use crate::session;
use crate::sse_tap::tap;
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

const CC_TRIMMED_BLOCK_2: &str = "You are a Claude agent, built on Anthropic's Claude Agent SDK.";

const CC_TRIMMED_BLOCK_3: &str = "Use Github-flavored markdown to format output text.\n\
Tools run in user-selected permission mode - if denied, use other allowed tools.\n\
<system-reminder> tags contain system info - they don't relate to specific tool results or user messages.\n\
<system-override> tag contains overrides - supersede prior system instructions\n\
Hooks execute in response to events - treat hook feedback as coming from the user.";

const CC_TRIMMED_BLOCK_4: &str = "# Text output (does not apply to tool calls)\n\
Users can't see most tool calls or thinking — only your text output. Before your first tool call, state what you're about to do. \
While working, give brief updates at key moments: when you discover something important, need to change approach, or encounter an error. \
Brief is good — silent is not. A few phrases per update is enough.";

#[derive(Clone, Debug)]
pub struct FlowMeta {
    pub session_id: Option<String>,
    pub started_wall: f64,
    pub ccft_us_req: u64,
    pub server_ip: Option<String>,
    pub user_text_chars: u64,
    pub tool_result_chars: u64,
    pub thinking_chars: u64,
    pub provider: &'static str,
    pub reference: Option<String>,
    pub lex_div: f64,
    pub fn_word_frac: f64,
    pub ngram_entropy: f64,
    pub novelty: f64,
}

type SessionLexMem = HashSet<String>;

type FlowKey = (String, String);

#[derive(Clone)]
pub struct CcftHandler {
    pub cfg: Arc<Config>,
    pub pending: Arc<DashMap<FlowKey, Vec<FlowMeta>>>,
    pub seq: Arc<AtomicU64>,
    pub session_lex: Arc<DashMap<String, SessionLexMem>>,
    pub session_fp: Arc<DashMap<String, SessionDelta>>,
}

impl CcftHandler {
    pub fn new(cfg: Arc<Config>) -> Self {
        Self {
            cfg,
            pending: Arc::new(DashMap::new()),
            seq: Arc::new(AtomicU64::new(0)),
            session_lex: Arc::new(DashMap::new()),
            session_fp: Arc::new(DashMap::new()),
         }
     }
 }


fn classify_post(req: &Request<Body>) -> Option<&'static str> {
    if req.method() != hyper::Method::POST {
        return None;
     }
    let path = req.uri().path();
    if path.ends_with("/v1/messages") || path.ends_with("/api/messages") {
        return Some(PROVIDER_ANTHROPIC);
     }
    if path.ends_with("/v1/chat/completions") || path.ends_with("/api/chat") {
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

#[derive(Default, Debug, Clone, Copy)]
pub struct UserTextLex {
    pub chars: u64,
    pub tool_chars: u64,
    pub thinking_chars: u64,
    pub lex_div: f64,
    pub fn_word_frac: f64,
    pub ngram_entropy: f64,
    pub novelty: f64,
}

#[derive(Clone, Default)]
pub(crate) struct SessionDelta {
    last_len: usize,
    last_id: Option<String>,
    seen_text: HashSet<u64>,
}

fn text_fingerprint(msg: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if let Some(c) = msg.get("content") {
        if let Some(s) = c.as_str() {
            s.hash(&mut h);
        } else if let Some(arr) = c.as_array() {
            for b in arr {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    t.hash(&mut h);
                } else if let Some(inner) = b.get("content") {
                    if let Some(s) = inner.as_str() {
                        s.hash(&mut h);
                    } else if let Some(arr2) = inner.as_array() {
                        for x in arr2 {
                            if let Some(t) = x.get("text").and_then(|t| t.as_str()) {
                                t.hash(&mut h);
                            }
                        }
                    }
                }
            }
        }
    }
    h.finish()
}

fn extract_request_delta(
    body_bytes: &[u8],
    seen_lex: Option<&HashSet<String>>,
    prev: Option<&SessionDelta>,
) -> (UserTextLex, Vec<String>, Option<SessionDelta>) {
    let none = || (UserTextLex::default(), Vec::new(), None);
    let Ok(data): Result<Value, _> = serde_json::from_slice(body_bytes) else {
        return none();
    };
    let Some(messages) = data.get("messages").and_then(|m| m.as_array()) else {
        return none();
    };

    let has_ids = messages.iter().any(|m| {
        m.get("id")
            .and_then(|i| i.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    });

    let mut new_flags: Vec<bool> = vec![false; messages.len()];
    match prev {
        None => {
            if let Some(idx) = last_user_idx(&messages) {
                new_flags[idx] = true;
               }
          }
        Some(p) => {
            if messages.len() > p.last_len {
                if let Some(idx) = last_user_idx(&messages) {
                    new_flags[idx] = true;
                   }
              } else if has_ids && p.last_id.is_some() {
                let last_id = p.last_id.as_ref().unwrap();
                if let Some(idx) = messages
                     .iter()
                     .position(|m| m.get("id").and_then(|i| i.as_str()) == Some(last_id.as_str()))
                  {
                     for i in (idx + 1)..messages.len() {
                         new_flags[i] = true;
                       }
                  } else {
                    for (i, m) in messages.iter().enumerate() {
                        if !p.seen_text.contains(&text_fingerprint(m)) {
                            new_flags[i] = true;
                          }
                      }
                  }
              } else {
                for (i, m) in messages.iter().enumerate() {
                    if !p.seen_text.contains(&text_fingerprint(m)) {
                        new_flags[i] = true;
                      }
                  }
              }
        }
    }

    let mut seen_text: HashSet<u64> = match &prev {
        Some(p) => p.seen_text.clone(),
        None => HashSet::new(),
    };
    for m in messages.iter() {
        seen_text.insert(text_fingerprint(m));
    }
    let last_id = messages
        .iter()
        .filter_map(|m| m.get("id").and_then(|i| i.as_str()))
        .max()
        .map(|s| s.to_string());
    let new_state = Some(SessionDelta {
        last_len: messages.len(),
        last_id,
        seen_text,
    });
    debug!(
          "[ccft][delta] msgs={} new_flags={}/{}",
        messages.len(),
        new_flags.iter().filter(|f| **f).count(),
        new_flags.len()
      );

    let mut parts: Vec<String> = Vec::new();
    let mut tool = 0u64;
    let mut thinking = 0u64;
    let mut text_block_sizes: Vec<usize> = Vec::new();

    for (i, msg) in messages.iter().enumerate() {
        if !new_flags[i] {
            continue;
        }

        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

        if role == "tool" {
            if let Some(c) = msg.get("content").and_then(|c| c.as_str()) {
                tool += c.chars().count() as u64;
            } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                for inner in arr {
                    if let Some(t) = inner.get("text").and_then(|t| t.as_str()) {
                        tool += t.chars().count() as u64;
                    }
                }
            }
            continue;
        }

        if role == "assistant" {
            if let Some(rc) = msg.get("reasoning_content").and_then(|r| r.as_str()) {
                thinking += rc.chars().count() as u64;
            }
        }

        let Some(content) = msg.get("content") else {
            continue;
        };
        if let Some(s) = content.as_str() {
            if role == "user" {
                let clean = clean_user_text(s);
                if !clean.is_empty() {
                    parts.push(clean);
                }
                text_block_sizes.push(s.chars().count());
            }
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
                    "thinking" => {
                        if let Some(t) = b.get("thinking").and_then(|t| t.as_str()) {
                            thinking += t.chars().count() as u64;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let text_chars: u64 = parts.iter().map(|s| s.chars().count() as u64).sum();
    if text_chars > 5000 {
        warn!(
            "[ccft][uch-big] delta_text={} (raw_messages={})",
            text_chars,
            messages.len()
        );
    }

    let clean = parts.join(" ");
    let (lex_div, fnw, nge) = lexical_stats(&clean);
    let (novelty, to_merge) = novelty_fraction(&clean, seen_lex);
    let chars: u64 = parts.iter().map(|s| s.chars().count() as u64).sum();
    (
        UserTextLex {
            chars,
            tool_chars: tool,
            thinking_chars: thinking,
            lex_div,
            fn_word_frac: fnw,
            ngram_entropy: nge,
            novelty,
        },
        to_merge,
        new_state,
    )
}

fn last_user_idx(messages: &[Value]) -> Option<usize> {
    messages
        .iter()
        .rposition(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
}

fn clean_user_text(s: &str) -> String {
    if s.trim_start()
        .starts_with("This session is being continued from a previous conversation")
    {
        return String::new();
    }
    strip_system_blocks(s)
}

const FUNCTION_WORDS: &[&str] = &[
    "the", "a", "an", "of", "in", "on", "at", "to", "for", "with", "from", "by", "and", "or",
    "but", "not", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had", "do",
    "does", "did", "will", "would", "can", "could", "should", "may", "might", "must", "i", "you",
    "he", "she", "it", "we", "they", "me", "my", "your", "his", "her", "our", "their", "this",
    "that", "these", "those", "there", "here", "then", "now", "as", "if", "than", "so", "about",
    "into", "after", "before", "between", "over", "under", "again", "once", "off", "up", "down",
    "out", "very", "just", "more", "most", "less", "least", "no", "yes", "what", "which", "when",
    "where", "how", "who",
];

const SESSION_LEX_CAP: usize = 50_000;

const SESSION_FP_CAP: usize = 50_000;

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
    let fnw = tokens.iter().filter(|t| FUNCTION_WORDS.contains(t)).count() as f64 / n as f64;
    let mut counts: HashMap<(String, String), f64> = HashMap::new();
    for w in tokens.windows(2) {
        *counts
            .entry((w[0].to_string(), w[1].to_string()))
            .or_default() += 1.0;
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

fn strip_system_blocks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    loop {
        let Some(open_at) = rest.find("<system-") else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..open_at]);
        let after_open = &rest[open_at + 1..]; // strip the `<`
        let Some(name_end) = after_open.find('>') else {
            out.push_str(&rest[open_at..]);
            break;
        };
        let tag_name = &after_open[..name_end]; // e.g. "system-reminder"
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

fn mutate_messages_body(body_bytes: &[u8], cfg: &Config) -> Option<Bytes> {
    let mut data: Value = match serde_json::from_slice(body_bytes) {
        Ok(d) => d,
        Err(e) => {
            warn!(
                "[ccft] anthropic body not parseable ({}), passing through",
                e
            );
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
        for (idx, replacement) in [
            (1usize, CC_TRIMMED_BLOCK_2),
            (2, CC_TRIMMED_BLOCK_3),
            (3, CC_TRIMMED_BLOCK_4),
        ] {
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

fn extract_previous_response_id(body: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(body).ok()?;
    v.get("previous_response_id")
        .and_then(Value::as_str)
        .map(|s| s.to_string())
}

/// Split a `host[:port]` hosts entry. A bare host implies the protocol's
/// default port (443 — flytrap only touches TLS CONNECT tunnels). Returns the
/// bracket-stripped host and port.
fn flytrap_host_entry(entry: &str) -> (&str, u16) {
    let (h, p) = match entry.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => (entry, "443"),
    };
    let host = h
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(h);
    (host, p.parse().unwrap_or(443))
}

/// Is this CONNECT authority (host[:port]) one we should flytrap?
fn should_flytrap_authority(cfg: &Config, host: &str, port: u16) -> bool {
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    cfg.hosts.iter().any(|entry| {
        let (h, p) = flytrap_host_entry(entry);
        h == host && p == port
    })
}

/// Is this TLS SNI hostname one we should flytrap? (SNI carries no port, so
/// this matches the hostname half of an entry; the CONNECT gate already
/// verified the exact host:port.)
fn should_flytrap_sni(cfg: &Config, host: &str) -> bool {
    cfg.hosts.iter().any(|entry| {
        let (h, _) = flytrap_host_entry(entry);
        h == host
    })
}

impl HttpHandler for CcftHandler {
    fn should_intercept_connect(
        &mut self,
        _ctx: &HttpContext,
        req: &Request<Body>,
    ) -> impl std::future::Future<Output = bool> + Send {
        async move {
            let host = req.uri().host().unwrap_or("");
            let port = req.uri().port_u16().unwrap_or(443);
            should_flytrap_authority(&self.cfg, host, port)
        }
    }

    fn should_intercept_tls(
        &mut self,
        _ctx: &HttpContext,
        client_hello: hudsucker::rustls::server::ClientHello<'_>,
    ) -> impl std::future::Future<Output = bool> + Send {
        async move {
            should_flytrap_sni(&self.cfg, client_hello.server_name().unwrap_or(""))
        }
    }

    fn handle_request(
         &mut self,
         _ctx: &HttpContext,
        req: Request<Body>,
     ) -> impl std::future::Future<Output = RequestOrResponse> + Send {
        async move {
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

            if provider == PROVIDER_OPENAI {
                debug!(
                     "[ccft][openai][raw-req] {} bytes: {}",
                    collected.len(),
                    String::from_utf8_lossy(&collected)
                 );
            }

            let session_id = session::extract(&parts.headers, Some(&collected));
            let (ux, to_merge, new_state) = match &session_id {
                Some(sid) => {
                    let lex = self.session_lex.get(sid);
                    let fp = self.session_fp.get(sid);
                    let d = extract_request_delta(
                         &collected,
                        lex.as_ref().map(|g| &**g),
                        fp.as_ref().map(|g| &**g),
                     );
                     (d.0, d.1, d.2)
                 }
                None => extract_request_delta(&collected, None, None),
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
                if let Some(ns) = new_state {
                    let mut mem = self.session_fp.entry(sid.clone()).or_default();
                     *mem = ns;
                    if mem.seen_text.len() > SESSION_FP_CAP {
                        mem.seen_text.clear();
                     }
                 }
            }

            let reference = if provider == PROVIDER_OPENAI {
                extract_previous_response_id(&collected)
             } else {
                None
             };

            let new_body = match provider {
                PROVIDER_ANTHROPIC => mutate_messages_body(&collected, &self.cfg).unwrap_or(collected),
                PROVIDER_OPENAI => mutate_openai_body(&collected, &self.cfg).unwrap_or(collected),
                 _ => collected,
             };

            let _ = self.seq.fetch_add(1, Ordering::Relaxed);
            let key = flow_key(&format!("{}", _ctx.client_addr), &parts.uri);
            let meta = FlowMeta {
                session_id,
                started_wall: now_wall_secs(),
                ccft_us_req: t0.elapsed().as_micros() as u64,
                server_ip: None,
                user_text_chars: ux.chars,
                tool_result_chars: ux.tool_chars,
                thinking_chars: ux.thinking_chars,
                provider,
                reference,
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
            new_req
                 .headers_mut()
                 .remove(hyper::header::CONTENT_ENCODING);

            if self.cfg.highway_enabled && provider == PROVIDER_ANTHROPIC {
                if let Some(ua) = new_req.headers_mut().get("user-agent") {
                    if let Ok(ua_str) = ua.to_str() {
                        if ua_str.contains("sdk-cli") {
                            let new_ua = ua_str.replace("sdk-cli", "cli");
                            new_req
                                 .headers_mut()
                                 .insert("user-agent", new_ua.parse().unwrap());
                         }
                     }
                 }
            }

            new_req.into()
        }
    }

    fn handle_response(
         &mut self,
         _ctx: &HttpContext,
        res: Response<Body>,
     ) -> impl std::future::Future<Output = Response<Body>> + Send {
        async move {
            if !self.cfg.ledger_enabled {
                return res;
             }

            let ct = res
                 .headers()
                 .get(hyper::header::CONTENT_TYPE)
                 .and_then(|v| v.to_str().ok())
                 .unwrap_or("")
                 .to_string();
            debug!(
                 "[ccft] handle_response: client={} ct={} pending_entries={}",
                 _ctx.client_addr,
                 ct,
                 self.pending.len()
             );

            let res = match decode_response(res) {
                Ok(r) => r,
                Err(e) => {
                    warn!("[ccft] decode_response failed: {}", e);
                    return Response::builder().status(502).body(Body::empty()).unwrap();
                 }
            };

            let client_key_prefix = format!("{}", _ctx.client_addr);
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

            debug!(
                 "[ccft] handle_response: client={} meta_found={} pending_entries={}",
                 client_key_prefix,
                 meta.is_some(),
                 self.pending.len()
             );
            let Some(meta) = meta else {
                return res;
             };

            let label = client_key_prefix;
            let (parts, body) = res.into_parts();

            if ct.contains("text/event-stream") {
                let tapped = tap(body, label, meta);
                let stream = BodyDataStream::new(tapped);
                Response::from_parts(parts, Body::from_stream(stream))
            } else {
                // Non-streaming (`stream:false`) JSON: drain the body eagerly
                // so we can tap it and land the ledger immediately. Its
                // Content-Length body never EOFs the streaming wrapper under
                // keep-alive, so the streaming path would leave the ledger unwritten.
                let bytes = body
                     .collect()
                     .await
                     .map(|c| c.to_bytes())
                     .unwrap_or_default();
                debug!("[ccft] non-stream collected {} bytes", bytes.len());
                let mut tapped = tap(Body::empty(), label, meta);
                tapped.tap_bytes(&bytes);
                Response::from_parts(parts, Body::from(bytes))
            }
        }
    }
}

pub fn record_state_on_startup(cfg: &Config) {
    let event = if cfg.ledger_enabled {
        "ledger_on"
    } else {
        "ledger_off"
    };
    ledger::record_state(event, cfg.pain_enabled);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_stats_direction() {
        let flat = "run the test run the test run the test check the output run the test again check the log file run the test";
        let varied = "I need to think carefully about how we might approach this problem differently and whether the existing design actually holds up under load";
        let (ttr_flat, fnw_flat, nge_flat) = lexical_stats(flat);
        let (ttr_var, fnw_var, nge_var) = lexical_stats(varied);
        assert!(
            nge_flat < nge_var,
            "machine-flat text should repeat (low entropy), got flat={nge_flat} varied={nge_var}"
        );
        assert!(ttr_flat < ttr_var, "machine-flat text should be less lexically diverse, got flat={ttr_flat} varied={ttr_var}");
    }

    #[test]
    fn lexical_stats_too_short_is_zero() {
        let (ttr, fnw, nge) = lexical_stats("fix the bug now");
        assert_eq!(ttr, 0.0);
        assert_eq!(fnw, 0.0);
        assert_eq!(nge, 0.0);
    }

    #[test]
    fn novelty_tracks_template_reuse() {
        let template = "please review and report on the following file then verify the build passes before we proceed";
        let (first_novelty, to_merge) = novelty_fraction(template, None);
        assert_eq!(first_novelty, 0.0, "first turn has nothing to repeat");
        let mut mem: HashSet<String> = HashSet::new();
        for b in to_merge {
            mem.insert(b);
        }
        let (second_novelty, _) = novelty_fraction(template, Some(&mem));
        assert!(
            second_novelty > 0.5,
            "repeated template should read as already-seen, got {second_novelty}"
        );
        let fresh = "we need to reconsider the caching layer because the eviction policy is causing cold starts";
        let (fresh_novelty, _) = novelty_fraction(fresh, Some(&mem));
        assert!(
            fresh_novelty < 0.3,
            "novel text should be mostly new, got {fresh_novelty}"
        );
    }

    #[test]
    fn content_bigrams_skip_function_word_glue() {
        let bigs = content_bigrams("of the and to be a in for it");
        assert!(bigs.is_empty(), "glue-only text has no content bigrams");
    }

    #[test]
    fn flytrap_gating_scoped_to_model_hosts() {
        let cfg = Config::default();
        // api.anthropic.com (bare host → default port 443) is flytrapped
        // from both the CONNECT authority and the TLS SNI.
        assert!(should_flytrap_authority(&cfg, "api.anthropic.com", 443));
        assert!(should_flytrap_sni(&cfg, "api.anthropic.com"));
        // Everything else passes through untouched — the exact regression
        // that broke gh / git / npm / pip TLS verify.
        assert!(!should_flytrap_authority(&cfg, "api.github.com", 443));
        assert!(!should_flytrap_sni(&cfg, "api.github.com"));
        assert!(!should_flytrap_authority(&cfg, "github.com", 443));
        assert!(!should_flytrap_sni(&cfg, "github.com"));
    }

    #[test]
    fn flytrap_gating_honors_custom_hosts() {
        let cfg = Config {
            hosts: vec!["127.0.0.1:8081".into()],
            ..Config::default()
        };
        assert!(should_flytrap_authority(&cfg, "127.0.0.1", 8081));
        assert!(should_flytrap_sni(&cfg, "127.0.0.1"));
        // Wrong port on the authority is not flytrapped.
        assert!(!should_flytrap_authority(&cfg, "127.0.0.1", 8082));
    }

    #[test]
    fn flytrap_entry_default_port_is_443() {
        let (host, port) = flytrap_host_entry("api.anthropic.com");
        assert_eq!(host, "api.anthropic.com");
        assert_eq!(port, 443);
        let (host, port) = flytrap_host_entry("127.0.0.1:11434");
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 11434);
    }

    #[test]
    fn env_host_parses_url_and_hostport() {
        use crate::config::host_from_env;
        assert_eq!(host_from_env("127.0.0.1:11434"), Some("127.0.0.1:11434".into()));
        assert_eq!(host_from_env("http://127.0.0.1:11434"), Some("127.0.0.1:11434".into()));
        assert_eq!(host_from_env("http://127.0.0.1:11434/v1"), Some("127.0.0.1:11434".into()));
        assert_eq!(host_from_env("api.openai.com"), Some("api.openai.com".into()));
        assert_eq!(host_from_env(""), None);
    }
}

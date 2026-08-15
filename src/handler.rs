//! Handler logic: matches Anthropic /v1/messages, mutates request body to
//! inject system_override + trim Claude Code's bloat blocks, and taps the
//! response stream for SSE token aggregation. Forwards every byte to the
//! client untouched — streaming UX preserved.

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
    pub provider: &'static str,
}

type FlowKey = (String, String);

#[derive(Clone)]
pub struct CcftHandler {
    pub cfg: Arc<Config>,
    pub pending: Arc<DashMap<FlowKey, Vec<FlowMeta>>>,
    pub seq: Arc<AtomicU64>,
}

impl CcftHandler {
    pub fn new(cfg: Arc<Config>) -> Self {
        Self {
            cfg,
            pending: Arc::new(DashMap::new()),
            seq: Arc::new(AtomicU64::new(0)),
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

/// Inspect the LAST user message of an Anthropic /v1/messages request body
/// and return (text_chars, tool_result_chars). When the message is plain
/// text the first counter is populated; when it's a tool_result the second
/// is. We use this distinction to drive the kinetics-only "driver" score
/// downstream — gap-based heuristics conflate bot tool-loops with humans
/// pushing hard, but the message-role payload tells us exactly which it is.
///
/// Returns (0, 0) if parsing fails or the schema doesn't match — caller
/// treats the absence as "unknown" rather than zero pressure.
fn extract_user_message_chars(body_bytes: &[u8]) -> (u64, u64) {
    let Ok(data): Result<Value, _> = serde_json::from_slice(body_bytes) else {
        return (0, 0);
    };
    let Some(messages) = data.get("messages").and_then(|m| m.as_array()) else {
        return (0, 0);
    };
    // Find the last message with role=user.
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"));
    let Some(msg) = last_user else { return (0, 0) };
    let Some(content) = msg.get("content") else { return (0, 0) };

    // content can be a string (older form) or an array of content blocks.
    if let Some(s) = content.as_str() {
        return (count_user_text(s), 0);
    }
    let Some(blocks) = content.as_array() else { return (0, 0) };

    let mut text = 0u64;
    let mut tool = 0u64;
    let mut text_block_sizes: Vec<usize> = Vec::new();
    for b in blocks {
        let kind = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match kind {
            "text" => {
                if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                    let counted = count_user_text(t);
                    text += counted;
                    text_block_sizes.push(t.chars().count());
                }
            }
            "tool_result" => {
                // tool_result.content is itself either a string or an array
                // of {type:"text", text:"..."} (or image) blocks.
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
    // Debug: when the result is suspiciously large for "user text" (a human
    // can't type 5000 chars/request), dump the block structure so we can
    // see what's being counted. Temporary.
    if text > 5000 {
        let n_blocks = blocks.len();
        warn!(
            "[ccft][uch-big] text={} (raw_blocks={}, text_block_sizes={:?})",
            text, n_blocks, text_block_sizes
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
    (text, tool)
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
fn count_user_text(s: &str) -> u64 {
    if s.trim_start().starts_with("This session is being continued from a previous conversation") {
        return 0;
    }
    let stripped = strip_system_blocks(s);
    stripped.chars().count() as u64
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
        let (user_text_chars, tool_result_chars) = if provider == PROVIDER_ANTHROPIC {
            extract_user_message_chars(&collected)
        } else {
            (0, 0)
        };

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
            user_text_chars,
            tool_result_chars,
            provider,
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

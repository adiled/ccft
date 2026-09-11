//! `ccft-sse` — a generic hyper [`Body`] tap that turns an SSE (or plain-JSON)
//! response stream into a usage/latency report.
//!
//! Split out of the `ccft` binary. This crate is provider-agnostic: it parses
//! OpenAI and Anthropic stream shapes into a [`TapMeta`]-carried report and
//! hands the finished report to a caller-supplied callback, so the write-side
//! ledger (which is ccft-specific) stays in the binary.
//!
//! ## What's here
//! - [`UsageAggregate`] — token counts gathered from a stream.
//! - [`TapMeta`] — per-request context (provider, reference, timestamps, lex
//!   fingerprint) that the tap fills in and reports back.
//! - [`SseTap`] — a `Body` wrapper that parses `data:` lines on the fly and
//!   drains a non-streaming JSON body to EOF.

use bytes::Bytes;
use hyper::body::{Body, Frame, SizeHint};
use serde_json::Value;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;
use tracing::{debug, info, warn};

/// Provider id used for OpenAI-style `choices`/`prompt_tokens` shapes.
pub const PROVIDER_OPENAI: &str = "openai";

#[derive(Default, Debug, Clone)]
pub struct UsageAggregate {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub model: Option<String>,
}

/// Per-request context the tap needs, and the fields it reports back.
///
/// The binary maps its own (ccft-specific) `FlowMeta` into this struct; the
/// tap only mutates `thinking_chars`/`reference`/`usage` while parsing.
#[derive(Debug, Clone)]
pub struct TapMeta {
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

/// A finished tap: everything the binary needs to write one ledger line.
#[derive(Debug, Clone)]
pub struct TapReport {
    pub started_wall: f64,
    pub end_wall: f64,
    pub session_id: Option<String>,
    pub label: String,
    pub server_ip: Option<String>,
    pub reference: Option<String>,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub latency_ms: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub ccft_us: u64,
    pub user_text_chars: u64,
    pub tool_result_chars: u64,
    pub thinking_chars: u64,
    pub lex_div: f64,
    pub fn_word_frac: f64,
    pub ngram_entropy: f64,
    pub novelty: f64,
}

pub struct SseTap<B> {
    inner: B,
    usage: UsageAggregate,
    started: Instant,
    bytes_seen: usize,
    label: String,
    meta: TapMeta,
    delta_chars: u64,
    line_buf: String,
    ref_id: Option<String>,
    on_report: Box<dyn Fn(TapReport) + Send + Sync>,
}

impl<B> SseTap<B> {
    pub fn new(
        inner: B,
        label: impl Into<String>,
        meta: TapMeta,
        on_report: impl Fn(TapReport) + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            usage: UsageAggregate::default(),
            started: Instant::now(),
            bytes_seen: 0,
            label: label.into(),
            meta,
            delta_chars: 0,
            line_buf: String::new(),
            ref_id: None,
            on_report: Box::new(on_report),
        }
    }

    /// Feed a fully-buffered response body and immediately finalize the
    /// report. Used for non-streaming (`stream:false`) JSON responses, whose
    /// Content-Length bodies don't signal EOF to the streaming `Body` wrapper
    /// under keep-alive — so we drain them eagerly here instead.
    pub fn tap_bytes(&mut self, bytes: &[u8]) {
        debug!("[ccft] tap_bytes: {} bytes", bytes.len());
        // Non-streaming bodies are a single JSON document. Do NOT run them
        // through `ingest` — that treats each `\n` line as an SSE event and
        // drains pretty-printed JSON newlines down to an empty buffer. Parse
        // the whole body directly instead.
        self.line_buf = String::from_utf8_lossy(bytes).into_owned();
        self.parse_complete_body();
        self.report();
    }

    fn ingest(&mut self, chunk: &[u8]) {
        self.bytes_seen += chunk.len();

        let s = String::from_utf8_lossy(chunk);
        self.line_buf.push_str(&s);

        while let Some(idx) = self.line_buf.find('\n') {
            let line = self.line_buf[..idx].trim_end_matches('\r').to_string();
            if let Some(rest) = line.strip_prefix("data: ") {
                self.parse_event(rest);
            }
            // Drain the consumed line (including its `\n`) so the next
            // poll never re-finds the same newline. Without this, a single
            // buffered line is re-parsed forever and EOF never arrives.
            self.line_buf.drain(..=idx);
        }
    }

    /// Parse a whole (non-streaming) JSON response body that arrived without
    /// `data: ` SSE framing — e.g. `stream:false` chat completions. Called on
    /// EOF for any leftover buffered body that wasn't SSE lines.
    fn parse_complete_body(&mut self) {
        debug!(
            "[ccft] parse_complete_body: line_buf={} bytes",
            self.line_buf.len()
        );
        let body = std::mem::take(&mut self.line_buf);
        let body = body.trim();
        if body.is_empty() {
            return;
        }
        let d: Value = match serde_json::from_str(body) {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    "[ccft] non-stream body not parseable ({}), ignoring: {}",
                    self.meta.provider, e
                );
                return;
            }
        };

        debug!("[ccft] parse_complete_body: {} bytes parsed", body.len());
        if self.meta.provider == PROVIDER_OPENAI {
            self.parse_openai_event(&d);
        } else if let Some(msg) = d.get("message") {
            if let Some(id) = msg.get("id").and_then(Value::as_str) {
                self.ref_id = Some(id.to_string());
            }
            if let Some(model) = msg.get("model").and_then(Value::as_str) {
                self.usage.model = Some(model.to_string());
            }
            if let Some(u) = msg.get("usage") {
                self.usage.input_tokens += u_u64(u, "input_tokens");
                self.usage.output_tokens += u_u64(u, "output_tokens");
                self.usage.cache_read_input_tokens += u_u64(u, "cache_read_input_tokens");
                self.usage.cache_creation_input_tokens += u_u64(u, "cache_creation_input_tokens");
            }
        }
    }

    fn parse_event(&mut self, json_str: &str) {
        let d: Value = match serde_json::from_str(json_str) {
            Ok(d) => d,
            Err(e) => {
                // `data: [DONE]` is the OpenAI stream terminator — expected,
                // not a content mismatch.
                if json_str.trim() == "[DONE]" {
                    return;
                }
                warn!(
                    "[ccft] unparseable {} data line ({}), ignoring: {}",
                    self.meta.provider, e, json_str
                );
                return;
            }
        };

        if self.meta.provider == PROVIDER_OPENAI {
            debug!("[ccft][openai][raw-resp] data: {}", json_str);
            self.parse_openai_event(&d);
            return;
        }

        match d.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(msg) = d.get("message") {
                    if let Some(id) = msg.get("id").and_then(Value::as_str) {
                        self.ref_id = Some(id.to_string());
                    }
                    if let Some(model) = msg.get("model").and_then(Value::as_str) {
                        self.usage.model = Some(model.to_string());
                    }
                    // Anthropic thinking blocks stream inside the message's
                    // content array (type: "thinking").
                    if let Some(blocks) = msg.get("content").and_then(Value::as_array) {
                        for b in blocks {
                            if let Some(t) = b.get("thinking").and_then(Value::as_str) {
                                self.meta.thinking_chars += t.chars().count() as u64;
                            }
                        }
                    }
                    if let Some(u) = msg.get("usage") {
                        self.usage.input_tokens += u_u64(u, "input_tokens");
                        self.usage.output_tokens += u_u64(u, "output_tokens");
                        self.usage.cache_read_input_tokens += u_u64(u, "cache_read_input_tokens");
                        self.usage.cache_creation_input_tokens +=
                            u_u64(u, "cache_creation_input_tokens");
                    }
                }
            }
            Some("message_delta") => {
                if let Some(u) = d
                    .get("usage")
                    .or_else(|| d.get("delta").and_then(|x| x.get("usage")))
                {
                    self.usage.input_tokens += u_u64(u, "input_tokens");
                    self.usage.output_tokens += u_u64(u, "output_tokens");
                    self.usage.cache_read_input_tokens += u_u64(u, "cache_read_input_tokens");
                    self.usage.cache_creation_input_tokens +=
                        u_u64(u, "cache_creation_input_tokens");
                }
            }
            _ => {}
        }
    }

    fn parse_openai_event(&mut self, d: &Value) {
        if let Some(id) = d.get("id").and_then(Value::as_str) {
            self.ref_id = Some(id.to_string());
        }
        if let Some(model) = d.get("model").and_then(Value::as_str) {
            self.usage.model = Some(model.to_string());
        }
        if let Some(choices) = d.get("choices").and_then(Value::as_array) {
            for c in choices {
                if let Some(delta) = c.get("delta") {
                    if let Some(content) = delta.get("content").and_then(Value::as_str) {
                        self.delta_chars += content.chars().count() as u64;
                    }

                    if let Some(r) = delta
                        .get("reasoning")
                        .or_else(|| delta.get("reasoning_content"))
                        .and_then(Value::as_str)
                    {
                        self.meta.thinking_chars += r.chars().count() as u64;
                    }
                }
            }
        }
        if let Some(u) = d.get("usage") {
            self.usage.input_tokens = u_u64(u, "prompt_tokens");
            self.usage.output_tokens = u_u64(u, "completion_tokens");
            if let Some(det) = u.get("prompt_tokens_details") {
                self.usage.cache_read_input_tokens += u_u64(det, "cached_tokens");
                self.usage.cache_creation_input_tokens += u_u64(det, "cache_creation_input_tokens");
            }
        }
    }

    fn report(&mut self) {
        let now_wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let latency_ms = self.started.elapsed().as_millis() as u64;

        let (input_tokens, output_tokens) = if self.meta.provider == PROVIDER_OPENAI {
            let out = if self.usage.output_tokens > 0 {
                self.usage.output_tokens
            } else {
                self.delta_chars / 4
            };
            (self.usage.input_tokens, out)
        } else {
            (self.usage.input_tokens, self.usage.output_tokens)
        };

        let reference = self.meta.reference.clone().or_else(|| self.ref_id.clone());

        let cache_creation = if self.usage.cache_creation_input_tokens > 0 {
            self.usage.cache_creation_input_tokens
        } else if self.usage.input_tokens > 0 {
            1 // Ollama doesn't report cache; input_tokens > 0 = 1 completion call
        } else {
            0
        };

        let rep = TapReport {
            started_wall: self.meta.started_wall,
            end_wall: now_wall,
            session_id: self.meta.session_id.clone(),
            label: self.label.clone(),
            server_ip: self.meta.server_ip.clone(),
            reference,
            model: self.usage.model.clone(),
            input_tokens,
            output_tokens,
            latency_ms,
            cache_read: self.usage.cache_read_input_tokens,
            cache_creation,
            ccft_us: self.meta.ccft_us_req,
            user_text_chars: self.meta.user_text_chars,
            tool_result_chars: self.meta.tool_result_chars,
            thinking_chars: self.meta.thinking_chars,
            lex_div: self.meta.lex_div,
            fn_word_frac: self.meta.fn_word_frac,
            ngram_entropy: self.meta.ngram_entropy,
            novelty: self.meta.novelty,
        };

        info!(
            "[ccft] LEDGER sid={} model={} in={} out={} cr={} cc={} lat={}ms",
            rep.session_id.as_deref().unwrap_or("-"),
            rep.model.as_deref().unwrap_or("?"),
            input_tokens,
            output_tokens,
            self.usage.cache_read_input_tokens,
            cache_creation,
            latency_ms,
        );

        (self.on_report)(rep);
    }
}

fn u_u64(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

impl<B> Body for SseTap<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::fmt::Display,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let me = &mut *self;
        match Pin::new(&mut me.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    me.ingest(data);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(None) => {
                debug!("[ccft][tap] EOF reached, parsing leftover body");
                me.parse_complete_body();
                me.report();
                Poll::Ready(None)
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// Minimal empty body used only by tests.
#[cfg(test)]
struct EmptyBody;

#[cfg(test)]
impl Body for EmptyBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(None)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn meta(provider: &'static str) -> TapMeta {
        TapMeta {
            session_id: Some("s1".into()),
            started_wall: 1000.0,
            ccft_us_req: 5,
            server_ip: Some("1.2.3.4".into()),
            user_text_chars: 10,
            tool_result_chars: 0,
            thinking_chars: 0,
            provider,
            reference: None,
            lex_div: 0.5,
            fn_word_frac: 0.2,
            ngram_entropy: 1.5,
            novelty: 0.3,
        }
    }

    fn tap_bytes_into(provider: &'static str, body: &str) -> TapReport {
        use std::sync::{Arc, Mutex};
        let rep: Arc<Mutex<Option<TapReport>>> = Arc::new(Mutex::new(None));
        let rep_cb = Arc::clone(&rep);
        let mut t = SseTap::new(EmptyBody, "label".to_string(), meta(provider), move |r| {
            *rep_cb.lock().unwrap() = Some(r);
        });
        t.tap_bytes(body.as_bytes());
        let result = {
            let mut g = rep.lock().unwrap();
            g.take().expect("report should fire after tap_bytes")
        };
        result
    }

    #[test]
    fn non_stream_anthropic_usage() {
        let body = r#"{"message":{"id":"m1","model":"claude-3","usage":{"input_tokens":11,"output_tokens":22,"cache_read_input_tokens":3,"cache_creation_input_tokens":4}}}"#;
        let rep = tap_bytes_into("anthropic", body);
        assert_eq!(rep.input_tokens, 11);
        assert_eq!(rep.output_tokens, 22);
        assert_eq!(rep.cache_read, 3);
        assert_eq!(rep.cache_creation, 4);
        assert_eq!(rep.model.as_deref(), Some("claude-3"));
        assert_eq!(rep.reference.as_deref(), Some("m1"));
        assert_eq!(rep.label, "label");
        assert_eq!(rep.session_id.as_deref(), Some("s1"));
        // end_wall is now, so latency/end are set
        assert!(rep.end_wall >= rep.started_wall);
    }

    #[test]
    fn non_stream_openai_prompt_completion() {
        let body = r#"{"id":"c1","model":"gpt-4o","choices":[{"delta":{"content":"hello world"}}],"usage":{"prompt_tokens":9,"completion_tokens":18,"prompt_tokens_details":{"cached_tokens":2}}}"#;
        let rep = tap_bytes_into(PROVIDER_OPENAI, body);
        assert_eq!(rep.input_tokens, 9);
        assert_eq!(rep.output_tokens, 18);
        assert_eq!(rep.cache_read, 2);
        assert_eq!(rep.reference.as_deref(), Some("c1"));
    }

    #[test]
    fn openai_reported_tokens_fallback_to_delta_chars() {
        // No `usage` — output falls back to delta_chars/4 (11 chars -> 2).
        let body = r#"{"id":"c2","model":"gpt-4o","choices":[{"delta":{"content":"abcdefghijk"}}]}"#;
        let rep = tap_bytes_into(PROVIDER_OPENAI, body);
        assert_eq!(rep.input_tokens, 0);
        assert_eq!(rep.output_tokens, 2); // 11 / 4
    }
}

//! A generic hyper [`Body`] tap that turns an SSE (or plain-JSON) response
//! stream into a usage/latency report.
//!
//! Provider-agnostic: it parses
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
        // A single whole JSON document (`stream:false`, often pretty-printed)
        // is not ndjson — shape-dispatch it once, not line by line.
        let whole = std::mem::take(&mut self.line_buf).trim().to_string();
        if serde_json::from_str::<Value>(whole.as_str()).is_ok() {
            self.parse_event(whole.as_str());
            self.report();
            return;
        }
        self.line_buf = whole;
        // ndjson (`{…}\n{…}`) or SSE-in-a-body: drain line by line, exactly
        // the way `ingest` would.
        while let Some(idx) = self.line_buf.find('\n') {
            let rest = self.line_buf[..idx]
                .strip_prefix("data: ")
                .unwrap_or(self.line_buf[..idx].trim())
                .trim()
                .to_string();
            if !rest.is_empty() {
                self.parse_event(&rest);
            }
            self.line_buf.drain(..=idx);
        }
        let tail = self.line_buf.trim().to_string();
        if !tail.is_empty() {
            let rest = tail.strip_prefix("data: ").unwrap_or(&tail).trim().to_string();
            if !rest.is_empty() {
                self.parse_event(&rest);
            }
        }
        std::mem::take(&mut self.line_buf);
        self.report();
    }


    /// Dispatch a single raw JSON frame by shape. Handles OpenAI SSE,
    /// Anthropic SSE, and Ollama's ndjson/JSON bodies — all OpenAI-family
    /// servers land under one provider tag, so shape decides.
        fn parse_event(&mut self, json_str: &str) {
        if json_str.trim() == "[DONE]" {
            // OpenAI stream terminator — expected, not an error.
            return;
        }
        let d: Value = match serde_json::from_str(json_str) {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    "[ccft] unparseable data line from {}: {}",
                    self.meta.provider, e
                );
                return;
            }
        };
        if d.as_array().is_some() {
            // Anthropic event frames nest inside a `message.content` array;
            // top-level arrays aren't expected for a chat event, so warn.
            warn!("[ccft] got array event; expected a single JSON doc — {}", json_str);
            return;
        }
        let m = d.get("message").cloned();
        if let Some(m) = m {
            // A message sub-object: if it has an `id` this is an Anthropic
            // event (usage rides under message.usage); otherwise it is an
            // Ollama `/api/chat` frame (content rides under message.content).
            if m.get("id").is_some() {
                self.parse_anthropic_event(&d);
            } else {
                self.parse_ollama_event(&d);
            }
        } else if d.get("response").is_some() || d.get("delta").is_some() || d.get("type").is_some() {
            // OpenAI Responses protocol (`/v1/responses`): streaming events
            // carry `type`/`delta`, final event carries `response.output`.
            self.parse_responses_event(&d);
        } else if self.meta.provider == PROVIDER_OPENAI {
            self.parse_openai_event(&d);
        } else {
            self.parse_anthropic_event(&d);
        }
    }

    /// Anthropic streaming frames (`message_start` / `message_delta`).
    fn parse_anthropic_event(&mut self, d: &Value) {
        if let Some(msg) = d.get("message") {
            if let Some(id) = msg.get("id").and_then(Value::as_str) {
                self.ref_id = Some(id.to_string());
            }
            if let Some(model) = msg.get("model").and_then(Value::as_str) {
                self.usage.model = Some(model.to_string());
            }
            // Anthropic thinking blocks ride inside the content array.
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
            return;
        }
        if let Some(u) = d.get("usage").or_else(|| d.get("delta").and_then(|x| x.get("usage"))) {
            self.usage.input_tokens += u_u64(u, "input_tokens");
            self.usage.output_tokens += u_u64(u, "output_tokens");
            self.usage.cache_read_input_tokens += u_u64(u, "cache_read_input_tokens");
            self.usage.cache_creation_input_tokens += u_u64(u, "cache_creation_input_tokens");
        }
    }

    /// Ollama `/api/chat` frame — each line carries `model`, `message`,
    /// optionally `done` and per-frame usage counters (`prompt_eval_count`,
    /// `eval_count`).  Counters only appear on the final frame, so we
    /// overwrite each field (rather than accumulate) from that frame.
    fn parse_ollama_event(&mut self, d: &Value) {
        if let Some(model) = d.get("model").and_then(Value::as_str) {
            self.usage.model = Some(model.to_string());
        }
        if let Some(msg) = d.get("message") {
            if let Some(content) = msg.get("content").and_then(Value::as_str) {
                self.delta_chars += content.chars().count() as u64;
            }
            for key in ["reasoning", "reasoning_content"] {
                if let Some(r) = msg.get(key).and_then(Value::as_str) {
                    self.meta.thinking_chars += r.chars().count() as u64;
                }
            }
        }
        if let Some(pe) = d.get("prompt_eval_count").and_then(Value::as_u64) {
            self.usage.input_tokens = pe;
        }
        if let Some(e) = d.get("eval_count").and_then(Value::as_u64) {
            self.usage.output_tokens = e;
        }
    }


    /// Responses protocol (OpenAI `/v1/responses`): two shapes.
    /// Streaming: `{"type":"response.output_text.delta","delta":"...","response":{"output_text_deltas":["..."],...}}`
    /// Non-streaming: `{"response":{"output":[{...}],"usage":{...},...},"usage":{...}}`
    fn parse_responses_event(&mut self, d: &Value) {
        // model may ride top-level or under response
        if let Some(model) = d.get("model").and_then(Value::as_str) {
            self.usage.model = Some(model.to_string());
        }
        // Streaming deltas
        if let Some(delta) = d.get("delta").and_then(Value::as_str) {
            self.delta_chars += delta.chars().count() as u64;
        }
        // Non-streaming: response.output is an array of output items
        if let Some(resp) = d.get("response") {
            if let Some(model) = resp.get("model").and_then(Value::as_str) {
                self.usage.model = Some(model.to_string());
            }
            if let Some(out) = resp.get("output").and_then(Value::as_array) {
                for item in out {
                    if let Some(t) = item.get("text").and_then(Value::as_str) {
                        self.delta_chars += t.chars().count() as u64;
                    }
                }
            }
            if let Some(u) = resp.get("usage") {
                self.usage.input_tokens = u_u64(u, "input_tokens");
                self.usage.output_tokens = u_u64(u, "output_tokens");
                if let Some(det) = u.get("input_tokens_details") {
                    self.usage.cache_read_input_tokens += u_u64(det, "cached_tokens");
                    self.usage.cache_creation_input_tokens += u_u64(det, "cached_tokens");
                }
            }
        }
        // Top-level usage fallback (streaming responses emit usage in final event)
        if let Some(u) = d.get("usage") {
            self.usage.input_tokens = u_u64(u, "input_tokens");
            self.usage.output_tokens = u_u64(u, "output_tokens");
        }
    }

    fn ingest(&mut self, chunk: &[u8]) {
        self.bytes_seen += chunk.len();

        let s = String::from_utf8_lossy(chunk);
        self.line_buf.push_str(&s);

        while let Some(idx) = self.line_buf.find('\n') {
            let line = self.line_buf[..idx].trim_end_matches('\r').to_string();
            let rest = line.strip_prefix("data: ").unwrap_or(line.trim()).trim();
            if !rest.is_empty() {
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
                let rest = std::mem::take(&mut me.line_buf).trim().to_string();
                if !rest.is_empty() {
                    let body = rest.strip_prefix("data: ").unwrap_or(&rest).trim();
                    me.parse_event(body);
                }
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

    #[test]
    fn ollama_stream_frames_ndjson() {
        let body = concat!(
            r#"{"model":"phi4-mini:3.8b","message":{"role":"assistant","content":"He"},"done":false,"prompt_eval_count":42,"eval_count":1}"#,
            "\n",
            r#"{"model":"phi4-mini:3.8b","message":{"role":"assistant","content":"llo!"},"done":false,"prompt_eval_count":42,"eval_count":2}"#,
            "\n",
            r#"{"model":"phi4-mini:3.8b","message":{"role":"assistant","content":""},"done":true,"prompt_eval_count":42,"eval_count":3,"total_duration":100000000}"#
        );
        let rep = tap_bytes_into(PROVIDER_OPENAI, body);
        assert_eq!(rep.input_tokens, 42);
        assert_eq!(rep.output_tokens, 3);
        assert_eq!(rep.model.as_deref(), Some("phi4-mini:3.8b"));
    }

    #[test]
    #[test]
    fn responses_stream_deltas_and_usage() {
        let body = concat!(
            r#"data: {"type":"response.output_text.delta","delta":"He"}"#,
            "\n",
            r#"data: {"type":"response.output_text.delta","delta":"llo world"}"#,
            "\n",
            r#"data: {"type":"response.output_text.delta","delta":"","response":{"output_text_deltas":["Hello world"],"usage":{"input_tokens":9,"output_tokens":18}}}"#
        );
        let rep = tap_bytes_into(PROVIDER_OPENAI, body);
        assert_eq!(rep.input_tokens, 9);
        assert_eq!(rep.output_tokens, 18);
    }

    #[test]
    fn responses_nonstream_response_output() {
        let body = r#"{"response":{"model":"gpt-5","output":[{"type":"message","text":"hello from responses"}],"usage":{"input_tokens":11,"output_tokens":22,"input_tokens_details":{"cached_tokens":3}}}}"#;
        let rep = tap_bytes_into(PROVIDER_OPENAI, body);
        assert_eq!(rep.input_tokens, 11);
        assert_eq!(rep.output_tokens, 22);
        assert_eq!(rep.cache_read, 3);
        assert_eq!(rep.model.as_deref(), Some("gpt-5"));
    }

    fn ollama_nonstream_single_frame() {
        let body = r#"{"model":"phi4-mini:3.8b","message":{"role":"assistant","content":"hi there, friend"},"done":true,"prompt_eval_count":7,"eval_count":19}"#;
        let rep = tap_bytes_into(PROVIDER_OPENAI, body);
        assert_eq!(rep.input_tokens, 7);
        assert_eq!(rep.output_tokens, 19);
        assert_eq!(rep.model.as_deref(), Some("phi4-mini:3.8b"));
    }
}

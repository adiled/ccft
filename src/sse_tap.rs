//! Streaming SSE tap. Wraps an inner hyper Body, forwards every frame to the
//! client untouched, and incrementally parses Anthropic /v1/messages SSE events
//! to extract `usage` totals. On stream end, prints a summary line.
//!
//! Critical design property: poll_frame returns the inner frame as-is, with no
//! buffering. The tap is a passive observer on the same task — adds microseconds
//! per chunk for the SSE parse, never holds bytes back from the client.

use crate::ledger;
use bytes::Bytes;
use hyper::body::{Body, Frame, SizeHint};
use serde_json::Value;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Instant;
use tracing::{debug, info, warn};

use crate::handler::FlowMeta;

#[derive(Default, Debug, Clone)]
pub struct UsageAggregate {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub model: Option<String>,
}

pub struct SseTap<B> {
    inner: B,
    /// Carry-over from prior chunks: incomplete final line.
    line_buf: String,
    usage: UsageAggregate,
    started: Instant,
    bytes_seen: usize,
    label: String,
    meta: FlowMeta,
    delta_chars: u64,
    /// OpenAI response `id` (e.g. chatcmpl-…) / Anthropic message `id` — the
    /// handle a later request's `previous_response_id` would reference. Stored
    /// as `ref` in the ledger so turns can be chained WITHOUT persisting
    /// message history.
    ref_id: Option<String>,
}

impl<B> SseTap<B> {
    pub fn new(inner: B, label: impl Into<String>, meta: FlowMeta) -> Self {
        Self {
            inner,
            line_buf: String::new(),
            usage: UsageAggregate::default(),
            started: Instant::now(),
            bytes_seen: 0,
            label: label.into(),
            meta,
            delta_chars: 0,
            ref_id: None,
        }
    }

    /// Append a chunk to the line buffer and parse any newly-complete `data: ...`
    /// SSE lines for usage info.
    fn ingest(&mut self, chunk: &[u8]) {
        self.bytes_seen += chunk.len();

        // Append decoded UTF-8 (lossy on non-UTF8 — tolerant to partial codepoints
        // because Anthropic SSE is ASCII JSON in practice).
        let s = String::from_utf8_lossy(chunk);
        self.line_buf.push_str(&s);

        // Process complete lines (\n-terminated). Leave any trailing partial line in buf.
        while let Some(idx) = self.line_buf.find('\n') {
            let line = self.line_buf[..idx].trim_end_matches('\r').to_string();
            self.line_buf.drain(..=idx); // remove line + \n
            if let Some(rest) = line.strip_prefix("data: ") {
                self.parse_event(rest);
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

        if self.meta.provider == crate::handler::PROVIDER_OPENAI {
            // Debug: dump each RAW SSE data line of the OpenAI response so
            // we can see the actual stream wire shape when diagnosing
            // openai shape handling. Gated behind RUST_LOG=debug.
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
                    if let Some(u) = msg.get("usage") {
                        self.usage.input_tokens += u_u64(u, "input_tokens");
                        self.usage.output_tokens += u_u64(u, "output_tokens");
                        self.usage.cache_read_input_tokens +=
                            u_u64(u, "cache_read_input_tokens");
                        self.usage.cache_creation_input_tokens +=
                            u_u64(u, "cache_creation_input_tokens");
                    }
                }
            }
            Some("message_delta") => {
                if let Some(u) = d.get("usage").or_else(|| d.get("delta").and_then(|x| x.get("usage"))) {
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
                }
            }
        }
        if let Some(u) = d.get("usage") {
            self.usage.input_tokens = u_u64(u, "prompt_tokens");
            self.usage.output_tokens = u_u64(u, "completion_tokens");
            if let Some(det) = u.get("prompt_tokens_details") {
                self.usage.cache_read_input_tokens += u_u64(det, "cached_tokens");
            }
        }
    }

    fn report(&self) {
        let now_wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let latency_ms = self.started.elapsed().as_millis() as u64;

        let (input_tokens, output_tokens) = if self.meta.provider == crate::handler::PROVIDER_OPENAI {
            let out = if self.usage.output_tokens > 0 {
                self.usage.output_tokens
            } else {
                self.delta_chars / 4
            };
            (self.usage.input_tokens, out)
        } else {
            (self.usage.input_tokens, self.usage.output_tokens)
        };

        // Ledger `ref` is a continuation handle, not a message: prefer the
        // request's `previous_response_id` (this turn's parent), else the
        // response `id` (this turn's own handle).
        let reference = self
            .meta
            .reference
            .as_deref()
            .or_else(|| self.ref_id.as_deref());

        let rec = ledger::LedgerRecord {
            timestamp_start: self.meta.started_wall,
            timestamp_end: now_wall,
            session_id: self.meta.session_id.as_deref(),
            client_ip: Some(&self.label),
            server_ip: self.meta.server_ip.as_deref(),
            region: None,
            reference,
            model: self.usage.model.as_deref(),
            input_tokens,
            output_tokens,
            latency_ms,
            cache_read: self.usage.cache_read_input_tokens,
            cache_creation: self.usage.cache_creation_input_tokens,
            ccft_us: self.meta.ccft_us_req,
            user_text_chars: self.meta.user_text_chars,
            tool_result_chars: self.meta.tool_result_chars,
            thinking_chars: self.meta.thinking_chars,
            lex_div: self.meta.lex_div,
            fn_word_frac: self.meta.fn_word_frac,
            ngram_entropy: self.meta.ngram_entropy,
            novelty: self.meta.novelty,
        };

        ledger::append(&rec);

        info!(
            "[ccft] LEDGER sid={} model={} in={} out={} cr={} cc={} lat={}ms",
            self.meta.session_id.as_deref().unwrap_or("-"),
            self.usage.model.as_deref().unwrap_or("?"),
            input_tokens,
            output_tokens,
            self.usage.cache_read_input_tokens,
            self.usage.cache_creation_input_tokens,
            latency_ms,
        );
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

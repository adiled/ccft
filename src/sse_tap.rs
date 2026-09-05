//! ccft-specific adapter over the generic `ccft-sse` tap.
//!
//! The generic crate parses SSE/JSON into a [`TapReport`] with no knowledge
//! of ccft's ledger; this module maps the bin's [`FlowMeta`] in and routes the
//! finished report to the write-side [`ledger::append`].

use crate::handler::FlowMeta;
use crate::ledger;
use ccft_sse::{SseTap, TapMeta, TapReport};
use hyper::body::Body;

/// Build an `SseTap` wired to ccft's ledger write. `label` is the client IP /
/// connection label used as the ledger's `cip`.
pub fn tap<B: Body + Unpin>(
    inner: B,
    label: impl Into<String>,
    meta: FlowMeta,
) -> SseTap<B>
where
    B::Data: From<Vec<u8>> + Send,
    B::Error: std::fmt::Display,
{
    SseTap::new(inner, label, meta.into(), write_report)
}

/// Map ccft's `FlowMeta` into the generic tap's [`TapMeta`].
impl From<FlowMeta> for TapMeta {
    fn from(m: FlowMeta) -> Self {
        TapMeta {
            session_id: m.session_id,
            started_wall: m.started_wall,
            ccft_us_req: m.ccft_us_req,
            server_ip: m.server_ip,
            user_text_chars: m.user_text_chars,
            tool_result_chars: m.tool_result_chars,
            thinking_chars: m.thinking_chars,
            provider: m.provider,
            reference: m.reference,
            lex_div: m.lex_div,
            fn_word_frac: m.fn_word_frac,
            ngram_entropy: m.ngram_entropy,
            novelty: m.novelty,
        }
    }
}

/// Route a finished report into one ccft ledger line.
fn write_report(rep: TapReport) {
    let rec = ledger::LedgerRecord {
        timestamp_start: rep.started_wall,
        timestamp_end: rep.end_wall,
        session_id: rep.session_id.as_deref(),
        client_ip: Some(&rep.label),
        server_ip: rep.server_ip.as_deref(),
        region: None,
        reference: rep.reference.as_deref(),
        model: rep.model.as_deref(),
        input_tokens: rep.input_tokens,
        output_tokens: rep.output_tokens,
        latency_ms: rep.latency_ms,
        cache_read: rep.cache_read,
        cache_creation: rep.cache_creation,
        ccft_us: rep.ccft_us,
        user_text_chars: rep.user_text_chars,
        tool_result_chars: rep.tool_result_chars,
        thinking_chars: rep.thinking_chars,
        lex_div: rep.lex_div,
        fn_word_frac: rep.fn_word_frac,
        ngram_entropy: rep.ngram_entropy,
        novelty: rep.novelty,
    };
    ledger::append(&rec);
}

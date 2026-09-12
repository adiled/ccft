//! `ccft` — an agentic self-improvement tool.
//!
//! This crate is both a **binary** (`src/main.rs`) and a **library**.
//! The library is a facade over ccft's internal reusable modules: the generic,
//! self-contained logic lives in `ccft_ledger`, `ccft_session`, `ccft_lex`,
//! `ccft_brainrot`, and `ccft_sse` (single-crate modules, not separate
//! published crates), and is re-exported here so downstream Rust code can
//! `use ccft::ledger::…` / `ccft::sse::…`.
//!
pub mod ccft_brainrot;
pub mod ccft_ledger;
pub mod ccft_lex;
pub mod ccft_session;
pub mod ccft_sse;

// The binary's own modules (handler, config, lifecycle, …) are
// ccft-specific and intentionally *not* exposed as a library surface.

/// Ledger read-side primitives (`Record`, `TailReader`, `Range`, …).
pub use crate::ccft_ledger as ledger;
/// Session-id extraction from headers + metadata bodies.
pub use crate::ccft_session as session;
/// Pure text fingerprinting: lexical stats, bigrams, novelty.
pub use crate::ccft_lex as lex;
/// Agentic turn classification + baseline fingerprinting.
pub use crate::ccft_brainrot as brainrot;
/// Generic hyper `Body` tap for OpenAI/Anthropic SSE + non-stream JSON.
pub use crate::ccft_sse as sse;

#[cfg(test)]
mod tests {
    // Prove the facade: downstream code can reach the modules through `ccft::`.
    #[test]
    fn facade_re_exports_modules() {
        // ledger: parse_range
        let r = super::ledger::parse_range("24h");
        assert!(r.is_ok(), "ledger::parse_range should parse '24h'");

        // sse: provider constant + report type
        assert_eq!(super::sse::PROVIDER_OPENAI, "openai");

        // lex: pure text fingerprint
        let (ttr, _, _) = super::lex::lexical_stats("the quick brown fox jumps over the lazy dog");
        assert!(ttr > 0.0);

        // brainrot: Aggregate folds records
        use super::ledger::Record;
        use super::brainrot::Aggregate;
        let a = Aggregate::ingest(vec![Record::default()]);
        assert_eq!(a.n, 1);

        // session: no body → None
        use hyper::HeaderMap;
        assert_eq!(super::session::extract(&HeaderMap::new(), None), None);
    }
}

# ccft-ledger

Append/tail/range-read a growing JSONL ledger: `TailReader`, `parse_range`,
percentile, coverage.

Part of the [`ccft`](https://github.com/adiled/ccft) workspace — an agentic
self-improvement proxy. This crate holds the generic, reusable ledger
read-side primitives used by the `ccft` binary's TUI and brainrot analysis.

```rust
let r = ccft_ledger::parse_range("24h");
assert!(r.is_ok());
```

MIT license — see [LICENSE](https://github.com/adiled/ccft/blob/main/LICENSE).

# ccft-brainrot

Agentic turn classification + baseline fingerprinting: EM gap mixture,
bot/driver scores.

Part of the [`ccft`](https://github.com/adiled/ccft) workspace. Feeds the
brainrot chart in the `ccft` TUI.

```rust
use ccft_brainrot::Aggregate;
use ccft_ledger::Record;
let a = Aggregate::ingest(vec![Record::default()]);
assert_eq!(a.n, 1);
```

MIT license.

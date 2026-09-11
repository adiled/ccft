# ccft-session

Extract agent session ids from hyper headers + JSON metadata bodies (Claude
Code, OpenAI, SDKs).

Part of the [`ccft`](https://github.com/adiled/ccft) workspace. Generic
session-id extraction used by the proxy's flow metadata.

```rust
use hyper::HeaderMap;
assert_eq!(ccft_session::extract(&HeaderMap::new(), None), None);
```

MIT license.

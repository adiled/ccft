# Architecture

## How it works

```
agent  ──HTTPS_PROXY=http://127.0.0.1:7178──>  ccft  ──HTTPS h1.1──>  model API
                                                  │
                                                  ├─ on_request:  decode → mutate `system` array → re-encode → forward
                                                  │
                                                  └─ on_response: wrap Body with SseTap →
                                                                  every chunk forwarded to client + parsed for SSE usage events →
                                                                  on stream end, append ledger.jsonl line
```

Built on [`hudsucker`](https://github.com/omjadas/hudsucker), a hyper-1.x + tokio + rustls flytrap library. h1.1 is forced (Anthropic accepts it cleanly via ALPN, which sidesteps the open h2 issues across the Go/Rust flytrap ecosystem).

The flytrap is **scoped to the `hosts` list in config** via hudsucker's `should_intercept_connect` / `should_intercept_tls` (see `src/handler.rs`). Defaults cover Anthropic + OpenAI first-party hosts (`api.anthropic.com`, `api.openai.com`) and local OpenAI-compatible servers with dedicated ports (Ollama `:11434`, LM Studio `:1234`, Jan `:1337`, GPT4All `:4891`). The list also auto-unions hosts from env vars like `OLLAMA_HOST` / `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL`. Every other CONNECT (e.g., `github.com`, `npm registry`, `pypi`) gets a raw passthrough tunnel — ccft never decrypts those bytes, so subprocesses spawned from an agent session that don't trust ccft's CA don't fail TLS on them.

## TUI

`ccft` with no subcommand at a tty opens the full-screen interactive dashboard. The brainrot chart is the frame; every other panel is a knob or an overlay.

**Keyboard:**

| Key | What |
|---|---|
| `←` / `→` (or `[` / `]`) | step through range presets |
| `t` `y` `h` `w` `W` `a` | jump to today / yday / 24h / 7d / this-week / all |
| `r` | force refresh |
| `s` `p` | overlay: sessions / perf |
| `?` | help overlay |
| `Esc` | close overlay |
| `q` / `Ctrl-C` | quit |

The header **status block** is always-on: port-bound dot, daemon pid, daemon uptime, clock. Flytrap health is permanently in the chrome.

The **range dial** at the bottom is the primary interactivity. Time is the X axis; every panel keeps the same range so drilling preserves context.

## Sources / dependencies

- [hudsucker](https://github.com/omjadas/hudsucker) — MIT/Apache, hyper-based flytrap library
- [rcgen](https://github.com/rustls/rcgen) — CA + cert generation
- [rustls](https://github.com/rustls/rustls) + `aws-lc-rs` — TLS server side
- [serde_json](https://github.com/serde-rs/json) — JSON parse + emit for request mutation, ledger, config
- [clap](https://github.com/clap-rs/clap) — CLI args + subcommands
- [dashmap](https://github.com/xacrimon/dashmap) — concurrent map for the request→response flow stash
- [ratatui](https://ratatui.rs/) — TUI rendering
- [tokio](https://tokio.rs/), [hyper](https://hyper.rs/) — async runtime + HTTP plumbing

## Earlier pilot (archived)

[`docs/archive/lua/`](archive/lua/) contains a pre-Rust pilot that ran cc-flytrap as a Lua plugin inside [proxelar](https://github.com/emanuele-em/proxelar) (a Rust HTTPS proxy). It worked for request mutation but proxelar buffers response bodies whenever a script is loaded — the streaming UX collapses to one chunk at end-of-stream. This Rust implementation was the response to that limit.

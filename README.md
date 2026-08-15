# ccft

<img width="1412" height="788" alt="Screenshot 2026-05-10 at 4 09 02 PM" src="https://github.com/user-attachments/assets/19417efd-da43-446c-bc67-b08c053f65f6" />

**ccft: an agentic self improvement tool.** A streaming flytrap that sits at the network boundary between a coding agent and its API endpoint, mutates the request system prompt to your preferences, and writes a per-response token ledger — all while preserving token-by-token streaming UX byte-for-byte.

Three design properties:

1. **One artifact, one location.** The binary at `~/.local/bin/ccft` is everything. No install dir, no rsync.
2. **Streaming is preserved.** A custom `Body` wrapper taps the upstream SSE chunk-by-chunk. Tokens reach the client at the same cadence as direct.
3. **Tiny resident footprint.** ~6 MB on disk, ~4 MB resident at idle.

> Service auto-start runs on **macOS** (launchd) and **Linux** (systemd-user). On Windows, `ccft install` sets up the binary + CA + config; auto-start isn't wired yet — run `ccft run` manually.

## Install

Prebuilt binary from the latest release:

```bash
# macOS (universal aarch64 + x86_64)
curl -L https://github.com/adiled/ccft/releases/latest/download/ccft-macos-universal -o /usr/local/bin/ccft

# Linux (x86_64)
curl -L https://github.com/adiled/ccft/releases/latest/download/ccft-linux-x86_64 -o /usr/local/bin/ccft

chmod +x /usr/local/bin/ccft
ccft install
ccft trust --apply
```

Or build from source (`brew install rust`):

```bash
make install
ccft trust --apply
```

`ccft install` provisions the CA, default config, plist, and launchd unit. `ccft trust --apply` writes the proxy + CA env into `~/.cc-flytrap/ccft.env` and sources it from every shell RC it finds (`.zshenv`, `.zshrc`, `.bashrc`, …), so every shell-launched agent inherits the trust. Full lifecycle in [`docs/install.md`](docs/install.md).

## What's inside

**TUI** — `ccft` at a tty opens a full-screen dashboard: brainrot chart (bot/driver vibes over time), heat-by-time bars, recent-traffic ledger, sessions/perf overlays. Range dial keys: `t y h w W a`.

**Ledger** — every request gets a JSONL line at `~/.local/share/ccft/ledger.jsonl` with input/output tokens, cache hits, latency, model, session id, and ccft's own processing time. Schema in [`docs/reference.md`](docs/reference.md#ledger-schema).

**Dev mode** — `ccft dev` sets up a parallel dev system: a separate `com.ccft.dev` service unit on port 7179 with an isolated config + ledger, independent of the main install. Run it locally at your own accord with `CCFT_DEV=1 ccft run`. Details in [`docs/install.md#dev-mode`](docs/install.md#dev-mode).

**Config** — three knobs in `~/.config/ccft/ccft.json`: `system_override` (extra system prompt), `pain` (false trims Claude Code's bloat blocks), `ledger` (write JSONL). See [`docs/reference.md#config`](docs/reference.md#config).

**Architecture** — hudsucker (hyper-1.x + tokio + rustls), host-gated to known model-provider hosts (`api.anthropic.com`, plus any configured OpenAI-compatible local servers). Other CONNECT requests pass straight through, so `gh`, `git`, `npm`, `pip` keep working from any subprocess. See [`docs/architecture.md`](docs/architecture.md).

## Docs

- [Install / uninstall / lifecycle / dev mode](docs/install.md)
- [Config / ledger schema / file layout / CLI reference](docs/reference.md)
- [How it works / TUI / dependencies](docs/architecture.md)

## License

MIT — see [`LICENSE`](LICENSE).

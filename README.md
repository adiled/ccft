# ccft

<img width="1412" height="788" alt="ccft-tui-screenshot" src="https://github.com/user-attachments/assets/19417efd-da43-446c-bc67-b08c053f65f6" />

**ccft: an agentic self improvement tool.** CCFT sits at your network edge keeping an eye on agentic traffic, maintaining a ledger, and delivering you an accounting of key metrics, all for good health for the bots, and the drivers.

What makes ccft truly a self-improvement tool? It weighs bots against drivers, which means wherever you are on agentic adoption spectrum, you'll always be aware of how effective the driving is. Use it and find out how! ;)

## Install ccft

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

`ccft install` provisions the CA, default config, plist, and launchd unit.

`ccft trust --apply` writes the proxy + CA env into `~/.cc-flytrap/ccft.env` and sources it from every shell RC it finds (`.zshenv`, `.zshrc`, `.bashrc`, …), so every shell-launched agent inherits the trust.

ccft trust only applies to ccft-configured hosts. By default, the installation configures only known AI providers, which ensures your other network activites are not intercepted at all.

ccft was first built for hygiene of remote AI provider traffic, these features include `pain` and `highway`, where pain is off by default, and highway is on by default, these features ensure your agentic experience is free from corruption. This also means some "features" of corrupt harnesses stop working.

## What's inside

**TUI** `ccft` at a tty opens a full-screen dashboard: brainrot chart (bot/driver vibes over time), heat-by-time bars, recent-traffic ledger, sessions/perf overlays. Range dial keys: `t y h w W a`.

**Ledger** every request gets a JSONL line at `~/.local/share/ccft/ledger.jsonl` with input/output tokens, cache hits, latency, model, session id, and ccft's own processing time. Schema in [`docs/reference.md`](docs/reference.md#ledger-schema).

**Config** knobs in `~/.config/ccft/ccft.json`: `system_override` (extra system prompt), `pain` (false trims Claude Code's bloat blocks), `ledger` (write JSONL), `hosts` (which `host[:port]` endpoints ccft flytraps; env vars like `OLLAMA_HOST` union in automatically). See [`docs/reference.md#config`](docs/reference.md#config).

## Learn more

- [Install / uninstall / lifecycle / dev mode](docs/install.md)
- [Config / ledger schema / file layout / CLI reference](docs/reference.md)
- [Architecture / under the hood](docs/architecture.md)

## License

MIT - see [`LICENSE`](LICENSE).

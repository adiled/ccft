# Reference

## Config

`~/.config/ccft/ccft.json` (or `$CCFT_CONFIG`):

```json
{
  "host":            "127.0.0.1",
  "port":            7178,
  "system_override": "",
  "pain":            false,
  "ledger":          true
}
```

| Key | Default | Meaning |
|---|---|---|
| `host` | `"127.0.0.1"` | Bind address |
| `port` | `7178` | Bind port |
| `system_override` | `""` | Extra system block injected into every `/v1/messages`. Empty = skip injection. |
| `pain` | `false` | `false` trims Claude Code's three large bloat blocks; `true` leaves them alone. (Only relevant to Claude Code traffic.) |
| `ledger` | `true` | Write per-response JSONL records to `~/.local/share/ccft/ledger.jsonl`. |

Config reload requires restart (`ccft restart`).

## Ledger schema

Each line in `~/.local/share/ccft/ledger.jsonl`:

```json
{
  "ts": 1778104532.67977, "te": 1778104534.415335,
  "dt": "2026-05-06 21:55:32",
  "human": "adil", "agent": "host-70bbe330",
  "sid": "170bf0f2-ae59-46b6-af51-22c1b107c08e",
  "cip": "127.0.0.1:54188", "pip": "124.29.237.112", "sip": null,
  "reg": null, "model": "claude-haiku-4-5-20251001",
  "in": 520, "out": 84, "tot": 604, "lat": 758,
  "cr": 0, "cc": 0, "c_us": 115
}
```

The TUI brainrot panel reads this file as-is.

## File layout

| Path | Owner | What |
|---|---|---|
| `~/.local/bin/ccft` | `ccft install` | The binary itself |
| `~/Library/LaunchAgents/com.ccft.plist` | `ccft install` (macOS) | launchd unit |
| `~/.config/systemd/user/com.ccft.service` | `ccft install` (Linux) | systemd-user unit |
| `~/.cc-flytrap/ca.pem`, `ca.key` | `ccft install` (or first run) | Self-signed CA |
| `~/.config/ccft/ccft.json` | user / `ccft install` (default) | Production config |
| `~/.config/ccft/dev.json` | user (optional) | Dev config |
| `~/.local/share/ccft/ledger.jsonl` | runtime | Production ledger |
| `~/.local/share/ccft/state.jsonl` | runtime | `ledger_on`/`ledger_off` events |
| `~/.local/share/ccft/dev/ledger.jsonl` | `ccft dev` | Dev ledger |
| `~/.local/share/ccft/logs/launchd.log` | macOS launchd | stdout+stderr from the service |
| `journalctl --user -u com.ccft` | Linux systemd | stdout+stderr from the service |

## Subcommand reference

| Command | What |
|---|---|
| `ccft run` | Run flytrap in foreground using production config (what launchd invokes) |
| `ccft dev` | Set up the parallel dev system: install a separate `com.ccft.dev` service unit (dev.json on port 7179, isolated dev ledger). Run it locally at your own accord with `CCFT_DEV=1 ccft run` |
| `ccft install` | Copy binary, generate CA, write plist, bootstrap launchd |
| `ccft uninstall` | Bootout, remove plist + binary, keep CA/config/ledger |
| `ccft status` | Print install + load + bind state |
| `ccft start` | `launchctl kickstart` |
| `ccft stop` | `launchctl bootout` |
| `ccft restart` | bootout + bootstrap |
| `ccft trust` | Print env vars for routing any coding agent through ccft |
| `ccft trust --apply` | Write proxy + CA env into `~/.cc-flytrap/ccft.env`, source it from every shell RC (with backup) |
| `ccft trust --revoke` | Remove the sourced env block from shell RCs (with backup) |
| `ccft trust --ca` | Dump CA PEM to stdout |
| `ccft logs [-n 50]` | Tail `launchd.log` |
| `ccft tui` (or `ccft` alone at a tty) | Full-screen interactive dashboard with time-dimension dials |

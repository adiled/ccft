# Install

## Supported platforms

| OS | Service auto-start | Lifecycle |
|---|---|---|
| **macOS** | `launchd` user agent | full (`ccft start/stop/restart/status/logs`) |
| **Linux** | `systemd-user` unit | full (`ccft start/stop/restart/status/logs` via `systemctl --user` + `journalctl`) |
| **Windows** | not implemented yet | manual (`ccft run` in a terminal, or wrap with NSSM/sc.exe) |

For source build: `rustc` ≥ 1.95 (`brew install rust` on mac, distro package on linux).

## Quick install (from source)

```bash
make install         # build + ccft install
ccft trust --apply   # write env into ~/.cc-flytrap/ccft.env + source from shell RCs
```

`ccft install` does five things, idempotently:

1. Generates a self-signed CA at `~/.cc-flytrap/{ca.pem,ca.key}` (if missing).
2. Writes a default config at `~/.config/ccft/ccft.json` (if missing).
3. Copies the running binary to `~/.local/bin/ccft`.
4. Writes the platform's service unit pointing at the installed binary:
   - macOS: `~/Library/LaunchAgents/com.ccft.plist` (RunAtLoad, KeepAlive)
   - Linux: `~/.config/systemd/user/com.ccft.service` (Restart=always)
5. Registers it with the platform's user-mode service manager (`launchctl bootstrap` / `systemctl --user enable --now`).

After install, the flytrap is running on `127.0.0.1:7178`. To route an agent through it:

```bash
ccft trust --apply   # writes HTTPS_PROXY + NODE_EXTRA_CA_CERTS into ~/.cc-flytrap/ccft.env, sourced from shell RCs
# — or, manually —
export HTTPS_PROXY=http://127.0.0.1:7178
export NODE_EXTRA_CA_CERTS=$HOME/.cc-flytrap/ca.pem
```

`ccft trust --revoke` reverses the env edits cleanly.

## Uninstall

```bash
ccft uninstall
```

Bootout, removes the plist, removes the installed binary. **Keeps** the CA cert, config, and ledger so a re-install picks up where you left off. To purge:

```bash
rm -rf ~/.cc-flytrap ~/.config/ccft ~/.local/share/ccft
```

## Lifecycle

```bash
ccft status                  # is it loaded? bound? on which port?
ccft start                   # kick launchd
ccft stop                    # bootout (will respawn on next login)
ccft restart                 # bootout + bootstrap
ccft logs                    # tail launchd output
ccft logs -n 200             # last 200 lines
```

## Dev mode

```bash
make dev                     # builds, then runs `ccft dev` in foreground
# — or, hot iterate —
cargo run --release -- dev
```

`ccft dev` sets up a **parallel dev system**: a separate `com.ccft.dev`
service unit running the dev config on 7179 with an isolated dev ledger. It
never touches the main install. The dev invoker (harness) can then run the
proxy locally at its own accord with `CCFT_DEV=1 ccft run` to verify things.

| | Production (`ccft run`) | Dev (`ccft dev` / `CCFT_DEV=1`) |
|---|---|---|
| Port | 7178 | 7179 |
| Config | `~/.config/ccft/ccft.json` | `~/.config/ccft/dev.json` |
| Ledger | `~/.local/share/ccft/ledger.jsonl` | `~/.local/share/ccft/dev/ledger.jsonl` |
| Service unit | `com.ccft` | `com.ccft.dev` |
| Process | launchd-managed | launchd-managed; run `CCFT_DEV=1 ccft run` for foreground |
| CA | shared `~/.cc-flytrap/ca.pem` | shared `~/.cc-flytrap/ca.pem` |

To use dev: `HTTPS_PROXY=http://127.0.0.1:7179 NODE_EXTRA_CA_CERTS=$HOME/.cc-flytrap/ca.pem your-agent -p "..."`. The CA is shared so trust setup carries over.

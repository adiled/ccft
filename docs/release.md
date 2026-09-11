# Release to crates.io

ccft ships through **crates.io** (`cargo install ccft`), not GitHub binary
downloadables. This avoids the macOS Gatekeeper "unverified / delete it"
problem entirely — `cargo install` builds from source, so there is no
unsigned binary to block.

## Publish order (dependency order matters)

Each crate's `path` deps become `version` deps in the published manifest, and
cargo resolves them against the crates.io index — so dependency crates must
land **before** the crates that depend on them:

1. `ccft-ledger`  — no ccft deps
2. `ccft-lex`     — no ccft deps
3. `ccft-session` — no ccft deps
4. `ccft-sse`     — no ccft deps
5. `ccft-brainrot`— depends on `ccft-ledger`
6. `ccft`         — depends on all five

## One-time setup

```bash
cargo login        # crates.io API token → ~/.cargo/credentials
```

In GitHub, add a **`CARGO_REGISTRY_TOKEN`** secret (the crates.io API token).
The tag-push workflow (`.github/workflows/release.yml`) uses it.

## Manual publish (if you want to do it by hand)

```bash
# verify each package builds as it would on crates.io
cargo package -p ccft-ledger
cargo package -p ccft-lex
cargo package -p ccft-session
cargo package -p ccft-sse

# publish in dependency order
cargo publish -p ccft-ledger
cargo publish -p ccft-lex
cargo publish -p ccft-session
cargo publish -p ccft-sse
cargo publish -p ccft-brainrot   # after ccft-ledger is live
cargo publish -p ccft            # after all five are live
```

`cargo publish --allow-dirty` if the working tree is dirty (e.g. new READMEs).

> Note: `cargo package`/`cargo publish` for a crate whose dependency isn't yet
> on crates.io will fail with `no matching package named … found` — that's the
> signal you've hit a dependency that needs publishing first. It's expected.

## Version bumps

Keep the workspace versions in sync when cutting a release:

- Root `ccft` version is the real user-facing version (e.g. `1.11.0`).
- Sub-crates are `0.1.x`; bump them only when their APIs change. A `^0.1`
  requirement is fine — cargo uses the workspace `path` during dev and the
  registry version after publish.

## Tag → workflow

Tagging `vX.Y.Z` triggers the release workflow: it publishes all six crates in
order, then opens a **source-only** GitHub release (notes + tarball) with no
executable artifacts. Users update via `ccft update` or the startup auto-update.

## Verify after publish

```bash
cargo install ccft        # clean install from the registry
ccft --version
ccft install
ccft trust --apply
ccft update                # explicit update path
```

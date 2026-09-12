# Release to crates.io

ccft ships through **crates.io** (`cargo install ccft`), not GitHub binary
downloadables. This avoids the macOS Gatekeeper "unverified / delete it"
problem entirely — `cargo install` builds from source, so there is no
unsigned binary to block.

## One crate, one publish

ccft is a single self-contained crate. It ships both a binary (`src/main.rs`)
and a library (`src/lib.rs`) that re-exports the reusable modules
(`ccft_ledger`, `ccft_session`, `ccft_lex`, `ccft_brainrot`, `ccft_sse`) so
downstream Rust code can `use ccft::ledger::…` / `ccft::sse::…`. There are no
separate sub-crates to publish in dependency order.

## One-time setup

```bash
cargo login        # crates.io API token → ~/.cargo/credentials
```

In GitHub, add a **`CRATES_IO_TOKEN`** secret (the crates.io API token).
The tag-push workflow (`.github/workflows/release.yml`) uses it.

## Manual publish (if you want to do it by hand)

```bash
# verify the package builds as it would on crates.io
cargo package -p ccft

# publish
cargo publish -p ccft
```

`cargo publish --allow-dirty` if the working tree is dirty.

## Version bumps

Bump the single `version` in `Cargo.toml` when cutting a release (e.g.
`1.10.0` → `1.11.0`). Keep it in sync with `release.sh`.

## Tag → workflow

Tagging `vX.Y.Z` triggers the release workflow: it publishes `ccft` to
crates.io, then opens a **source-only** GitHub release (notes + tarball) with
no executable artifacts. Users update via `ccft update` or the startup
auto-update.

## Verify after publish

```bash
cargo install ccft        # clean install from the registry
ccft --version
ccft install
ccft trust --apply
ccft update                # explicit update path
```

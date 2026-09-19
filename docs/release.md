# Releasing

```bash
./release.sh patch    # 1.10.6 -> 1.10.7 (default)
./release.sh minor    # 1.10.6 -> 1.11.0
./release.sh major    # 1.10.6 -> 2.0.0
```

That's it. The script bumps `Cargo.toml` (and syncs `Cargo.lock`), commits,
tags `vX.Y.Z`, and pushes. The tag push triggers the workflow, which
publishes to crates.io (token comes from the `CRATES_IO_TOKEN` secret) and
opens a source-only GitHub release.

Verify when it's done:

```bash
cargo install ccft
ccft --version
```
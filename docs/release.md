# Releasing

```bash
./release.sh patch    # 1.10.2 -> 1.10.3 (default)
./release.sh minor    # 1.10.2 -> 1.11.0
./release.sh major    # 1.10.2 -> 2.0.0
./release.sh 2.3.4    # exact version
```

That's it. The script bumps `VERSION`/`Cargo.toml`/`Cargo.lock`, commits,
tags `vX.Y.Z`, and pushes. The tag push triggers the workflow, which
publishes to crates.io (token comes from the `CRATES_IO_TOKEN` secret) and
opens a source-only GitHub release.

Verify when it's done:

```bash
cargo install ccft
ccft --version
```

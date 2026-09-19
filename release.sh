#!/usr/bin/env bash
set -euo pipefail

# release.sh - bump version, sync lockfile, tag, and push.
# Usage: ./release.sh [patch|minor|major]
#
# Examples:
#     ./release.sh patch            # bumps 1.10.6 -> 1.10.7
#     ./release.sh minor            # bumps 1.10.6 -> 1.11.0
#     ./release.sh major            # bumps 1.10.6 -> 2.0.0
#
# Core (always): Cargo.toml + Cargo.lock.

# --- Project add-ons (optional) ----------------------------------------------
# Extra files kept in version lockstep with Cargo.toml. Leave empty for plain
# Rust projects. Example: (orchd-osx/build.zig.zon orchd-apple/build.zig.zon)
EXTRA_VERSIONED_FILES=()
# -----------------------------------------------------------------------------

BRANCH="$(git branch --show-current)"
if [[ "$BRANCH" != "main" ]]; then
  echo "error: must be on main (currently on '$BRANCH')" >&2
  exit 1
fi

# Sync with origin
git fetch origin main
git reset --hard origin/main

# Determine version
NEW_VER=""
case "${1:-patch}" in
  patch)
    OLD_VER="$(grep '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
    IFS='.' read -r MAJOR MINOR PATCH <<< "$OLD_VER"
    NEW_VER="$MAJOR.$MINOR.$((PATCH + 1))"
     ;;
  minor)
    OLD_VER="$(grep '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
    IFS='.' read -r MAJOR MINOR PATCH <<< "$OLD_VER"
    NEW_VER="$MAJOR.$((MINOR + 1)).0"
     ;;
  major)
    OLD_VER="$(grep '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')"
    IFS='.' read -r MAJOR MINOR PATCH <<< "$OLD_VER"
    NEW_VER="$((MAJOR + 1)).0.0"
     ;;
  *)
    echo "error: unknown bump type '${1}' (use patch|minor|major)" >&2
    exit 1
     ;;
esac

# Files kept in version lockstep with Cargo.toml
VERSIONED_FILES=(Cargo.toml "${EXTRA_VERSIONED_FILES[@]}")

# Every versioned file must already carry OLD_VER, or versions have drifted.
for f in "${VERSIONED_FILES[@]}"; do
  if ! grep -q "\"$OLD_VER\"" "$f"; then
    echo "error: $f does not contain version $OLD_VER (files out of sync?)" >&2
    exit 1
  fi
done

# Bump versions in all lockstep files:
#   Cargo.toml      -> `version = "x.y.z"`
#   build.zig.zon   -> `.version = "x.y.z"`   (fingerprint is name-bound, unchanged)
for f in "${VERSIONED_FILES[@]}"; do
  sed -i '' -e "s/^version = \"$OLD_VER\"/version = \"$NEW_VER\"/" \
            -e "s/\.version = \"$OLD_VER\"/.version = \"$NEW_VER\"/" "$f"
done

# Update Cargo.lock to match
cargo generate-lockfile

# Verify no uncommitted changes aside from version bump
if ! git diff --quiet -- "${VERSIONED_FILES[@]}" Cargo.lock; then
  git add "${VERSIONED_FILES[@]}" Cargo.lock
  git commit -m "chore: bump to $NEW_VER"
fi

# Tag and push
TAG="v$NEW_VER"
git tag -a "$TAG" -m "v$NEW_VER"
git push origin main --tags
echo "Released $TAG ✓"
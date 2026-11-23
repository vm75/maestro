#!/usr/bin/env bash
set -euo pipefail
FEATURES=(--no-default-features --features asuswrt)
SUFFIX=""
if [[ "${1:-}" == "--linux-ui" ]]; then
  shift
  REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  npm --prefix "$REPO_DIR/linux-web" ci
  npm --prefix "$REPO_DIR/linux-web" run build
  FEATURES=(--no-default-features --features linux-ui)
  SUFFIX="-linux-ui"
fi
TARGET="${1:-aarch64-unknown-linux-musl}"
ARTIFACT="maestro-$TARGET$SUFFIX"
rustup target add "$TARGET"
cargo build --release --locked --target "$TARGET" "${FEATURES[@]}"
mkdir -p dist
cp "target/$TARGET/release/maestro" "dist/$ARTIFACT"
(
  cd dist
  sha256sum "$ARTIFACT" > "$ARTIFACT.sha256"
)
echo "Built dist/$ARTIFACT"

#!/usr/bin/env bash
set -euo pipefail
REPO_DIR=$(CDPATH= cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$REPO_DIR"
TMP=$(mktemp -d)
NAME=router-hub_0.10.2-1_aarch64-3.10.ipk
trap 'rm -rf "$TMP"; rm -f "dist/$NAME" "dist/$NAME.sha256"' EXIT
printf '#!/bin/sh\nexit 0\n' > "$TMP/router-hub"
chmod +x "$TMP/router-hub"
bash scripts/package-ipk.sh v0.10.2 aarch64-unknown-linux-musl "$TMP/router-hub"
(
  cd dist
  sha256sum -c "$NAME.sha256"
)
test "$(tar -tzf "dist/$NAME" | tr '\n' ' ')" = "./debian-binary ./control.tar.gz ./data.tar.gz "
tar -xzOf "dist/$NAME" ./control.tar.gz > "$TMP/control.tar.gz"
tar -xzOf "dist/$NAME" ./data.tar.gz > "$TMP/data.tar.gz"
tar -xOzf "$TMP/control.tar.gz" ./control | grep -Fx 'Architecture: aarch64-3.10'
tar -tzf "$TMP/control.tar.gz" | grep -Fx './postinst'
tar -tzf "$TMP/control.tar.gz" | grep -Fx './prerm'
tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/bin/router-hub'
tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/etc/init.d/S99router-hub'
tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/libexec/router-hub/setup-merlin.sh'
tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/share/router-hub/router-hub.example.toml'
echo "IPK package structure and checksum passed"

#!/usr/bin/env bash
set -euo pipefail
REPO_DIR=$(CDPATH= cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$REPO_DIR"
TMP=$(mktemp -d)
VERSION=$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)
NAME="maestro_${VERSION}-1_aarch64-3.10.ipk"
trap 'rm -rf "$TMP"' EXIT
PACKAGE_DIR="$TMP/ipk"
BINARY=${1:-"$TMP/maestro"}
if [[ $# -eq 0 ]]; then
    printf '#!/bin/sh\nexit 0\n' > "$BINARY"
    chmod +x "$BINARY"
fi
MAESTRO_IPK_OUTPUT_DIR="$PACKAGE_DIR" bash scripts/package-ipk.sh "v${VERSION}" aarch64-unknown-linux-musl "$BINARY"
(
  cd "$PACKAGE_DIR"
  sha256sum -c "$NAME.sha256"
)
test "$(tar -tzf "$PACKAGE_DIR/$NAME" | tr '\n' ' ')" = "./debian-binary ./control.tar.gz ./data.tar.gz "
tar -xzOf "$PACKAGE_DIR/$NAME" ./control.tar.gz > "$TMP/control.tar.gz"
tar -xzOf "$PACKAGE_DIR/$NAME" ./data.tar.gz > "$TMP/data.tar.gz"
tar -xOzf "$TMP/control.tar.gz" ./control | grep -Fx 'Architecture: aarch64-3.10'
tar -xOzf "$TMP/control.tar.gz" ./control | grep -Fx 'Conflicts: router-hub'
tar -xOzf "$TMP/control.tar.gz" ./control | grep -Fx 'Depends: tar'
tar -tzf "$TMP/control.tar.gz" | grep -Fx './postinst'
tar -tzf "$TMP/control.tar.gz" | grep -Fx './prerm'
# Installation consumes YAML only; migrate the persistent YAML-era layout first.
tar -xzOf "$TMP/control.tar.gz" ./postinst | grep -F 'setup-merlin.sh'
setup=scripts/setup-merlin.sh
init_line=$(grep -nF 'init-nginx' "$setup" | tail -1 | cut -d: -f1)
start_line=$(grep -nF '/opt/etc/init.d/S99maestro restart' "$setup" | tail -1 | cut -d: -f1)
migration_line=$(grep -nF 'migrate-router-layout' "$setup" | cut -d: -f1)
test -n "$migration_line" && test "$migration_line" -lt "$init_line"
! grep -Eq 'migrate-config|migrate-nginx-inputs|--no-check-certificate|curl .* -k ' "$setup"
test -n "$init_line" && test -n "$start_line" && test "$init_line" -lt "$start_line"
tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/bin/maestro'
tar -xzOf "$TMP/data.tar.gz" ./opt/bin/maestro > "$TMP/packaged-maestro"
cmp "$BINARY" "$TMP/packaged-maestro"
wc -c "$TMP/packaged-maestro" "$PACKAGE_DIR/$NAME"
if LC_ALL=C grep -aqF '/linux-assets/' "$TMP/packaged-maestro"; then
    echo "Unexpected Linux UI asset route in the Asuswrt executable" >&2
    exit 1
fi
! tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/bin/router-hub'
tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/etc/init.d/S99maestro'
tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/libexec/maestro/setup-merlin.sh'
tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/share/maestro/dehydrated-dns01-hook.sh'
! tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/var/lib/maestro/dehydrated/dehydrated-dns01-hook.sh'
tar -tzf "$TMP/data.tar.gz" | grep -Fx './opt/share/maestro/maestro.example.yaml'
# Exercise the actual packaged hook setup with no install utility in PATH.
hook_test="$TMP/hook-test"
mkdir -p "$hook_test/bin" "$hook_test/examples" "$hook_test/scripts"
for tool in cp chmod mkdir dirname; do
    ln -s "$(command -v "$tool")" "$hook_test/bin/$tool"
done
tar -xzOf "$TMP/data.tar.gz" ./opt/libexec/maestro/setup-merlin.sh |
    sed -n '/^if .* -L .*HOOK/,/^# The application updater/p' > "$hook_test/setup"
test -s "$hook_test/setup"
tar -xzOf "$TMP/data.tar.gz" ./opt/share/maestro/dehydrated-dns01-hook.sh > "$hook_test/examples/dehydrated-dns01-hook.sh"
run_hook_setup() {
    PATH="$hook_test/bin" HOOK="$1" EXAMPLES_DIR="$hook_test/examples" \
        /bin/sh -eu "$hook_test/setup"
}
hook="$hook_test/state/hook.sh"
run_hook_setup "$hook"
cmp "$hook_test/examples/dehydrated-dns01-hook.sh" "$hook"
test "$(stat -c '%a' "$hook")" = 755
# Existing custom hooks and symlinks are not overwritten.
printf 'custom hook\n' > "$hook"
run_hook_setup "$hook"
grep -Fx 'custom hook' "$hook"
ln -s "$hook" "$hook_test/existing-link"
run_hook_setup "$hook_test/existing-link"
test -L "$hook_test/existing-link"
grep -Fx 'custom hook' "$hook"
ln -s "$hook_test/missing-target" "$hook_test/dangling-link"
run_hook_setup "$hook_test/dangling-link"
test -L "$hook_test/dangling-link"
test ! -e "$hook_test/missing-target"
# Tarball fallback source and a symlinked destination directory.
mv "$hook_test/examples/dehydrated-dns01-hook.sh" "$hook_test/scripts/"
ln -s "$hook_test/state" "$hook_test/linked-state"
run_hook_setup "$hook_test/linked-state/fallback.sh"
test -L "$hook_test/linked-state"
cmp "$hook_test/scripts/dehydrated-dns01-hook.sh" "$hook_test/state/fallback.sh"
test "$(stat -c '%a' "$hook_test/state/fallback.sh")" = 755
# Exercise the packaged init script without touching host services or /opt.
mkdir -p "$TMP/bin"
tar -xzOf "$TMP/data.tar.gz" ./opt/etc/init.d/S99maestro | \
    sed -e 's|^PATH=.*|PATH="$INIT_TEST_BIN:/usr/bin:/bin"|' \
        -e 's|^\. /opt/etc/init.d/rc.func$|. "$INIT_TEST_RC"|' > "$TMP/init"
cat > "$TMP/bin/pidof" <<'EOF'
#!/bin/sh
exit "${INIT_TEST_PID_STATUS:-1}"
EOF
chmod +x "$TMP/bin/pidof"
# Model rc.func's stop behavior: dead returns 1, successful stop returns 0.
cat > "$TMP/rc.func" <<'EOF'
printf '%s\n' "$1" >> "$INIT_TEST_CALLS"
return "${INIT_TEST_RC_STATUS:-1}"
EOF
export INIT_TEST_BIN="$TMP/bin" INIT_TEST_RC="$TMP/rc.func" INIT_TEST_CALLS="$TMP/calls"
sh "$TMP/init" stop
test ! -e "$TMP/calls"
INIT_TEST_PID_STATUS=0 INIT_TEST_RC_STATUS=0 sh "$TMP/init" stop
test "$(cat "$TMP/calls")" = stop
# A real stop failure and a dead service check must still report failure.
if INIT_TEST_PID_STATUS=0 sh "$TMP/init" stop; then
    echo "Init script hid a stop failure" >&2
    exit 1
fi
if sh "$TMP/init" check; then
    echo "Init script reported a stopped service as running" >&2
    exit 1
fi
echo "IPK package structure and checksum passed"

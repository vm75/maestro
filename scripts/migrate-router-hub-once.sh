#!/bin/sh
# One-time, operator-invoked data staging from Router Hub to Maestro.
# Not installed by the IPK and never invoked by Maestro at runtime.
set -eu

die() { printf 'Migration refused: %s\n' "$*" >&2; exit 1; }

if [ "$#" -eq 0 ]; then
    ROOT=/
elif [ "$#" -eq 2 ] && [ "$1" = "--root" ]; then
    ROOT=$2
else
    die "Usage: MAESTRO_BACKUP_VERIFIED=YES $0 [--root TEST_ROOT]"
fi
[ -d "$ROOT" ] || die "Root directory does not exist: $ROOT"
ROOT=$(cd "$ROOT" && pwd)
[ "${MAESTRO_BACKUP_VERIFIED:-}" = YES ] || die "Back up config, state, nginx, JFFS and the old IPK off-router; set MAESTRO_BACKUP_VERIFIED=YES only after checking the backup."
if [ "$ROOT" = / ]; then
    [ "$(id -u)" = 0 ] || die "Run as root on the router"
    if command -v pidof >/dev/null 2>&1 && pidof router-hub >/dev/null 2>&1; then
        die "Stop Router Hub before staging its data"
    fi
    PREFIX=
else
    PREFIX=$ROOT
fi

OLD_CONFIG_DIR="$PREFIX/opt/etc/router-hub"
OLD_CONFIG="$OLD_CONFIG_DIR/router-hub.toml"
OLD_DATA_DIR="$PREFIX/opt/var/lib/router-hub"
NEW_CONFIG_DIR="$PREFIX/opt/etc/maestro"
NEW_CONFIG="$NEW_CONFIG_DIR/maestro.toml"
NEW_DATA_DIR="$PREFIX/opt/var/lib/maestro"
NGINX_ROOT="$PREFIX/opt/etc/nginx"

[ -f "$OLD_CONFIG" ] || die "Missing existing Router Hub configuration: $OLD_CONFIG"
[ ! -e "$NEW_CONFIG_DIR" ] && [ ! -L "$NEW_CONFIG_DIR" ] || die "Maestro config directory already exists; refusing to overwrite"
[ ! -e "$NEW_DATA_DIR" ] && [ ! -L "$NEW_DATA_DIR" ] || die "Maestro data directory already exists; refusing to overwrite"

umask 077
mkdir -p "$PREFIX/opt/etc" "$PREFIX/opt/var/lib"
# Never mutate or move the old directories: they are the rollback copy.
cp -pR "$OLD_CONFIG_DIR" "$NEW_CONFIG_DIR"
if [ -d "$OLD_DATA_DIR" ]; then
    cp -pR "$OLD_DATA_DIR" "$NEW_DATA_DIR"
else
    mkdir -p "$NEW_DATA_DIR"
fi
mv "$NEW_CONFIG_DIR/router-hub.toml" "$NEW_CONFIG"
chmod 0600 "$NEW_CONFIG"

# Rewrite only known absolute directory roots and the old default ipset names.
# Do not indiscriminately rename domains, user labels or custom configuration.
find "$NEW_CONFIG_DIR" "$NEW_DATA_DIR" -type f \( -name '*.toml' -o -name '*.json' \) -exec sed -i \
    -e 's|/opt/etc/router-hub|/opt/etc/maestro|g' \
    -e 's|/opt/var/lib/router-hub|/opt/var/lib/maestro|g' \
    -e 's|/opt/var/run/router-hub|/opt/var/run/maestro|g' \
    -e 's|/opt/var/log/router-hub|/opt/var/log/maestro|g' \
    -e 's|router_hub_bans4|maestro_bans4|g' \
    -e 's|router_hub_bans6|maestro_bans6|g' '{}' \;

# The settings marker lives in generated nginx .conf files, not just TOML.
# Shared nginx config is intentionally edited only after backup confirmation.
for directory in "$NEW_CONFIG_DIR" "$NEW_DATA_DIR" "$NGINX_ROOT"; do
    [ -d "$directory" ] || continue
    find "$directory" -type f -name '*.conf' -exec sed -i \
        's|^# router-hub-settings: |# maestro-settings: |' '{}' \;
done

if grep -Eq 'router[-_]hub|ROUTER_HUB' "$NEW_CONFIG"; then
    printf '%s\n' "Migration staged, but the new TOML still contains Router Hub identifiers." >&2
    printf '%s\n' "Review and resolve them before installation: $NEW_CONFIG" >&2
    exit 2
fi
printf 'Staged Maestro config: %s\n' "$NEW_CONFIG"
printf 'Staged Maestro data:   %s\n' "$NEW_DATA_DIR"
printf '%s\n' "Original Router Hub directories were not modified or removed."
printf '%s\n' "Next: inspect all paths and state, remove old JFFS hooks, uninstall Router Hub,"
printf '%s\n' "reboot to clear old firewall chains, verify the cleanup, then install Maestro."

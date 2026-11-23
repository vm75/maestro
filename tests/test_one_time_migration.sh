#!/usr/bin/env bash
# Disposable filesystem test for the explicitly invoked one-time migration.
set -euo pipefail
REPO_DIR=$(CDPATH= cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
ROOT="$TMP/root"
mkdir -p "$ROOT/opt/etc/router-hub/nginx" "$ROOT/opt/var/lib/router-hub/dehydrated" \
         "$ROOT/opt/etc/nginx/domains-available"
cat > "$ROOT/opt/etc/router-hub/router-hub.toml" <<'EOF'
[paths]
data_dir = "/opt/etc/router-hub"
runtime_dir = "/opt/var/run/router-hub"
log_file = "/opt/var/log/router-hub.log"
[nginx]
inputs_dir = "/opt/etc/router-hub/nginx"
[certificates]
certs_dir = "/opt/var/lib/router-hub/dehydrated/certs"
[firewall]
set_name_v4 = "router_hub_bans4"
set_name_v6 = "router_hub_bans6"
EOF
printf '%s\n' '{"path":"/opt/etc/router-hub/nginx"}' > "$ROOT/opt/etc/router-hub/nginx/sites.json"
printf '%s\n' 'cert data' > "$ROOT/opt/var/lib/router-hub/dehydrated/dehydrated"
printf '%s\n' '# router-hub-settings: {"access_log":false}' 'server { return 200; }' \
  > "$ROOT/opt/etc/nginx/domains-available/site.conf"
cp "$ROOT/opt/etc/router-hub/router-hub.toml" "$TMP/original"

if sh "$REPO_DIR/scripts/migrate-router-hub-once.sh" --root "$ROOT" >"$TMP/denied" 2>&1; then
    echo 'Migration ran without confirmed backup' >&2
    exit 1
fi
test ! -e "$ROOT/opt/etc/maestro"
MAESTRO_BACKUP_VERIFIED=YES sh "$REPO_DIR/scripts/migrate-router-hub-once.sh" --root "$ROOT"
cmp "$TMP/original" "$ROOT/opt/etc/router-hub/router-hub.toml"
test -f "$ROOT/opt/var/lib/maestro/dehydrated/dehydrated"
grep -F 'data_dir = "/opt/etc/maestro"' "$ROOT/opt/etc/maestro/maestro.toml"
grep -F 'certs_dir = "/opt/var/lib/maestro/dehydrated/certs"' "$ROOT/opt/etc/maestro/maestro.toml"
grep -F 'set_name_v4 = "maestro_bans4"' "$ROOT/opt/etc/maestro/maestro.toml"
grep -F '/opt/etc/maestro/nginx' "$ROOT/opt/etc/maestro/nginx/sites.json"
grep -F '# maestro-settings: {"access_log":false}' "$ROOT/opt/etc/nginx/domains-available/site.conf"
test "$(stat -c %a "$ROOT/opt/etc/maestro/maestro.toml")" = 600
if MAESTRO_BACKUP_VERIFIED=YES sh "$REPO_DIR/scripts/migrate-router-hub-once.sh" --root "$ROOT" >"$TMP/refused" 2>&1; then
    echo 'Migration overwrote staged Maestro files on rerun' >&2
    exit 1
fi
# Never touch an existing target, even if it contains only a sentinel.
OTHER="$TMP/other"
mkdir -p "$OTHER/opt/etc/router-hub" "$OTHER/opt/etc/maestro" "$OTHER/opt/var/lib"
cp "$TMP/original" "$OTHER/opt/etc/router-hub/router-hub.toml"
printf sentinel > "$OTHER/opt/etc/maestro/sentinel"
if MAESTRO_BACKUP_VERIFIED=YES sh "$REPO_DIR/scripts/migrate-router-hub-once.sh" --root "$OTHER" >"$TMP/overwrite" 2>&1; then
    echo 'Migration accepted an existing Maestro directory' >&2
    exit 1
fi
test "$(cat "$OTHER/opt/etc/maestro/sentinel")" = sentinel
echo 'One-time Router Hub migration staging safety tests passed'

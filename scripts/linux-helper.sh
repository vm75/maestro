#!/usr/bin/env bash
# Explicit, reviewed helper installation for a disposable Linux development host.
# Invoked through sudo by linux-dev.sh; never invoked by the Maestro daemon.
set -euo pipefail
umask 077
export PATH=/usr/sbin:/usr/bin:/sbin:/bin

fail() { printf '%s\n' "$*" >&2; exit 1; }

safe_directory() {
    local path=$1 mode
    [[ -d "$path" && ! -L "$path" && $(stat -c %u -- "$path") == 0 ]] ||
        fail "Unsafe directory: $path"
    mode=$(stat -c %a -- "$path")
    (( (8#$mode & 022) == 0 )) || fail "Non-root-writable directory: $path"
}

safe_file() {
    local path=$1 mask=$2 mode
    [[ -f "$path" && ! -L "$path" && $(stat -c %u -- "$path") == 0 ]] ||
        fail "Unsafe file: $path"
    mode=$(stat -c %a -- "$path")
    (( (8#$mode & mask) == 0 )) || fail "Unsafe permissions: $path"
}

confirm() {
    local answer
    read -r -p "$1 [y/N] " answer || fail 'Cancelled.'
    [[ "$answer" == y || "$answer" == Y ]] || fail 'Cancelled.'
}

proposed_policy() {
    local uid=$1
    local ID='' ID_LIKE='' units
    source /etc/os-release
    case " $ID $ID_LIKE " in
        *' debian '*|*' ubuntu '*) units='"smbd.service","wsdd-server.service","nfs-server.service"' ;;
        *) units='"smb.service","wsdd.service","nfs-server.service"' ;;
    esac
    printf '{\n  "version": 1,\n  "workload_uid": %s,\n  "system_units": [%s],\n  "package_writes": true,\n  "samba_password": true,\n  "share_writes": true\n}\n' "$uid" "$units"
}

main() {
    [[ $# -ge 1 && ( "$1" == install || "$1" == remove ) ]] || fail 'Usage: linux-helper.sh install BINARY | remove'
    local action=$1 binary=${2:-} account uid gid
    [[ "$action" == install && $# == 2 || "$action" == remove && $# == 1 ]] || fail 'Invalid arguments.'
    [[ $EUID == 0 && ${SUDO_UID:-} =~ ^[1-9][0-9]*$ ]] || fail 'Invoke via the non-root development Makefile target.'
    uid=$SUDO_UID
    account=$(id -un "$uid")
    gid=$(id -g "$uid")
    [[ "$account" =~ ^[a-z_][a-z0-9_-]{0,31}$ && "$gid" =~ ^[0-9]+$ ]] || fail 'Invalid workload identity.'
    [[ $(id -u "$account") == "$uid" ]] || fail 'Workload UID mismatch.'
    local policy=/etc/maestro/admin-policy.json rule=/etc/sudoers.d/maestro helper=/usr/local/libexec/maestro-admin
    local expected="$account ALL=(root) NOPASSWD: NOSETENV: /usr/local/libexec/maestro-admin \"\""
    local dir
    for dir in / /usr /usr/local /etc; do safe_directory "$dir"; done
    for dir in /usr/local/libexec /etc/maestro /etc/sudoers.d; do
        if [[ -e "$dir" || -L "$dir" ]]; then safe_directory "$dir"; fi
    done
    if [[ -e "$rule" || -L "$rule" ]]; then
        safe_file "$rule" 022
        [[ $(cat -- "$rule") == "$expected" ]] || fail 'Refusing to replace/remove a foreign or modified sudoers rule.'
    fi
    if [[ -e "$helper" || -L "$helper" ]]; then safe_file "$helper" 06022; fi

    local grants existing=0
    if [[ -e "$policy" || -L "$policy" ]]; then
        safe_file "$policy" 037
        # Fail closed for mismatched accounts and malformed policy. Preserve grants.
        python3 - "$policy" "$uid" <<'PY'
import json, sys
from pathlib import Path
p = Path(sys.argv[1])
if p.stat().st_size > 16384:
    sys.exit("Policy exceeds the helper's size limit.")
data = json.loads(p.read_bytes())
if not isinstance(data, dict) or data.get("version") != 1 or data.get("workload_uid") != int(sys.argv[2]):
    sys.exit("Existing policy belongs to a different workload account or version.")
if set(data) - {"version", "workload_uid", "system_units", "package_writes", "samba_password", "share_writes", "ban_shield", "wireguard_peers"}:
    sys.exit("Existing policy has unknown fields.")
units = data.get("system_units")
if not isinstance(units, list) or len(units) > 128 or any(not isinstance(u, str) for u in units):
    sys.exit("Invalid system_units policy.")
if any(type(data.get(k, False)) is not bool for k in ("package_writes", "samba_password", "share_writes", "ban_shield", "wireguard_peers")):
    sys.exit("Invalid helper grant flags.")
PY
        existing=1
        grants=$(cat -- "$policy")
    fi
    if [[ "$action" == remove ]]; then
        if [[ ! -e "$rule" && -e "$helper" && $existing == 0 ]]; then
            fail 'Refusing to remove an unassociated helper executable.'
        fi
        printf 'Remove helper and exact sudo authorization for %s?\nPolicy and native services/shares will be retained.\n' "$account"
        confirm 'Remove'
        rm -f -- "$rule" "$helper"
        printf 'Helper removed. Restart Maestro and refresh the UI.\n'
        return
    fi

    [[ "$binary" == /* && -f "$binary" && ! -L "$binary" ]] || fail 'Expected a locally built regular helper binary.'
    command -v visudo >/dev/null || fail 'Install sudo/visudo first.'
    if (( ! existing )); then grants=$(proposed_policy "$uid"); fi
    printf 'Install the development helper for %s (UID %s).\n' "$account" "$uid"
    printf 'Binary: %s\nSHA256: ' "$binary"
    sha256sum -- "$binary"
    printf 'Sudoers: %s\nRoot policy (%s):\n%s\n' "$expected" "$([[ $existing == 1 ]] && echo preserved || echo proposed)" "$grants"
    printf 'Package grants permit native package transactions; share grants permit host share publication.\nUse only on a development host or disposable VM.\n'
    confirm 'Install with these reviewed grants'

    for dir in /usr/local/libexec /etc/maestro /etc/sudoers.d; do
        if [[ ! -e "$dir" ]]; then install -d -o root -g root -m 0755 -- "$dir"; fi
        safe_directory "$dir"
    done
    local staged_rule staged_helper staged_policy
    staged_rule=$(mktemp /etc/sudoers.d/.maestro-dev-rule.XXXXXX)
    staged_helper=$(mktemp /usr/local/libexec/.maestro-dev-helper.XXXXXX)
    staged_policy=$(mktemp /etc/maestro/.maestro-dev-policy.XXXXXX)
    trap 'rm -f -- "$staged_rule" "$staged_helper" "$staged_policy"' EXIT
    printf '%s\n' "$expected" > "$staged_rule"
    chmod 0440 "$staged_rule"
    visudo -cf "$staged_rule" >/dev/null
    install -o root -g root -m 0755 -- "$binary" "$staged_helper"
    if (( ! existing )); then
        printf '%s\n' "$grants" > "$staged_policy"
        chown "root:$gid" "$staged_policy"
        chmod 0640 "$staged_policy"
        # Publish without replacing an existing root policy.
        ln -- "$staged_policy" "$policy"
    else
        chgrp "$gid" "$policy"
        chmod 0640 "$policy"
    fi
    mv -fT -- "$staged_helper" "$helper"
    mv -fT -- "$staged_rule" "$rule"
    printf 'Helper installed. Existing policy contents were preserved if present.\n'
    printf 'Review or change grants later with sudoedit %s.\n' "$policy"
    # Clean up while local variables are still in scope.
    rm -f -- "$staged_rule" "$staged_helper" "$staged_policy"
    trap - EXIT
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi

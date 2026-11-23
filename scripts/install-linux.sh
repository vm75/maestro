#!/usr/bin/env bash
# Explicit native Linux deployment; never called by the running daemon.
set -euo pipefail
umask 077

usage() {
  echo "Usage: install-linux.sh --user ACCOUNT --config /absolute/maestro.toml --binary /path/to/release"
  echo "       [--with-helper --helper /path/to/maestro-admin | --remove-helper]"
  echo "       [--enable-linger] [--dry-run] [--yes]"
}
account= config= binary= helper= helper_mode=keep
yes=0 dry=0 linger=0
while (($#)); do
  case "$1" in
    --user|--config|--binary|--helper)
      flag=$1; shift
      (($#)) || { echo "Missing value: $flag" >&2; exit 2; }
      case "$flag" in
        --user) account=$1 ;; --config) config=$1 ;;
        --binary) binary=$1 ;; --helper) helper=$1 ;;
      esac ;;
    --with-helper)
      [[ "$helper_mode" == keep ]] || exit 2
      helper_mode=install ;;
    --remove-helper)
      [[ "$helper_mode" == keep ]] || exit 2
      helper_mode=remove ;;
    --enable-linger) linger=1 ;;
    --dry-run) dry=1 ;;
    --yes) yes=1 ;;
    --help|-h) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

[[ "$account" =~ ^[a-z_][a-z0-9_-]{0,31}$ && "$account" != root ]] ||
  { echo "Use an existing non-root workload account" >&2; exit 2; }
[[ "$config" =~ ^/[A-Za-z0-9_./+-]+$ && "$config" != *"/../"* ]] ||
  { echo "Config must be a safe absolute path" >&2; exit 2; }
[[ "$binary" = /* && -f "$binary" && ! -L "$binary" ]] ||
  { echo "A release binary is required" >&2; exit 2; }
[[ "$helper_mode" != install || ( "$helper" = /* && -f "$helper" && ! -L "$helper" ) ]] ||
  { echo "--with-helper requires an explicit helper binary" >&2; exit 2; }

# Adjacent checksums check local bundle integrity, not publisher authenticity.
check_binary() {
  local input=$1 expected filename actual base
  base=$(basename -- "$input")
  [[ -f "$input.sha256" && ! -L "$input.sha256" ]] || {
    echo "Missing adjacent checksum: $input.sha256" >&2; exit 1;
  }
  read -r expected filename < "$input.sha256" || true
  [[ "$filename" == "$base" && "$expected" =~ ^[a-fA-F0-9]{64}$ ]] ||
    { echo "Unexpected checksum contents" >&2; exit 1; }
  actual=$(sha256sum -- "$input" | cut -d' ' -f1)
  [[ "$actual" == "$expected" ]] || { echo "Checksum mismatch" >&2; exit 1; }
  printf '%s' "$actual"
}
sha=$(check_binary "$binary")
helper_sha=none
if [[ "$helper_mode" == install ]]; then helper_sha=$(check_binary "$helper"); fi

uid=$(id -u "$account" 2>/dev/null) || { echo "Missing account" >&2; exit 1; }
gid=$(id -g "$account")
group=$(id -gn "$account")
home=$(getent passwd "$account" | cut -d: -f6)
[[ "$uid" =~ ^[0-9]+$ && "$uid" != 0 && "$gid" =~ ^[0-9]+$ &&
   "$group" =~ ^[a-z_][a-z0-9_-]{0,31}$ &&
   "$home" =~ ^/[A-Za-z0-9_./+-]+$ ]] ||
  { echo "Invalid account identity or home directory" >&2; exit 1; }
[[ -f "$config" && ! -L "$config" && $(stat -c %u -- "$config") == "$uid" ]] ||
  { echo "Private workload-owned TOML configuration is required" >&2; exit 1; }
mode=$(stat -c %a -- "$config")
[[ "$mode" =~ ^[0-7]{3,4}$ ]] && (( (8#$mode & 077) == 0 )) ||
  { echo "Configuration must not be readable by group or others" >&2; exit 1; }

service=/etc/systemd/system/maestro.service
if [[ -e "$service" ]]; then
  [[ -f "$service" && ! -L "$service" ]] || { echo "Unsafe existing service" >&2; exit 1; }
  grep -Fq '# Managed by Maestro Linux installer' "$service" ||
    { echo "Refusing to overwrite a foreign maestro.service" >&2; exit 1; }
fi

cat <<PLAN
Maestro native Linux installation plan
  Workload user: $account (UID $uid, GID $gid)
  Non-root service: /usr/local/bin/maestro --config $config --linux-mode serve
  Binary SHA256: $sha
  Helper action: $helper_mode
  Helper SHA256: $helper_sha
  Fixed root helper path: /usr/local/libexec/maestro-admin
  Root policy: /etc/maestro/admin-policy.json (default denies all protected writes)
  Exact sudoers rule: /etc/sudoers.d/maestro
  Enable lingering: $linger
  Existing config, data, user/groups and native packages are not modified.
  No host package setup, daemon groups or backend initialization is automatic.
PLAN
if ((dry)); then exit 0; fi
((EUID == 0)) || { echo "Apply must run as root" >&2; exit 1; }
if ((!yes)); then
  read -r -p "Apply this plan? [y/N] " approval || exit 1
  [[ "$approval" == y || "$approval" == Y ]] || { echo "Cancelled"; exit 1; }
fi

# All target directories must be fixed, root-owned and non-writable by others.
safe_directory() {
  local path=$1 owner mode
  [[ -d "$path" && ! -L "$path" ]] || { echo "Unsafe directory $path" >&2; exit 1; }
  owner=$(stat -c %u -- "$path")
  mode=$(stat -c %a -- "$path")
  [[ "$owner" == 0 && "$mode" =~ ^[0-7]{3,4}$ ]] &&
    (( (8#$mode & 022) == 0 )) || { echo "Untrusted directory $path" >&2; exit 1; }
}
for dir in / /usr /usr/local /etc /etc/systemd /etc/systemd/system; do
  safe_directory "$dir"
done
install -d -o root -g root -m 0755 /usr/local/bin /usr/local/libexec
safe_directory /usr/local/bin
safe_directory /usr/local/libexec

installed=/usr/local/bin/maestro
old_bin=$(mktemp /usr/local/bin/.maestro-prev.XXXXXX)
new_bin=$(mktemp /usr/local/bin/.maestro-next.XXXXXX)
old_service=$(mktemp /etc/systemd/system/.maestro-prev.XXXXXX)
new_service=$(mktemp /etc/systemd/system/.maestro-next.XXXXXX)
had_bin=0 had_service=0
cleanup() { rm -f -- "$old_bin" "$new_bin" "$old_service" "$new_service"; }
trap cleanup EXIT
if [[ -e "$installed" ]]; then
  [[ -f "$installed" && ! -L "$installed" ]] || { echo "Unsafe binary target" >&2; exit 1; }
  cp -p -- "$installed" "$old_bin"
  had_bin=1
fi
if [[ -e "$service" ]]; then cp -p -- "$service" "$old_service"; had_service=1; fi
install -o root -g root -m 0755 -- "$binary" "$new_bin"
cat > "$new_service" <<SERVICE
# Managed by Maestro Linux installer
[Unit]
Description=Maestro Linux host manager
After=network-online.target
Wants=network-online.target
[Service]
Type=simple
User=$account
Group=$group
WorkingDirectory=$home
Environment=HOME=$home
ExecStart=/usr/local/bin/maestro --config $config --linux-mode serve
Restart=on-failure
RestartSec=5s
# The restricted optional sudo -n helper requires setuid escalation.
# NoNewPrivileges=yes and an empty capability bounding set would break it.
# Main Maestro still runs under User=/Group= with no ambient privileges.
NoNewPrivileges=no
UMask=0077
[Install]
WantedBy=multi-user.target
SERVICE
chmod 0644 "$new_service"
mv -f -- "$new_bin" "$installed"
mv -f -- "$new_service" "$service"
systemctl daemon-reload
if ! systemctl enable maestro.service >/dev/null || ! systemctl restart maestro.service; then
  echo "Service start failed; rolling back binary and unit" >&2
  if ((had_bin)); then cp -p -- "$old_bin" "$installed"; else rm -f -- "$installed"; fi
  if ((had_service)); then cp -p -- "$old_service" "$service"; else rm -f -- "$service"; fi
  systemctl daemon-reload || true
  if ((had_bin && had_service)); then systemctl restart maestro.service || true; fi
  exit 1
fi

if ((linger)); then loginctl enable-linger "$account"; fi

if [[ "$helper_mode" == install ]]; then
  command -v visudo >/dev/null || { echo "visudo required for helper" >&2; exit 1; }
  install -d -o root -g root -m 0755 /etc/maestro /etc/sudoers.d
  safe_directory /etc/maestro
  safe_directory /etc/sudoers.d
  policy=/etc/maestro/admin-policy.json
  if [[ ! -e "$policy" ]]; then
    staged_policy=$(mktemp /etc/maestro/.maestro-policy.XXXXXX)
    printf '{"version":1,"workload_uid":%s,"system_units":[],"package_writes":false,"samba_password":false}\n' "$uid" > "$staged_policy"
    chmod 0600 "$staged_policy"
    mv -n -- "$staged_policy" "$policy"
  fi
  [[ -f "$policy" && ! -L "$policy" && $(stat -c %u -- "$policy") == 0 ]] ||
    { echo "Root policy is unsafe" >&2; exit 1; }
  policy_mode=$(stat -c %a -- "$policy")
  (( (8#$policy_mode & 077) == 0 )) || { echo "Root policy must be private" >&2; exit 1; }
  rule=/etc/sudoers.d/maestro
  if [[ -e "$rule" ]]; then
    [[ -f "$rule" && ! -L "$rule" ]] || { echo "Unsafe sudoers path" >&2; exit 1; }
    grep -Fq '/usr/local/libexec/maestro-admin ""' "$rule" ||
      { echo "Refusing foreign sudoers rule" >&2; exit 1; }
  fi
  staged_rule=$(mktemp /etc/sudoers.d/.maestro-rule.XXXXXX)
  printf '%s ALL=(root) NOPASSWD: NOSETENV: /usr/local/libexec/maestro-admin ""\n' "$account" > "$staged_rule"
  chmod 0440 "$staged_rule"
  visudo -cf "$staged_rule" >/dev/null
  staged_helper=$(mktemp /usr/local/libexec/.maestro-admin.XXXXXX)
  install -o root -g root -m 0755 -- "$helper" "$staged_helper"
  mv -f -- "$staged_helper" /usr/local/libexec/maestro-admin
  mv -f -- "$staged_rule" "$rule"
  echo "Helper installed, but its root-owned policy denies protected writes by default."
elif [[ "$helper_mode" == remove ]]; then
  if [[ -e /etc/sudoers.d/maestro ]]; then
    [[ -f /etc/sudoers.d/maestro && ! -L /etc/sudoers.d/maestro ]] || exit 1
    rm -f -- /etc/sudoers.d/maestro
  fi
  if [[ -e /usr/local/libexec/maestro-admin ]]; then
    [[ -f /usr/local/libexec/maestro-admin && ! -L /usr/local/libexec/maestro-admin ]] || exit 1
    rm -f -- /usr/local/libexec/maestro-admin
  fi
  echo "Helper executable and sudo authorization removed; private policy retained."
fi
echo "Non-root Maestro installed. Complete disposable guest acceptance before production use."

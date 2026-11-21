#!/usr/bin/env bash
# Verify chroot preparation/cleanup and deferred package startup without privileges.
set -euo pipefail
REPO_DIR=$(CDPATH= cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
mkdir -p "$TMP/bin" "$TMP/target/etc" "$TMP/repository"
touch "$TMP/target/etc/alpine-release"
export PROVISION_CALLS="$TMP/calls"
cat > "$TMP/bin/mount" <<'EOF'
#!/bin/sh
for destination do :; done
printf 'mount %s\n' "$destination" >> "$PROVISION_CALLS"
[ "$destination" != "${FAIL_MOUNT:-}" ]
EOF
cat > "$TMP/bin/umount" <<'EOF'
#!/bin/sh
printf 'umount %s\n' "$1" >> "$PROVISION_CALLS"
EOF
cat > "$TMP/bin/chroot" <<'EOF'
#!/bin/sh
printf 'chroot\n' >> "$PROVISION_CALLS"
printf '%s\n' "$@" > "$CHROOT_ARGS"
exit "${CHROOT_STATUS:-0}"
EOF
chmod +x "$TMP/bin/"*
export PATH="$TMP/bin:$PATH" CHROOT_ARGS="$TMP/chroot-args"
# Alpine may derive /router-hub and fsck options from the live ISO's share.
printf '%s\n' '/dev/vda3 / ext4 defaults 0 1' \
    'router-hub /router-hub 9p defaults 0 2' \
    'router-hub /mnt/router-hub 9p noauto 0 0' \
    'other-share /mnt/other 9p ro 0 0' > "$TMP/fstab"
sh "$REPO_DIR/lab/aarch64/configure-share.sh" "$TMP/fstab" "$TMP/modules"
cp "$TMP/fstab" "$TMP/expected-fstab"
sh "$REPO_DIR/lab/aarch64/configure-share.sh" "$TMP/fstab" "$TMP/modules"
cmp "$TMP/expected-fstab" "$TMP/fstab"
test "$(grep -c '^router-hub ' "$TMP/fstab")" -eq 1
grep -Fx 'router-hub /mnt/router-hub 9p trans=virtio,version=9p2000.L,ro 0 0' "$TMP/fstab" >/dev/null
grep -Fx 'other-share /mnt/other 9p ro 0 0' "$TMP/fstab" >/dev/null
grep -Fx '/dev/vda3 / ext4 defaults 0 1' "$TMP/fstab" >/dev/null
grep -Fx 9pnet_virtio "$TMP/modules/router-hub.conf" >/dev/null

helper="$REPO_DIR/lab/aarch64/installer/provision-installed.sh"
sh "$helper" /dev/test-disk "$TMP/target" "$TMP/repository"
grep -Fx LAB_OFFLINE=1 "$CHROOT_ARGS" >/dev/null
grep -Fx ROUTER_HUB_DEFER_START=1 "$CHROOT_ARGS" >/dev/null
grep -Fx /mnt/router-hub/lab/aarch64/provision.sh "$CHROOT_ARGS" >/dev/null
printf 'umount %s\n' "$TMP/target/mnt/router-hub" "$TMP/target/dev" \
    "$TMP/target/sys" "$TMP/target/proc" "$TMP/target/tmp" "$TMP/target/run" "$TMP/target" > "$TMP/expected-cleanup"
grep '^umount ' "$PROVISION_CALLS" > "$TMP/cleanup"
cmp "$TMP/expected-cleanup" "$TMP/cleanup"

# Failures in provisioning must propagate, while still releasing every mount.
: > "$PROVISION_CALLS"
if CHROOT_STATUS=42 sh "$helper" /dev/test-disk "$TMP/target" "$TMP/repository"; then
    echo 'Chroot provisioning failure was hidden' >&2
    exit 1
else
    test "$?" -eq 42
fi
grep '^umount ' "$PROVISION_CALLS" > "$TMP/cleanup"
cmp "$TMP/expected-cleanup" "$TMP/cleanup"

# Partial mount failures release only mounts successfully created by this run.
: > "$PROVISION_CALLS"
if FAIL_MOUNT="$TMP/target/sys" sh "$helper" /dev/test-disk "$TMP/target" "$TMP/repository"; then
    echo 'Mount failure was hidden' >&2
    exit 1
fi
! grep -Fx chroot "$PROVISION_CALLS" >/dev/null
printf 'umount %s\n' "$TMP/target/proc" "$TMP/target/tmp" "$TMP/target/run" "$TMP/target" > "$TMP/expected-partial-cleanup"
grep '^umount ' "$PROVISION_CALLS" > "$TMP/cleanup"
cmp "$TMP/expected-partial-cleanup" "$TMP/cleanup"

# Exercise the actual package setup flow against a disposable filesystem.
mkdir -p "$TMP/root/opt/bin" "$TMP/root/opt/etc/init.d" "$TMP/root/opt/etc/router-hub" "$TMP/root/jffs/scripts" "$TMP/examples"
cat > "$TMP/bin/id" <<'EOF'
#!/bin/sh
echo 0
EOF
cat > "$TMP/root/opt/bin/router-hub" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$SETUP_COMMANDS"
EOF
cat > "$TMP/root/opt/etc/init.d/S99router-hub" <<'EOF'
#!/bin/sh
printf '%s\n' "$1" >> "$SERVICE_COMMANDS"
EOF
chmod +x "$TMP/bin/id" "$TMP/root/opt/bin/router-hub" "$TMP/root/opt/etc/init.d/S99router-hub"
printf 'existing configuration\n' > "$TMP/root/opt/etc/router-hub/router-hub.toml"
printf '{}\n' > "$TMP/examples/firewall-policy.example.json"
sed -e "s|/opt/|$TMP/root/opt/|g" -e "s|/jffs/|$TMP/root/jffs/|g" \
    "$REPO_DIR/scripts/setup-merlin.sh" > "$TMP/setup-merlin.sh"
export SETUP_COMMANDS="$TMP/setup-commands" SERVICE_COMMANDS="$TMP/service-commands"
ROUTER_HUB_DEFER_START=1 sh "$TMP/setup-merlin.sh" "$TMP/examples" >/dev/null
test ! -f "$SERVICE_COMMANDS"
grep -F 'migrate-nginx-inputs' "$SETUP_COMMANDS" >/dev/null
grep -F 'init-nginx' "$SETUP_COMMANDS" >/dev/null
grep -F 'check-config' "$SETUP_COMMANDS" >/dev/null
test -f "$TMP/root/jffs/scripts/post-mount"
test -f "$TMP/root/jffs/scripts/firewall-start"
ROUTER_HUB_DEFER_START=0 sh "$TMP/setup-merlin.sh" "$TMP/examples" >/dev/null
grep -F 'update-dehydrated' "$SETUP_COMMANDS" >/dev/null
printf 'stop\nrestart\n' > "$TMP/expected-service-commands"
cmp "$TMP/expected-service-commands" "$SERVICE_COMMANDS"

# Preserve shell customizations and show current credentials once per interactive shell.
mkdir -p "$TMP/shell-root/root" "$TMP/shell-root/opt/etc/router-hub"
printf '# existing shell settings\n' > "$TMP/shell-root/root/.bashrc"
sh "$REPO_DIR/lab/aarch64/configure-shell.sh" "$TMP/shell-root"
cp "$TMP/shell-root/root/.bashrc" "$TMP/expected-bashrc"
sh "$REPO_DIR/lab/aarch64/configure-shell.sh" "$TMP/shell-root"
cmp "$TMP/expected-bashrc" "$TMP/shell-root/root/.bashrc"
grep -Fx '# existing shell settings' "$TMP/shell-root/root/.bashrc" >/dev/null
printf 'user=lab\npassword=test-password\n' > "$TMP/shell-root/etc/router-lab/httpd-credentials.txt"
printf 'auth_token=test-token\n' > "$TMP/shell-root/opt/etc/router-hub/auth-token.txt"
sed -e "s|/etc/router-lab/|$TMP/shell-root/etc/router-lab/|g" \
    -e "s|/opt/etc/router-hub/|$TMP/shell-root/opt/etc/router-hub/|g" \
    "$REPO_DIR/lab/aarch64/files/login-banner.sh" > "$TMP/test-banner"
sed "s|/etc/router-lab/login-banner.sh|$TMP/test-banner|" \
    "$TMP/shell-root/root/.bashrc" > "$TMP/test-bashrc"
bash --noprofile --rcfile "$TMP/test-bashrc" -i -c '. "$1"' bash "$TMP/test-banner" \
    > "$TMP/banner-output" 2> "$TMP/banner-stderr"
test "$(grep -c '^ASUS httpd Basic Auth:' "$TMP/banner-output")" -eq 1
grep -F 'password=test-password' "$TMP/banner-output" >/dev/null
grep -F 'auth_token=test-token' "$TMP/banner-output" >/dev/null
grep -F 'http://127.0.0.1:8080/user20.asp' "$TMP/banner-output" >/dev/null
bash -c '. "$1"' bash "$TMP/test-banner" > "$TMP/noninteractive-output"
test ! -s "$TMP/noninteractive-output"
printf 'auth_token=updated-token\n' > "$TMP/shell-root/opt/etc/router-hub/auth-token.txt"
sh -i -c '. "$1"' sh "$TMP/test-banner" > "$TMP/ash-output" 2> "$TMP/ash-stderr"
grep -F 'auth_token=updated-token' "$TMP/ash-output" >/dev/null

# Exercise the ISO orchestration with mocked system commands. Installation must
# use its own target, leaving the live project share outside recursive cleanup.
mkdir -p "$TMP/install-repo/lab/aarch64/installer"
printf '' > "$TMP/console"
cp "$REPO_DIR/lab/aarch64/installer/inittab" "$TMP/inittab"
cat > "$TMP/bin/awk" <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$TMP/bin/setup-alpine" <<'EOF'
#!/bin/sh
test "${SYSROOT:-/mnt}" = /mnt/router-lab || exit 41
test "${ERASE_DISKS:-}" = /dev/vda || exit 42
case /mnt/router-hub in
    "${SYSROOT:-/mnt}"/*) exit 43 ;;
esac
printf 'install\n' >> "$INSTALLER_CALLS"
EOF
cat > "$TMP/install-repo/lab/aarch64/installer/provision-installed.sh" <<'EOF'
printf 'provision\n' >> "$INSTALLER_CALLS"
EOF
cat > "$TMP/bin/poweroff" <<'EOF'
#!/bin/sh
printf 'poweroff\n' >> "$INSTALLER_CALLS"
EOF
printf '#!/bin/sh\nexit 0\n' > "$TMP/bin/modprobe"
chmod +x "$TMP/bin/awk" "$TMP/bin/setup-alpine" "$TMP/bin/poweroff" "$TMP/bin/modprobe"
sed -e "s|/dev/ttyAMA0|$TMP/console|g" -e "s|/etc/inittab|$TMP/inittab|g" \
    -e "s|/mnt/router-hub|$TMP/install-repo|g" \
    "$REPO_DIR/lab/aarch64/installer/install.sh" > "$TMP/install.sh"
export INSTALLER_CALLS="$TMP/installer-calls"
sh "$TMP/install.sh"
printf 'install\nprovision\npoweroff\n' > "$TMP/expected-installer-calls"
cmp "$TMP/expected-installer-calls" "$INSTALLER_CALLS"
echo 'Installed-disk provisioning and deferred startup checks passed.'

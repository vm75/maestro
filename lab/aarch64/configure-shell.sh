#!/bin/sh
# Install root's lab shell settings; the optional root is for isolated validation.
set -eu
SCRIPT_DIR=$(CDPATH= cd "$(dirname "$0")" && pwd)
TARGET=${1:-}
mkdir -p "$TARGET/root" "$TARGET/etc/router-lab" "$TARGET/etc/profile.d"
cp "$SCRIPT_DIR/files/login-banner.sh" "$TARGET/etc/router-lab/login-banner.sh"
chmod 0600 "$TARGET/etc/router-lab/login-banner.sh"
for line in 'export PATH=/opt/sbin:/opt/bin:$PATH' '. /etc/router-lab/login-banner.sh'; do
    if ! grep -qxF "$line" "$TARGET/root/.bashrc" 2>/dev/null; then
        printf '\n%s\n' "$line" >> "$TARGET/root/.bashrc"
    fi
done
chmod 0600 "$TARGET/root/.bashrc"
# Alpine defaults to ash, which reads /etc/profile.d rather than .bashrc.
cat > "$TARGET/etc/profile.d/router-lab.sh" <<'EOF'
if [ "$(id -u)" = 0 ] && [ -r /etc/router-lab/login-banner.sh ]; then
    . /etc/router-lab/login-banner.sh
fi
EOF
chmod 0644 "$TARGET/etc/profile.d/router-lab.sh"

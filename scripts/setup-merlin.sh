#!/bin/sh
# Shared first-install/upgrade configuration for tarball and Entware IPK installs.
set -eu

EXAMPLES_DIR="${1:?Usage: setup-merlin.sh EXAMPLES_DIR}"
CONFIG_DIR="/opt/etc/router-hub"
DATA_DIR="/opt/var/lib/router-hub"
MOSQUITTO_USER="${MOSQUITTO_USER:-}"
MOSQUITTO_PASS="${MOSQUITTO_PASS:-${MOSQUITTO_PASSWD:-}}"

[ "$(id -u)" = "0" ] || { echo "Run as root on the router." >&2; exit 1; }
[ -x /opt/bin/router-hub ] || { echo "/opt/bin/router-hub not installed" >&2; exit 1; }
mkdir -p /opt/etc/init.d "$CONFIG_DIR" "$DATA_DIR" /opt/var/log/nginx /opt/var/run/router-hub
if [ -x /opt/etc/init.d/S99router-hub ]; then
    /opt/etc/init.d/S99router-hub stop
fi

if [ ! -f "$CONFIG_DIR/router-hub.toml" ]; then
    cp "$EXAMPLES_DIR/router-hub.example.toml" "$CONFIG_DIR/router-hub.toml"
    chmod 0600 "$CONFIG_DIR/router-hub.toml"
    TOKEN="$(openssl rand -hex 24)"
    sed -i "s/REPLACE-WITH-AT-LEAST-24-RANDOM-CHARACTERS/$TOKEN/" "$CONFIG_DIR/router-hub.toml"
    echo "Created $CONFIG_DIR/router-hub.toml. Review rendered_page and menu_tree before starting."
fi

# Relocate current-format inputs while Router Hub is stopped.
/opt/bin/router-hub --config "$CONFIG_DIR/router-hub.toml" migrate-nginx-inputs

# Uses only embedded current Jinja policies and preserves existing runtime files.
# The command preserves an existing nginx.conf and respects configured nginx paths.
/opt/bin/router-hub --config "$CONFIG_DIR/router-hub.toml" init-nginx

if [ -n "$MOSQUITTO_USER" ] && [ -n "$MOSQUITTO_PASS" ]; then
    mkdir -p /opt/etc/mosquitto
    HASH=$(openssl passwd -6 "$MOSQUITTO_PASS")
    printf '%s:%s\n' "$MOSQUITTO_USER" "$HASH" > /opt/etc/mosquitto/passwd
    chmod 0644 /opt/etc/mosquitto/passwd
    echo "Configured Mosquitto password for user '$MOSQUITTO_USER' in /opt/etc/mosquitto/passwd."
elif [ -n "$MOSQUITTO_USER" ] || [ -n "$MOSQUITTO_PASS" ]; then
    echo "Warning: Both --mosquitto-user and --mosquitto-pass (or MOSQUITTO_USER and MOSQUITTO_PASS env vars) are required." >&2
fi
# Repair permissions on existing installations as well as new ones.
chmod 0600 "$CONFIG_DIR/router-hub.toml"
if [ -d /opt/etc/nginx/certs ]; then
    find /opt/etc/nginx/certs -type f -name 'privkey*.pem' -exec chmod 0644 {} \; # read access needed for nobody
fi
if [ ! -f "$CONFIG_DIR/firewall-policy.json" ] && [ -f "$EXAMPLES_DIR/firewall-policy.example.json" ]; then
    cp "$EXAMPLES_DIR/firewall-policy.example.json" "$CONFIG_DIR/firewall-policy.json"
fi
if [ ! -f "$DATA_DIR/firewall-policy.json" ] && [ -f "$EXAMPLES_DIR/firewall-policy.example.json" ]; then
    cp "$EXAMPLES_DIR/firewall-policy.example.json" "$DATA_DIR/firewall-policy.json"
fi
chmod 0600 "$CONFIG_DIR/firewall-policy.json" "$DATA_DIR/firewall-policy.json" 2>/dev/null || true

POST_MOUNT=/jffs/scripts/post-mount
if [ ! -f "$POST_MOUNT" ]; then
    printf '#!/bin/sh\n' >"$POST_MOUNT"
    chmod 0755 "$POST_MOUNT"
fi
if ! grep -qF 'router-hub firewall reconciliation' "$POST_MOUNT"; then
    cat >> "$POST_MOUNT" <<'EOF'

# Router Hub: firewall reconciliation
/opt/etc/init.d/S99router-hub reconcile
EOF
fi

FIREWALL_START=/jffs/scripts/firewall-start
if [ ! -f "$FIREWALL_START" ]; then
    printf '#!/bin/sh\n' >"$FIREWALL_START"
    chmod 0755 "$FIREWALL_START"
fi
if ! grep -qF 'Router Hub: restore firewall hooks after Asuswrt rebuilds its firewall' "$FIREWALL_START"; then
    cat >> "$FIREWALL_START" <<'EOF'

# Router Hub: restore firewall hooks after Asuswrt rebuilds its firewall
(
    attempt=0

    while [ "$attempt" -lt 30 ]; do
        if /opt/etc/init.d/S99router-hub reconcile; then
            logger -t router-hub \
                "firewall-start reconciliation completed"
            exit 0
        fi

        attempt=$((attempt + 1))
        sleep 1
    done

    logger -t router-hub \
        "firewall-start reconciliation failed after 30 attempts"
) &
EOF
fi

/opt/bin/router-hub --config "$CONFIG_DIR/router-hub.toml" check-config
/opt/etc/init.d/S99router-hub restart
printf '\nRouter Hub installed. Use the ASUS menu entry, or open http://<router-ip>:3030/#token=<auth-token>.\n'

#!/bin/sh
# Shared first-install/upgrade configuration for tarball and Entware IPK installs.
set -eu
PATH=/opt/bin:/opt/sbin:/usr/sbin:/usr/bin:/sbin:/bin
export PATH

EXAMPLES_DIR="${1:?Usage: setup-merlin.sh EXAMPLES_DIR}"
CONFIG_DIR="/opt/etc/maestro"
DATA_DIR="/opt/var/lib/maestro"
MOSQUITTO_USER="${MOSQUITTO_USER:-}"
MOSQUITTO_PASS="${MOSQUITTO_PASS:-${MOSQUITTO_PASSWD:-}}"

# The lab installer configures the installed disk in a chroot before first boot.
# In that mode, complete setup while leaving all service lifecycle calls to boot.
[ "$(id -u)" = "0" ] || { echo "Run as root on the router." >&2; exit 1; }
[ -x /opt/bin/maestro ] || { echo "/opt/bin/maestro not installed" >&2; exit 1; }
mkdir -p /opt/etc/init.d "$CONFIG_DIR" "$DATA_DIR" "$DATA_DIR/dehydrated" /opt/var/log/nginx /opt/var/run/maestro
if [ "${MAESTRO_DEFER_START:-0}" != 1 ] && [ -x /opt/etc/init.d/S99maestro ]; then
    /opt/etc/init.d/S99maestro stop
fi

# Fail closed on an existing router deployment without prepared YAML.
# This is only an installation guard, not a conversion or fallback path.
if [ ! -e "$CONFIG_DIR/maestro.yaml" ] && [ -e "$CONFIG_DIR/maestro.toml" ]; then
    echo "Prepare the router YAML offline before installing this version." >&2
    exit 1
fi
if [ ! -e "$CONFIG_DIR/maestro.yaml" ] && [ ! -L "$CONFIG_DIR/maestro.yaml" ]; then
    cp "$EXAMPLES_DIR/maestro.example.yaml" "$CONFIG_DIR/maestro.yaml"
    chmod 0600 "$CONFIG_DIR/maestro.yaml"
    TOKEN="$(openssl rand -hex 24)"
    sed -i "s/REPLACE-WITH-AT-LEAST-24-RANDOM-CHARACTERS/$TOKEN/" "$CONFIG_DIR/maestro.yaml"
    echo "Created $CONFIG_DIR/maestro.yaml. Review rendered_page and menu_tree before starting."
fi

# Validate authored YAML before changing the live nginx configuration.
/opt/bin/maestro --config "$CONFIG_DIR/maestro.yaml" check-config

# Install the embedded main configuration, backing up a different nginx.conf.
# Saved inputs, existing shared fragments, and configured nginx paths are retained.
/opt/bin/maestro --config "$CONFIG_DIR/maestro.yaml" init-nginx

# Respect the configured executable and challenge hook paths.
HOOK=$(/opt/bin/maestro --config "$CONFIG_DIR/maestro.yaml" config-value certificates.dns01_hook)
DEHYDRATED_BIN=$(/opt/bin/maestro --config "$CONFIG_DIR/maestro.yaml" config-value commands.dehydrated)
if [ ! -e "$HOOK" ] && [ ! -L "$HOOK" ]; then
    mkdir -p "$(dirname "$HOOK")"
    if [ -f "$EXAMPLES_DIR/dehydrated-dns01-hook.sh" ]; then
        install -m 0755 "$EXAMPLES_DIR/dehydrated-dns01-hook.sh" "$HOOK"
    elif [ -f "$EXAMPLES_DIR/../scripts/dehydrated-dns01-hook.sh" ]; then
        install -m 0755 "$EXAMPLES_DIR/../scripts/dehydrated-dns01-hook.sh" "$HOOK"
    fi
fi
# The application updater verifies TLS and publishes atomically.
if ! /opt/bin/maestro --config "$CONFIG_DIR/maestro.yaml" update-dehydrated; then
    echo "Warning: Dehydrated update failed; retaining existing executable at $DEHYDRATED_BIN." >&2
fi

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
chmod 0600 "$CONFIG_DIR/maestro.yaml"
NGINX_ROOT=$(/opt/bin/maestro --config "$CONFIG_DIR/maestro.yaml" config-value nginx.root_dir)
if [ -n "$NGINX_ROOT" ] && [ -d "$NGINX_ROOT/certs" ]; then
    find "$NGINX_ROOT/certs" -type f -name 'privkey*.pem' -exec chmod 0600 {} \;
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
if ! grep -qF 'maestro firewall reconciliation' "$POST_MOUNT"; then
    cat >> "$POST_MOUNT" <<'EOF'

# Maestro: firewall reconciliation
/opt/etc/init.d/S99maestro reconcile
EOF
fi

FIREWALL_START=/jffs/scripts/firewall-start
if [ ! -f "$FIREWALL_START" ]; then
    printf '#!/bin/sh\n' >"$FIREWALL_START"
    chmod 0755 "$FIREWALL_START"
fi
if ! grep -qF 'Maestro: restore firewall hooks after Asuswrt rebuilds its firewall' "$FIREWALL_START"; then
    cat >> "$FIREWALL_START" <<'EOF'

# Maestro: restore firewall hooks after Asuswrt rebuilds its firewall
(
    attempt=0

    while [ "$attempt" -lt 30 ]; do
        if /opt/etc/init.d/S99maestro reconcile; then
            logger -t maestro \
                "firewall-start reconciliation completed"
            exit 0
        fi

        attempt=$((attempt + 1))
        sleep 1
    done

    logger -t maestro \
        "firewall-start reconciliation failed after 30 attempts"
) &
EOF
fi

/opt/bin/maestro --config "$CONFIG_DIR/maestro.yaml" check-config
if [ "${MAESTRO_DEFER_START:-0}" != 1 ]; then
    /opt/etc/init.d/S99maestro restart
fi
printf '\nMaestro installed. Use the ASUS menu entry, or open http://<router-ip>:3030/#token=<auth-token>.\n'

#!/bin/sh
set -eu

SCRIPT_DIR=$(dirname "$0")
REPO_DIR=$(dirname "$SCRIPT_DIR")

BINARY="./router-hub"
CONFIG_DIR="/opt/etc/router-hub"
DATA_DIR="/opt/var/lib/router-hub"
MOSQUITTO_USER="${MOSQUITTO_USER:-}"
MOSQUITTO_PASS="${MOSQUITTO_PASS:-${MOSQUITTO_PASSWD:-}}"

while [ $# -gt 0 ]; do
    case "$1" in
        --mosquitto-user)
            MOSQUITTO_USER="$2"
            shift 2
            ;;
        --mosquitto-pass|--mosquitto-passwd)
            MOSQUITTO_PASS="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [BINARY] [--mosquitto-user USER --mosquitto-pass PASS]"
            exit 0
            ;;
        *)
            if [ -f "$1" ]; then
                BINARY="$1"
                shift
            else
                echo "Unknown option or binary not found: $1" >&2
                exit 1
            fi
            ;;
    esac
done

[ "$(id -u)" = "0" ] || { echo "Run as root on the router." >&2; exit 1; }
[ -f "$BINARY" ] || { echo "Binary not found: $BINARY" >&2; exit 1; }

mkdir -p /opt/bin /opt/etc/init.d
if [ -x /opt/etc/init.d/S99router-hub ]; then
    /opt/etc/init.d/S99router-hub stop
fi
if [ "$BINARY" != "/opt/bin/router-hub" ]; then
    cp "$BINARY" /opt/bin/router-hub
fi
chmod 0755 /opt/bin/router-hub
cp "$SCRIPT_DIR/S99router-hub" /opt/etc/init.d/S99router-hub
chmod 0755 /opt/etc/init.d/S99router-hub

export MOSQUITTO_USER MOSQUITTO_PASS
exec /bin/sh "$SCRIPT_DIR/setup-merlin.sh" "$REPO_DIR/config"

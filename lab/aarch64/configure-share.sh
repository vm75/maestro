#!/bin/sh
# Persist the Maestro 9p project share; replace obsolete installer and legacy mounts.
set -eu
FSTAB=${1:-/etc/fstab}
MODULES_DIR=${2:-/etc/modules-load.d}
mkdir -p "$MODULES_DIR"
printf '%s\n' 9pnet_virtio > "$MODULES_DIR/maestro.conf"
rm -f "$MODULES_DIR/router-hub.conf"
temporary=$(mktemp "$FSTAB.XXXXXX")
trap 'rm -f "$temporary"' EXIT
awk '!($3 == "9p" && (($1 == "router-hub" && ($2 == "/router-hub" || $2 == "/mnt/router-hub")) || ($1 == "maestro" && ($2 == "/maestro" || $2 == "/mnt/maestro"))))' \
    "$FSTAB" > "$temporary"
printf '%s\n' 'maestro /mnt/maestro 9p trans=virtio,version=9p2000.L,ro 0 0' >> "$temporary"
chmod 0644 "$temporary"
mv "$temporary" "$FSTAB"

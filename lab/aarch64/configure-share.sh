#!/bin/sh
# Persist the project share, replacing entries generated from the live ISO mount.
set -eu
FSTAB=${1:-/etc/fstab}
MODULES_DIR=${2:-/etc/modules-load.d}
mkdir -p "$MODULES_DIR"
printf '%s\n' 9pnet_virtio > "$MODULES_DIR/router-hub.conf"
temporary=$(mktemp "$FSTAB.XXXXXX")
trap 'rm -f "$temporary"' EXIT
awk '!($1 == "router-hub" && $3 == "9p" && ($2 == "/router-hub" || $2 == "/mnt/router-hub"))' \
    "$FSTAB" > "$temporary"
printf '%s\n' 'router-hub /mnt/router-hub 9p trans=virtio,version=9p2000.L,ro 0 0' >> "$temporary"
chmod 0644 "$temporary"
mv "$temporary" "$FSTAB"

#!/bin/sh
# Persist only the Maestro 9p project share.
set -eu
FSTAB=${1:-/etc/fstab}
MODULES_DIR=${2:-/etc/modules-load.d}
mkdir -p "$MODULES_DIR"
printf '%s\n' 9pnet_virtio > "$MODULES_DIR/maestro.conf"
temporary=$(mktemp "$FSTAB.XXXXXX")
trap 'rm -f "$temporary"' EXIT
awk '!($3 == "9p" && $1 == "maestro" && ($2 == "/maestro" || $2 == "/mnt/maestro"))' \
    "$FSTAB" > "$temporary"
printf '%s\n' 'maestro /mnt/maestro 9p trans=virtio,version=9p2000.L,ro 0 0' >> "$temporary"
chmod 0644 "$temporary"
mv "$temporary" "$FSTAB"

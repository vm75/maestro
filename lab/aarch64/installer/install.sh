#!/bin/sh
# Runs once from init, after OpenRC and before the serial-console login prompt.
set -eu

# Never install from an already installed disk, even if this file is retained.
if ! awk '$2 == "/" && $3 == "tmpfs" { live=1 } END { exit !live }' /proc/mounts; then
    echo 'Router lab: skipping automatic installation outside the live ISO.'
    exit 0
fi

exec </dev/ttyAMA0 >/dev/ttyAMA0 2>&1
echo 'Router lab: mounting the repository and starting setup-alpine.'
modprobe 9pnet_virtio
mkdir -p /mnt/router-hub
mount -t 9p -o trans=virtio,version=9p2000.L,ro router-hub /mnt/router-hub

# Do not copy the automatic install hook into the installed system's inittab.
sed -i '\|^::wait:/bin/sh /etc/router-lab/install.sh$|d' /etc/inittab
# init.sh only attaches this overlay after creating a new disposable disk.
# Keep Alpine's recursive /mnt cleanup away from the repository mount.
SYSROOT=/mnt/router-lab ERASE_DISKS=/dev/vda setup-alpine -f /mnt/router-hub/lab/aarch64/setup-alpine.answers

echo 'Router lab: provisioning the installed disk.'
sh /mnt/router-hub/lab/aarch64/installer/provision-installed.sh

echo 'Router lab: installation and provisioning finished. Powering off now. make run-lab on the host.'
poweroff

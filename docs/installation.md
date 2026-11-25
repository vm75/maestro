# Router installation

[← Back to README](../README.md)

## Requirements and support

For a router installation:

- Asuswrt-Merlin with JFFS custom scripts/configuration enabled.
- Entware mounted at `/opt`.
- Entware GNU tar (`opkg install tar`) for backups and automatic upgrades.
- A binary built for the router CPU and libc/ABI.
- nginx, dehydrated, OpenSSL, ipset, iptables, and related utilities only for
  the features that use them.

Release tarballs target AArch64 musl and the Entware IPK has architecture
`aarch64-3.10`. Verify `opkg print-architecture` on the target router. Legacy
MIPS compatibility is
not promised; a target is supported only after testing it on the matching
router CPU and firmware.

For an installed Maestro 0.11.0 router, complete the offline upgrade preparation below before replacing the package.

## Install a published version

### Entware IPK (recommended for the router)

Stable GitHub releases include a `.ipk` and `.ipk.sha256` pair. For example,
after `v0.11.0` is published, download both assets from
[GitHub Releases](https://github.com/vm75/maestro/releases) and verify:

```sh
VERSION=0.11.0
PACKAGE="maestro_${VERSION}-1_aarch64-3.10.ipk"
BASE="https://github.com/vm75/maestro/releases/download/v${VERSION}"
cd /tmp
curl -fLO "${BASE}/${PACKAGE}"
curl -fLO "${BASE}/${PACKAGE}.sha256"
sha256sum -c "${PACKAGE}.sha256"
/opt/bin/opkg install "/tmp/${PACKAGE}"
```

Use a root shell. The package owns the Maestro binary, Entware init script,
example configuration, and shared setup helper, but **not** runtime settings or
state. The `postinst` hook initializes first-install Maestro YAML configuration, installs JFFS reconciliation hooks,
validates configuration and starts the service. `prerm` stops the service
before an upgrade or removal; it does not delete persistent data.

### Upgrade from Maestro 0.11.0

The router must be prepared offline before installing the new package. Maestro
does not convert TOML at runtime or during install. The router installer refuses
to create a fresh YAML if its existing TOML configuration is still present.
Linux is a fresh installation and has no migration procedure.

1. Make an independent backup using the installed version and separately copy
   the configuration, nginx inputs and runtime tree, certificate directories,
   custom paths, JFFS hooks and installed IPK/binary. Keep the backup private.
2. Stop Maestro. Convert `/opt/etc/maestro/maestro.toml` to
   `/opt/etc/maestro/maestro.yaml` using an offline TOML reader/YAML writer or
   manual conversion against [the current example](../config/maestro.example.yaml).
   Preserve every configured value and set `platform: asuswrt`. Restrict the YAML
   to mode 0600; retain the old TOML in the independent backup.
3. Convert existing managed `nginx.toml` and `sites.toml` to the matching YAML
   mappings. Place them under the configured `nginx.inputs_dir` (default
   `/opt/etc/maestro/nginx`). If persistent render inputs are still under
   `/opt/etc/nginx/settings`, copy the authored data fragments to the input
   directory as well. Do not copy Jinja templates or remove live nginx includes.
   Compare against existing destination files and resolve conflicts manually.
   Preserve JSON state, native nginx configuration, keys and certificates.
4. Install the verified package. Use `maestro --config /opt/etc/maestro/maestro.yaml check-config`
   and `/opt/sbin/nginx -t`. Confirm configured paths and check the ASUS UI,
   nginx, certificate renewals, WireGuard, AdGuard and BanShield.
5. On failure, stop the new daemon and restore the old binary and matching
   configuration/state from the independent backup before restarting it.

Do not start a fresh configuration over an existing deployment. The installer
does not delete administrator-created state.

### Upgrade an existing YAML-based router layout

Create and copy an independent backup off the router before upgrading. Stop
Maestro and ensure no certificate operation is running before migrating.
The router installer invokes `migrate-router-layout` before nginx initialization.
For a manual binary upgrade, run:

```sh
/opt/etc/init.d/S99maestro stop
/opt/bin/maestro --config /opt/etc/maestro/maestro.yaml migrate-router-layout
/opt/bin/maestro --config /opt/etc/maestro/maestro.yaml check-config
/opt/sbin/nginx -t
/opt/etc/init.d/S99maestro start
```

The migration changes the former default data directory to
`/opt/var/lib/maestro`, preserves explicitly different data paths, consolidates
firewall policy beside the router YAML, relocates HTTP-01 recovery records, and
flattens known nested input fragments. The original YAML and retired files are
archived under the persistent data directory in `layout-v13-backup/`.
Conflicting destinations stop migration rather than overwrite existing data.
Unknown files, certificate stores, custom Nginx sites and configured command
paths are not moved. Retain the independent backup for rollback.

### Backups

Create a backup from the installed binary:

```sh
/opt/bin/maestro backup
```

On routers this writes `/tmp/maestro-backup-YYYYMMDD-HHMMSS.tar.gz` using
local time. Copy it off the router before rebooting, since `/tmp` is temporary.
On native Linux (native `version: 1` YAML), the archive is saved
alongside the configuration file selected by `--config`:

```sh
maestro --config /etc/maestro/maestro.yaml --linux-mode backup
# Optional existing destination directory on either platform:
maestro --config /etc/maestro/maestro.yaml --linux-mode backup --output-dir /srv/backups
```

The archive includes the configuration directory, running executable, configured
persistent state, nginx root and separately configured nginx inputs/includes,
certificate store, dehydrated executable, and existing `S99maestro` init script.
Router backups also include `paths.jffs_scripts_dir` (default `/jffs/scripts`),
`paths.hosts_add` and `paths.dnsmasq_conf_add`. Missing optional inputs are skipped;
an existing configuration file is required. Configured root symlink targets are
included, while symlinks inside archived directories are preserved without following
them. Archives use paths relative to `/` for manual extraction and restore.

The command uses `commands.tar` (router default `/opt/bin/tar`, native system
tar on Linux) with gzip and GNU exclusion support and a
five-minute timeout. Archives have mode `0600` because they contain tokens and
private keys. Failed archives are removed, existing archives are never overwritten,
and prior `maestro-backup-*.tar.gz` archives are excluded to prevent recursive
backups. A second backup in the same second fails rather than replacing the first.

Router backups use Entware `/opt/bin/tar`.
The router setup scripts also prefer `/opt/bin` and `/opt/sbin` for Unix tools.
`mount`, `umount`, and `ping` use `/bin`; OpenSSL uses `/usr/sbin/openssl`.
Router-specific service, NVRAM and firewall commands retain their firmware paths.
Explicit `commands` values in existing YAML take precedence; update old paths
there when upgrading.

Before using `maestro upgrade` from 0.12.0, install Entware tar and explicitly
set `commands.tar: /opt/bin/tar` in the existing YAML. The installed version
creates its backup before the new package and its dependencies are installed.

Test mode only prints the planned destination and does not create an archive.

Backups read files without stopping services or freezing writes. Avoid editing
settings or renewing certificates during a backup; stop Maestro before a manual
backup when a consistent cross-file snapshot is required. Review and restore the
needed files from an extracted archive or use the restore command below while
services are stopped.

### Restore a backup

Use a trusted backup from the same installation. Stop Maestro and any other
services whose configuration or certificates will be restored. On a router:

```sh
/opt/etc/init.d/S99maestro stop
/opt/bin/maestro restore /tmp/maestro-backup-YYYYMMDD-HHMMSS.tar.gz
# After a successful restore, validate before starting the service:
/opt/bin/maestro --config /opt/etc/maestro/maestro.yaml check-config
/opt/etc/init.d/S99maestro start
```

Replace the archive name with your actual backup. Restore does not load the
current configuration, so it also works when that file is missing or invalid.
It does not stop or start services itself. An explicit `--test-mode` validates
the archive and destination without restoring files; configuration-file
`test_mode` settings do not apply to this recovery command.

To restore into a separate tree for inspection first, create a private destination
and use `--root` on either platform:

```sh
mkdir -m 700 /tmp/maestro-recovery
maestro restore /tmp/maestro-backup-YYYYMMDD-HHMMSS.tar.gz --root /tmp/maestro-recovery
```

Paths in the archive are relative to `/`; for example `etc/maestro/maestro.yaml`
is restored beneath `/tmp/maestro-recovery/etc/maestro/maestro.yaml` in this
example. The default root is `/`, restoring original locations. Symbolic links
retain their archived targets, including absolute targets; inspection-tree links
can therefore still point outside that tree. Restore never writes through those
links. Existing symlink parents and file/directory type conflicts are rejected.

The entire archive is staged privately and checked before destination writes.
Traversal and absolute entry names, duplicate paths, device/special entries,
and writes beneath archived symlinks are rejected. Archives are limited to
100,000 entries and 2 GiB uncompressed, including headers and padding. Staging
uses the system temporary directory (`TMPDIR`, or `/tmp`). Ensure that temporary
storage and destination storage have space for the restored files.

Restore replaces archived files individually using atomic renames and restores
directory and file permission bits without setuid/setgid bits; directory sticky
bits are preserved. Files belong
to the restoring user; archived owner IDs and timestamps are not applied. Hard links
are restored as independent files with the same contents. Extra destination files
are left in place. Restore is not a transaction across the whole tree: an I/O
failure during application can leave earlier files restored. Keep services stopped
and review the error before retrying. Restoring an executable can roll back its
version; package-manager records are not rolled back by the command.

### Router-only upgrade command

```sh
/opt/bin/maestro upgrade
# Download and verify without attempting opkg installation:
/opt/bin/maestro upgrade --download-only
# Explicitly skip the pre-upgrade backup:
/opt/bin/maestro upgrade --no-backup
```

The CLI checks GitHub's **latest stable** release and compares it against its
compiled version. If newer, it downloads the matching AArch64 Entware IPK and
its SHA-256 checksum, verifies the checksum, then stages the file as
`/tmp/maestro_<version>-1_aarch64-3.10.ipk`. When called as root with an
available compatible `/opt/bin/opkg`, it replaces the one-shot upgrade process
with `opkg install` so the package lifecycle can safely stop the running Router
Hub daemon.

After verifying a newer IPK and before installing it or printing the manual
installation command, `upgrade` creates the same backup as `backup`. This also
applies to `--download-only`. A backup failure aborts the upgrade before installation;
use `--no-backup` to explicitly skip it. No backup is made when already up to date
or when release download/verification fails.

When automatic installation is unavailable (including `--download-only`),
it prints the exact manual command.
If opkg fails, the verified package remains in `/tmp`; the retry command is
printed before launching opkg. Network or checksum errors abort without
installing. During installation, `prerm` stops the running service and `postinst`
executes `init-nginx`, validates the configuration,
and restarts the service. A configuration failure stops the install
hook and leaves the service stopped for administrator review; it never deletes
existing persistent state or silently claims rollback of the installed binary.

This is a separate CLI process, **not** an authenticated API/UI action.
`upgrade` is rejected on native Linux and simulated test-mode runs. The
upgrade requires a published stable IPK; prerelease tags and older tarball-only
releases cannot be installed through this command.

### Router-only Toolbox packages

[Toolbox](https://github.com/vm75/toolbox/releases) publishes Entware IPK
bundles independently of Maestro. Maestro does not build NGINX or other
Toolbox packages.

```sh
/opt/bin/maestro toolbox list
/opt/bin/maestro toolbox install toolbox-nginx-entware-1.30.8-99_aarch64
/opt/bin/maestro toolbox install toolbox-nginx-entware-1.30.8-99_aarch64 --download-only
```

List shows published, non-prerelease Entware releases containing a complete
compatible `aarch64-3.10` IPK bundle and SHA256SUMS, up to ten API pages.
Android releases and incompatible or mixed-architecture bundles are omitted.
Select the exact tag shown; no tool-specific build or version selector is needed.
The old `upgrade-nginx` / `update-nginx` commands are replaced by Toolbox.

Installation verifies every IPK against the release SHA256SUMS manifest and
the GitHub asset digest when provided. No packages are installed until the
whole bundle is staged in a unique private directory under /tmp. A full Maestro
backup is made by default; `--no-backup` skips it. Selection explicitly
reinstalls same-version packages, while downgrades require
`--allow-downgrade`. One opkg invocation receives every IPK, and Maestro checks
the resulting installed package versions. Non-root, missing-opkg and
`--download-only` runs print a manual command instead. Installation is
CLI-only and unavailable on Linux or in test mode.

For NGINX, the four matching packages are required (nginx-ssl,
nginx-mod-brotli, nginx-mod-stream and nginx-mod-headers-more). Automatic
installation retains pre/post configuration tests, a separate binary/module
recovery snapshot and live binary activation using SIGUSR2 and SIGQUIT.
Toolbox publishes IPK checksums rather than standalone executable/module
checksums, so verification uses the package hashes, installed package versions,
configuration test and verified new master identity. A stopped NGINX remains
stopped unless its package hooks start it. Inspect the printed recovery snapshot
on error; after SIGUSR2, automatic rollback is unsafe. An opkg failure may
leave package records or other packages partially changed; file rollback does
not roll back package metadata. For other tools, normal opkg package hooks
handle service lifecycle, and no automatic package rollback is promised.
Keep verified packages and backups outside volatile /tmp for recovery.

Manual NGINX installation bypasses Maestro's automatic activation and snapshot
logic; test the configuration and activate the new binary separately.

### Tarball installation (alternative)

On the router, ensure `curl`, `tar`, and `sha256sum` are available. You can
instead download and verify on another Linux machine, then transfer the
extracted directory to the router. For example, **after** `v0.8.1` has been
published:

```sh
VERSION=v0.10.1
TARGET=aarch64-unknown-linux-musl
PACKAGE="maestro-${VERSION}-${TARGET}"
URL="https://github.com/vm75/maestro/releases/download/${VERSION}"

curl -fLO "${URL}/${PACKAGE}.tar.gz"
curl -fLO "${URL}/${PACKAGE}.tar.gz.sha256"
sha256sum -c "${PACKAGE}.tar.gz.sha256"
tar -xzf "${PACKAGE}.tar.gz"
cd "${PACKAGE}"
# Run as root on the router:
./scripts/install-merlin.sh ./maestro
```

The package includes the executable, `config/`, `scripts/`, and release
documentation. Back up the router's configuration and persistent data before
upgrading; the installer preserves existing `maestro.yaml` and should be
reviewed for your environment. AArch64 musl compatibility requires validation
on your router CPU and firmware.

## Build and install from source

Build the release artifact for the target CPU. The helper also writes a SHA-256
file under `dist/`:

```sh
./scripts/build-release.sh aarch64-unknown-linux-musl
```

Alternatively, build and deploy to a remote target in one step using:

```sh
./scripts/build-and-deploy.sh user@router-host
```

Or copy the resulting binary, `config/`, and `scripts/` to the router, then run
the installer as root from that copied repository:

```sh
./scripts/install-merlin.sh ./maestro-aarch64-unknown-linux-musl --mosquitto-user admin --mosquitto-pass mysecretpassword
```

The installer:

1. installs the binary as `/opt/bin/maestro` and the Entware init script as
   `/opt/etc/init.d/S99maestro`;
2. creates `/opt/etc/maestro/maestro.yaml` and a random authentication
   token on first install;
3. installs Maestro's main nginx configuration, saving a different existing
   file as `nginx.conf.before-maestro-<unique-id>` and printing its path;
   preserves saved inputs and existing shared/custom files, creates missing
   bundled files and runtime/data directories;
4. configures Mosquitto credentials in `/opt/etc/mosquitto/passwd` if `--mosquitto-user` and `--mosquitto-pass` (or `MOSQUITTO_USER` / `MOSQUITTO_PASS` env vars) are supplied;
5. adds token-free reconciliation calls to `/jffs/scripts/post-mount` and
   `/jffs/scripts/firewall-start` (with a background retry loop);
6. validates the configuration and restarts Maestro.

Review the generated YAML before starting. In particular, set the correct
Asuswrt extension page (`user1.asp` through `user20.asp`), `menu_tree`,
`menu_index`, nginx paths, certificate paths, bind address, and allowed
origins. The installer’s default paths are examples for a typical Entware
layout, not proof that every firmware image uses the same layout.

The service supports the usual Entware init actions:

```sh
/opt/etc/init.d/S99maestro start
/opt/etc/init.d/S99maestro stop
/opt/etc/init.d/S99maestro restart
/opt/etc/init.d/S99maestro status
```

`reconcile` requests an immediate firewall reconciliation from the running
process and is used by the installed `post-mount` and `firewall-start` hooks.


## Guided native Linux backend setup

The Linux release installer supports explicit backend preparation in addition to its minimal deployment mode:

```bash
sudo ./scripts/install-linux.sh --user YOUR_ACCOUNT --config /home/YOUR_ACCOUNT/.config/maestro/maestro.yaml --binary /absolute/path/maestro --initialize-config --backends all --dry-run
```

Review the plan and repeat without `--dry-run` to apply. Select `podman,docker,virtualization,incus,samba,nfs` individually or `all`. Omitting `--backends` preserves the minimal deployment behavior. `--skip-packages` verifies existing native packages without upgrading them. On Ubuntu/Debian the installer refreshes APT and installs native packages; on Arch it performs a synchronized full upgrade. It adds required Docker, libvirt and Incus groups, starts their socket/service units, enables lingering and prepares the workload user's D-Bus socket before Maestro starts. No external repositories are added. Docker and Incus group membership confers broad host control. Samba/NFS publication and Incus initialization remain explicit. The helper is independently optional (`--with-helper --helper /absolute/path/maestro-admin`); root-owned grants still default to deny. Package, group and daemon changes are not rolled back after later failures.

### Restore a portable router backup

New router backups contain a versioned manifest for /opt and /jffs. Stop
services and mount Entware on the destination before restoring. Restore maps
the archived physical roots to the destination's existing logical mounts.
When a mount moves, external archived paths must be explicitly mapped (including
identity mappings if they should stay in the original location):

```sh
maestro restore /tmp/maestro-backup.tar.gz --test-mode \\
  --map /mnt/old-certs=/mnt/new-certs
maestro restore /tmp/maestro-backup.tar.gz \\
  --map /mnt/old-certs=/mnt/new-certs
```

Mappings are absolute source and logical destination prefixes. Validate the
archive first with test mode. Explicitly mapped symlink targets are rewritten
relative to the restored files. Old archives retain their original restore
semantics. Review physical paths embedded in authored YAML and native NGINX
configuration manually before starting services; arbitrary file contents are
not rewritten.

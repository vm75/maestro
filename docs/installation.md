# Router installation

[← Back to README](../README.md)

## Requirements and support

For a router installation:

- Asuswrt-Merlin with JFFS custom scripts/configuration enabled.
- Entware mounted at `/opt`.
- A binary built for the router CPU and libc/ABI.
- nginx, dehydrated, OpenSSL, ipset, iptables, and related utilities only for
  the features that use them.

Release tarballs target AArch64 musl and the Entware IPK has architecture
`aarch64-3.10`. Verify `opkg print-architecture` on the target router. Legacy
MIPS compatibility is
not promised; a target is supported only after testing it on the matching
router CPU and firmware.

For older installations, read the [migration guide](migration.md) before upgrading.

## Install a published version

### Entware IPK (recommended for the router)

Stable GitHub releases include a `.ipk` and `.ipk.sha256` pair. For example,
after `v0.10.2` is published, download both assets from
[GitHub Releases](https://github.com/vm75/router-hub/releases) and verify:

```sh
VERSION=0.10.2
PACKAGE="router-hub_${VERSION}-1_aarch64-3.10.ipk"
BASE="https://github.com/vm75/router-hub/releases/download/v${VERSION}"
cd /tmp
curl -fLO "${BASE}/${PACKAGE}"
curl -fLO "${BASE}/${PACKAGE}.sha256"
sha256sum -c "${PACKAGE}.sha256"
/opt/bin/opkg install "/tmp/${PACKAGE}"
```

Use a root shell. The package owns the Router Hub binary, Entware init script,
example configuration, and shared setup helper, but **not** runtime settings or
state. The `postinst` hook initializes first-install configuration, migrates
current-format nginx inputs where needed, installs JFFS reconciliation hooks,
validates configuration and starts the service. `prerm` stops the service
before an upgrade or removal; it does not delete persistent data.

**Migrating from a tarball installation:** Make a backup and read the
[migration guide](migration.md) first. The IPK can take ownership of the existing
binary and init script. Existing `/opt/etc/router-hub` settings, certificate
files and persistent state are left intact. Removing the IPK also intentionally
leaves administrator-created configuration, data and JFFS hooks in place.

### Backups

Create a backup from the installed binary:

```sh
/opt/bin/router-hub backup
```

On routers this writes `/tmp/router-hub-backup-YYYYMMDD-HHMMSS.tar.gz` using
local time. Copy it off the router before rebooting, since `/tmp` is temporary.
On native Linux (`platform = "linux"` or `--linux-mode`), the archive is saved
alongside the configuration file selected by `--config`:

```sh
router-hub --config /etc/router-hub/router-hub.toml --linux-mode backup
# Optional existing destination directory on either platform:
router-hub --config /etc/router-hub/router-hub.toml --linux-mode backup --output-dir /srv/backups
```

The archive includes the configuration directory, running executable, configured
persistent state, nginx root and separately configured nginx inputs/includes,
certificate store, dehydrated executable, and existing `S99router-hub` init script.
Router backups also include `paths.jffs_scripts_dir` (default `/jffs/scripts`),
`paths.hosts_add` and `paths.dnsmasq_conf_add`. Missing optional inputs are skipped;
an existing configuration file is required. Configured root symlink targets are
included, while symlinks inside archived directories are preserved without following
them. Archives use paths relative to `/` for manual extraction and restore.

The command uses `commands.tar` (default `/bin/tar`) with gzip support and a
five-minute timeout. Archives have mode `0600` because they contain tokens and
private keys. Failed archives are removed, existing archives are never overwritten,
and prior `router-hub-backup-*.tar.gz` archives are excluded to prevent recursive
backups. A second backup in the same second fails rather than replacing the first.
Test mode only prints the planned destination and does not create an archive.

Backups read files without stopping services or freezing writes. Avoid editing
settings or renewing certificates during a backup; stop Router Hub before a manual
backup when a consistent cross-file snapshot is required. Review and restore the
needed files from an extracted archive or use the restore command below while
services are stopped.

### Restore a backup

Use a trusted backup from the same installation. Stop Router Hub and any other
services whose configuration or certificates will be restored. On a router:

```sh
/opt/etc/init.d/S99router-hub stop
/opt/bin/router-hub restore /tmp/router-hub-backup-YYYYMMDD-HHMMSS.tar.gz
# After a successful restore, validate before starting the service:
/opt/bin/router-hub --config /opt/etc/router-hub/router-hub.toml check-config
/opt/etc/init.d/S99router-hub start
```

Replace the archive name with your actual backup. Restore does not load the
current configuration, so it also works when that file is missing or invalid.
It does not stop or start services itself. An explicit `--test-mode` validates
the archive and destination without restoring files; configuration-file
`test_mode` settings do not apply to this recovery command.

To restore into a separate tree for inspection first, create a private destination
and use `--root` on either platform:

```sh
mkdir -m 700 /tmp/router-hub-recovery
router-hub restore /tmp/router-hub-backup-YYYYMMDD-HHMMSS.tar.gz --root /tmp/router-hub-recovery
```

Paths in the archive are relative to `/`; for example `etc/router-hub/router-hub.toml`
is restored beneath `/tmp/router-hub-recovery/etc/router-hub/router-hub.toml` in this
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
/opt/bin/router-hub upgrade
# Download and verify without attempting opkg installation:
/opt/bin/router-hub upgrade --download-only
# Explicitly skip the pre-upgrade backup:
/opt/bin/router-hub upgrade --no-backup
```

The CLI checks GitHub's **latest stable** release and compares it against its
compiled version. If newer, it downloads the matching AArch64 Entware IPK and
its SHA-256 checksum, verifies the checksum, then stages the file as
`/tmp/router-hub_<version>-1_aarch64-3.10.ipk`. When called as root with an
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
executes `migrate-nginx-inputs`, then `init-nginx`, validates the configuration,
and restarts the service. A migration or configuration failure stops the install
hook and leaves the service stopped for administrator review; it never deletes
existing persistent state or silently claims rollback of the installed binary.

This is a separate CLI process, **not** an authenticated API/UI action.
`upgrade` is rejected on native Linux and simulated test-mode runs. The
upgrade requires a published stable IPK; prerelease tags and older tarball-only
releases cannot be installed through this command.

### Router-only NGINX upgrade command

```sh
/opt/bin/router-hub upgrade-nginx
# Target mainline releases instead of stable:
/opt/bin/router-hub upgrade-nginx --channel mainline
# Check if an update is available without downloading or installing:
/opt/bin/router-hub upgrade-nginx --check
# Download and verify the 4-package bundle without attempting opkg installation:
/opt/bin/router-hub upgrade-nginx --download-only
# Explicitly skip the pre-upgrade backup:
/opt/bin/router-hub upgrade-nginx --no-backup
# Target a specific NGINX version or exact release tag:
/opt/bin/router-hub upgrade-nginx --version 1.30.7
/opt/bin/router-hub upgrade-nginx --tag nginx-v1.30.7-run44684227015-a1
# Force re-installation even if already up to date:
/opt/bin/router-hub upgrade-nginx --force
```

The CLI queries GitHub Releases for manually built Entware NGINX releases tagged
`nginx-v*` (published by the [Build Nginx workflow](../.github/workflows/build-nginx.yml)).
By default it selects the newest release in the **stable** channel (even minor
version numbers such as 1.28.x and 1.30.x); `--channel mainline` targets odd
minor versions (e.g. 1.29.x and 1.31.x).

The command downloads and verifies `SHA256SUMS` and the four version-matched
IPK packages (`nginx-ssl`, `nginx-mod-brotli`, `nginx-mod-stream`, and
`nginx-mod-headers-more`). Dynamic modules are strictly ABI-bound to their NGINX
build; installing the bundle together prevents runtime module load failures.

Before making any changes, `upgrade-nginx` tests the existing NGINX configuration
syntax (`nginx -t`) and creates a full backup (same as `router-hub backup`).
When running as root, it installs all four packages in a single `opkg install`
transaction, re-verifies configuration syntax with the newly installed binary,
and restarts the NGINX service (`/opt/etc/init.d/S80nginx restart`).

If the post-installation configuration test fails, NGINX is **not** restarted,
ensuring active worker processes continue serving traffic while the administrator
inspects syntax changes. Staged verified packages remain in `/tmp/nginx-<tag>/`
for inspection or manual retries. `update-nginx` is supported as an alias.
Like `upgrade`, this command is rejected on native Linux and simulated test-mode runs.

### Tarball installation (alternative)

On the router, ensure `curl`, `tar`, and `sha256sum` are available. You can
instead download and verify on another Linux machine, then transfer the
extracted directory to the router. For example, **after** `v0.8.1` has been
published:

```sh
VERSION=v0.10.1
TARGET=aarch64-unknown-linux-musl
PACKAGE="router-hub-${VERSION}-${TARGET}"
URL="https://github.com/vm75/router-hub/releases/download/${VERSION}"

curl -fLO "${URL}/${PACKAGE}.tar.gz"
curl -fLO "${URL}/${PACKAGE}.tar.gz.sha256"
sha256sum -c "${PACKAGE}.tar.gz.sha256"
tar -xzf "${PACKAGE}.tar.gz"
cd "${PACKAGE}"
# Run as root on the router:
./scripts/install-merlin.sh ./router-hub
```

The package includes the executable, `config/`, `scripts/`, and release
documentation. Back up the router's configuration and persistent data before
upgrading; the installer preserves existing `router-hub.toml` and should be
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
./scripts/install-merlin.sh ./router-hub-aarch64-unknown-linux-musl --mosquitto-user admin --mosquitto-pass mysecretpassword
```

The installer:

1. installs the binary as `/opt/bin/router-hub` and the Entware init script as
   `/opt/etc/init.d/S99router-hub`;
2. creates `/opt/etc/router-hub/router-hub.toml` and a random authentication
   token on first install;
3. bootstraps an empty current Jinja nginx tree if nginx.conf is missing,
   preserving existing configuration, and creates runtime/data directories;
4. configures Mosquitto credentials in `/opt/etc/mosquitto/passwd` if `--mosquitto-user` and `--mosquitto-pass` (or `MOSQUITTO_USER` / `MOSQUITTO_PASS` env vars) are supplied;
5. adds token-free reconciliation calls to `/jffs/scripts/post-mount` and
   `/jffs/scripts/firewall-start` (with a background retry loop);
6. validates the configuration and restarts Router Hub.

Review the generated TOML before starting. In particular, set the correct
Asuswrt extension page (`user1.asp` through `user20.asp`), `menu_tree`,
`menu_index`, nginx paths, certificate paths, bind address, and allowed
origins. The installer’s default paths are examples for a typical Entware
layout, not proof that every firmware image uses the same layout.

The service supports the usual Entware init actions:

```sh
/opt/etc/init.d/S99router-hub start
/opt/etc/init.d/S99router-hub stop
/opt/etc/init.d/S99router-hub restart
/opt/etc/init.d/S99router-hub status
```

`reconcile` requests an immediate firewall reconciliation from the running
process and is used by the installed `post-mount` and `firewall-start` hooks.

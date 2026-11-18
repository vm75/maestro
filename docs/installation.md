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

### Router-only upgrade command

```sh
/opt/bin/router-hub upgrade
# Download and verify without attempting opkg installation:
/opt/bin/router-hub upgrade --download-only
```

The CLI checks GitHub's **latest stable** release and compares it against its
compiled version. If newer, it downloads the matching AArch64 Entware IPK and
its SHA-256 checksum, verifies the checksum, then stages the file as
`/tmp/router-hub_<version>-1_aarch64-3.10.ipk`. When called as root with an
available compatible `/opt/bin/opkg`, it attempts `opkg install` directly.
Otherwise (including `--download-only`) it prints the exact manual command.
If opkg fails, it retains the verified package in `/tmp` and prints the retry
command. Network or checksum errors abort without installing.

This is a separate CLI process, **not** an authenticated API/UI action.
`upgrade` is rejected on native Linux and simulated test-mode runs. The
upgrade requires a published stable IPK; prerelease tags and older tarball-only
releases cannot be installed through this command.

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

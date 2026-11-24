# Maestro

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/Platform-Asuswrt--Merlin-red.svg)](https://www.asuswrt-merlin.net/)

**One administrator UI for your Asuswrt-Merlin router’s services, sites, and security.**

Manage your router from a standalone page or inside the ASUS menu. Written in
Rust, with an offline-capable UI, no database, and no frontend build step.
Maestro coordinates your installed Entware tools. The modular feature registry
organizes the UI into Dashboard, Services, Web, Security, and Network groups;
existing feature URLs and API calls remain compatible.

[Install](#install-on-your-router) · [Try locally](#try-it-locally) · [Documentation](#documentation) · [Releases](https://github.com/vm75/maestro/releases)

## Features

| | What you can do |
| --- | --- |
| **Nginx sites** | Manage domains, subdomains, reverse proxies, and subfolders with inherited settings, validation, and rollback. |
| **HTTPS certificates** | Issue and renew through dehydrated using HTTP-01 or DNS-01; inspect certificates and clear stuck locks. |
| **Ban Shield** | Detect log-based attacks, score IPs and subnets, and enforce escalating bans with allowlists and observe-only mode. |
| **Entware services** | Start, stop, restart, enable, and disable services; inspect status and logs. |
| **AdGuard Home** | Toggle protection and manage DNS rewrites without conflicting with nginx-managed names. |
| **Wake-on-LAN** | Save LAN devices, check reachability, and send magic packets. |
| **Dashboard & ASUS menu** | View service and runtime status, standalone or in an Asuswrt extension page. |

> Keep management access on a trusted LAN or VPN and use a long random token.
> The built-in listener is HTTP-only; use an HTTPS reverse proxy when needed.
> Do not expose the listener directly to the public internet.

## Install on your router

You need **Asuswrt-Merlin**, JFFS custom scripts enabled, **Entware at `/opt`**,
and a binary matching your router CPU and ABI. Install nginx, dehydrated,
OpenSSL, and firewall utilities as required by the features you use.
Releases provide **AArch64 musl** tarballs and **aarch64-3.10 Entware IPKs**;
compatibility still needs testing on matching hardware and firmware. Legacy MIPS
support is not promised.

1. Back up your configuration and persistent state. For an existing installation,
   follow the [offline router upgrade procedure](docs/installation.md#upgrade-from-maestro-0110) before deployment.
2. For a new install, download the matching `.ipk` and checksum from
   [GitHub Releases](https://github.com/vm75/maestro/releases), verify the
   checksum, then run `/opt/bin/opkg install /tmp/<package>.ipk` as root.
   The IPK installs Maestro, initializes configuration, and starts the
   service. Existing Maestro installations must have current YAML inputs before installing.

   Alternatively, download the matching tarball and checksum from
   [GitHub Releases](https://github.com/vm75/maestro/releases).
   Verify with `sha256sum -c <archive>.tar.gz.sha256`, then extract the archive.
3. **For tarball installs only**, from the extracted package run as root on the router:

   ```sh
   ./scripts/install-merlin.sh ./maestro
   ```

   The installer creates a token and Maestro configuration on first install,
   installs startup hooks, and restarts Maestro. Migrating Router Hub data
   requires the explicit one-time procedure before installation.
4. Review `/opt/etc/maestro/maestro.yaml` after either installation method: set your bind address,
   allowed origins, ASUS page/menu, API URL, and service paths. The shipped
   values are examples. Restart after changes:

   ```sh
   /opt/etc/init.d/S99maestro restart
   ```

5. Open the configured ASUS menu entry, or
   `http://<router-ip>:3030/#token=<auth-token>` using the token from your YAML.

On an existing router install, `/opt/bin/maestro upgrade` checks the latest
stable GitHub release, verifies its IPK checksum, and attempts `opkg install`.
It creates a backup first; use `--no-backup` to skip it.
Use `--download-only` to stage a verified package under `/tmp` and print a
manual install command. Upgrade is unavailable in Linux and test modes.

Toolbox builds and publishes custom Entware packages separately. On a live
router, list compatible releases and select an exact release tag:

```sh
/opt/bin/maestro toolbox list
/opt/bin/maestro toolbox install toolbox-nginx-entware-1.30.8-99_aarch64
```

The installer verifies and installs **every IPK** in the selected release,
including all four matching NGINX packages. Use `--download-only` to stage
the bundle, or `--allow-downgrade` to permit older versions. See
[Toolbox installation](docs/installation.md#router-only-toolbox-packages).
Maestro's own `upgrade` command remains separate.


Create a backup separately with `maestro backup`; see
[backup instructions](docs/installation.md#backups). Use
`maestro restore <archive>` while services are stopped to recover a backup;
`--root <directory>` restores into a separate tree for inspection.

See [installation](docs/installation.md) for verified download commands, source
builds, installer options, and service control.

## Try it locally

With **Rust 1.85+**, from a checkout:

```sh
make run-simulated
```

Open <http://127.0.0.1:3030> and use the fixture test token `maestro-test-token`.
This uses fixture data, simulates external commands, skips ASUS menu changes,
and does not send Wake-on-LAN packets.

For real Linux services, use `make init-linux` once, then `make run-linux`.
For the QEMU router environment, use `make init-router` once, then
`make run-router`. See [development environments](docs/development.md#run-locally)
for prerequisites and browser access, or run `make help`. `make run` remains
an alias for `make run-simulated`.

## Configure and use

Router configuration is YAML; Linux uses a separate native version: 1 YAML authority. Older router TOML must be converted offline before installation.

Start with the [example YAML](config/maestro.example.yaml) for paths,
authentication, commands, and integration settings. Then use the UI to manage
services, sites, certificates, saved devices, and firewall policy.

- **Nginx:** configure reusable listeners and source access rules in **Global settings**, then select them on sites. See [ingress and certificate windows](docs/nginx-ingress.md).
  **Setup quick reference** has offline profiles and route examples.
- **Ban Shield:** evaluate rules with `observe_only` before enabling enforcement;
  tune thresholds and ban durations to suit your network.
- **AdGuard Home:** configure its endpoint and credentials to enable integration.

See [configuration and operation](docs/configuration.md) for persistence,
security, nginx settings, and firewall tuning.

## Documentation

| Guide | Contents |
| --- | --- |
| [Installation](docs/installation.md) | Packages, source builds, installer options, service commands |
| [Configuration](docs/configuration.md) | YAML mappings, nginx settings, Ban Shield tuning, operational notes |
| [Phase 2 validation](docs/plans/proposed/modular-platform-ui-migration.md) | Remaining acceptance gates and reference parity ledger |
| [Development](docs/development.md) | Prerequisites, local checks, certificate tests, API overview |
| [Linux integration](docs/linux.md) | Real-service lab setup and smoke tests |
| [Architecture](docs/architecture.md) | Components, data flows, persistence, invariants |
| [CI and releases](docs/releases.md) | Maestro publishing and external Toolbox packages |
| [Nginx bundle](config/nginx/README.md) | Site options, route policies, templates, validation |
| [Changelog](CHANGELOG.md) | Release history |

Coding agents: see [AGENTS.md](AGENTS.md) for repository workflow and constraints.

## License

[MIT](LICENSE)

# Native Linux integration

[← Back to README](../README.md)

For testing the production execution path on a development Linux machine,
use `platform = "linux"` and `test_mode = false` in a **separate**
configuration. Alternatively, pass `--linux-mode` to select the Linux
platform from the CLI. Do **not** pass `--test-mode`: that flag simulates
external commands. Simulated test mode and the Linux platform cannot be
combined.

The native Linux platform runs the normal command runner and HTTP API,
reads real Linux `/proc` metrics, and uses the configured service paths. It
skips ASUS menu installation, standalone startup does not emit an ASUS ASP
page, and AdGuard LAN IP discovery does not call router NVRAM. Set a real
`[adguard].lan_ip` if AdGuard Home is enabled. All native Linux paths and
executables must be configured for the development host. Use
`[server].daemonize = false` for local interactive runs.

To prepare a persistent local setup, run:

```sh
./scripts/setup-linux.sh
cargo run --no-default-features --features linux -- --config data/linux/maestro.toml --linux-mode serve
```

The setup script checks for required tools and creates `data/linux/maestro.toml`,
`data/linux/nginx/nginx.conf`, and isolated state, log, and runtime directories.
It preserves existing files on subsequent runs. New configs bind the API to
`127.0.0.1:3030`, use nginx port `8080`, generate a random API token in the TOML
file, and disable firewall enforcement and AdGuard integration. Nginx uses local
temporary directories so it can run without root. Start nginx through the UI;
the setup script does not start services or install packages.

## Svelte shell build

Build the Linux shell and embed its static assets in a Linux release binary:

```sh
./scripts/build-release.sh --linux-ui x86_64-unknown-linux-gnu
```

This build uses Node.js 20.19+ or 22.12+ with npm and produces a
`maestro-x86_64-unknown-linux-gnu-linux-ui` binary and checksum in `dist/`.
For development, run `npm --prefix linux-web ci`,
`npm --prefix linux-web run build`, and add `--no-default-features --features linux-ui` to
`cargo run`. Start the binary with a Linux configuration and open its root URL.
Sign in with the configured access token. Browser sign-in accepts local HTTP
or the HTTPS URL listed in `server.allowed_origins`. For HTTPS, the reverse
proxy must preserve the public Host header.

## Systemd service actions

The Services page reads system and user inventories and recent journal entries.
To enable actions, list the exact units Maestro may manage in the matching
scope under `[services]`:

```toml
[services]
systemd_system_write_units = ["example.service"]
systemd_user_write_units = []
```

Both lists default to empty. Actions use the Maestro process's existing
systemd permissions and run without password prompts. Both the requested
unit and its canonical systemd name must be allowed. Generated Maestro
Quadlet units are reserved for the workload coordinator. Boot enablement and
disablement are separate from start and stop. Systemd may continue a job that
it has already accepted, so Maestro rejects cancellation of submitted service
actions.

## Package update status

The Updates page identifies the native package family from `os-release` and
reads cached candidates on APT, DNF, Pacman, and Zypper. Backend queries use
existing metadata and report the reboot-required marker when available.
The package list reflects the host's existing repository metadata.

Set `[system].allow_package_writes = true` to expose tracked refresh and install
jobs when the Maestro process already runs as root. Install requires a
confirmation and may restart services. Maestro issues no reboot command.
Pacman refresh uses `checkupdates` with an isolated cache under the configured
state directory. Pacman installation performs a full native system upgrade.
Once submitted, package jobs run to completion or timeout. An interrupted
package transaction may require recovery with native tools.

## Workload writes and recovery

Set `[system].allow_workload_writes = true` to allow workload apply, revert,
recovery and lifecycle actions. The default is read-only. Native apply
currently accepts reviewed managed-directory provisioning and supported
Quadlet changes. Plan and revert previews list resources the configured
provider cannot apply. Removing a storage definition removes its ownership
marker and preserves the directory contents.

Set `[paths].quadlet_dir` to an existing owner-only directory in the rootless
user manager's Quadlet search path. Maestro verifies generated files and
loaded units before changing state or reading journals. The Applications
inventory reports verified systemd state, including mixed member states.
Failed or timed-out inspections leave the state unknown. Lifecycle controls
also require verified units for workload dependencies.

For an interrupted apply, inspect the journal with `workloads_pending` and
submit the reviewed plan fingerprint to `workloads_resume`. The coordinator
verifies native state before continuing and keeps ambiguous changes pending.

## Native integration smoke test

The built-in **unprivileged** smoke test requires `nginx`, `openssl`, `curl`,
and Python 3.11+ with Jinja2 installed. It uses Cargo to build the `linux`
profile unless you supply a prebuilt binary. The test creates an isolated
temporary configuration and nginx prefix under `/tmp`, verifies Linux tool
discovery, authentication and system metrics, and exercises nginx validation,
startup, reload, shutdown, and managed-site creation, updates, regeneration,
HTTPS requests with self-signed certificates, and deletion:

```sh
make test-linux
# Or retain the temporary directory and logs after execution:
MAESTRO_KEEP_LINUX_LAB=1 make test-linux
# Or test an existing Linux release binary:
MAESTRO_LINUX_SMOKE_BIN=./target/release/maestro make test-linux
```

The smoke test neither uses mocks nor touches the host's nginx configuration.
It deliberately leaves the Ban Shield firewall policy **disabled**, does
not send WOL packets, and does not issue public certificates or change DNS.
It is a real local integration smoke test, not exhaustive feature coverage.

To test additional features against actual services, point the Linux
configuration to a dedicated nginx tree, real `dehydrated`, `openssl`,
`ip`, `ping`, `wg`/`wg-quick`, and a dedicated AdGuard Home API endpoint.
Use `[nginx].prefix_path` when nginx's relative includes must resolve below
a separate `-p` prefix. Certificate DNS-01 tests require a real DNS
provider and a domain you control; `scripts/test-dehydrated.sh` exercises
the dehydrated command against Let's Encrypt staging without changing Maestro's certificate store. HTTP-01 additionally requires a publicly reachable
ACME challenge. Run real ipset/iptables Ban Shield enforcement only inside an
isolated Linux network namespace or a disposable VM: even
`observe_only = true` can remove Maestro-owned firewall hooks during
reconciliation. A local systemd unit does not automatically satisfy the
Entware-style init script interface; use real compatible init scripts in a
separate `[services].init_dir`.

The Linux platform does **not** emulate firmware behaviors such as NVRAM,
ASUS menu injection, JFFS startup hooks, or router-specific service actions.
Those still need router validation. Linux keeps its firewall policy within
`[paths].data_dir` instead of reading or mirroring the router's
`/opt/etc/maestro/firewall-policy.json`.

## Service adapters and nginx setup

Linux setup installs executable `S80nginx` and `S99maestro` adapters in
`data/linux/init.d`. These use the normal Services discovery and actions. They require
Python 3.11 or newer and default to `data/linux/maestro.toml`; set
`MAESTRO_CONFIG` to select another configuration. For an existing
Maestro setup, set `[services].init_dir` to the absolute path of
`data/linux/init.d` and rerun `./scripts/setup-linux.sh`. Existing Maestro
adapters and configuration files are preserved. Router Hub Linux lab
configurations require a deliberate manual transfer; there is no runtime
environment-variable fallback or automatic configuration import.

Build Maestro before starting it through `data/linux/init.d/S99maestro start`.
The adapter prefers the release binary, falling back to the debug binary, and
requires `server.daemonize = false`. It also recognizes a foreground server
started with an explicit `--config` argument for the same configuration.
Nginx actions use the configured executable, prefix, configuration, and PID file.

Linux setup selects dehydrated from `PATH` and writes its absolute path to
`[commands].dehydrated`. If unavailable, it selects a local executable path
under `data/linux/certs`; use the Certificates page’s dehydrated update action
to download it before issuing certificates. For existing configurations, add
an explicit `[commands].dehydrated` path and restart Maestro.

Linux setup requires Python Jinja2 (available via `requirements.txt` in the virtual environment), nginx, and openssl. For a new lab it renders
only the current Jinja bundle, generates a temporary fallback TLS certificate,
and records its loopback listeners and runtime paths in
`maestro/nginx/nginx.toml`. HTTP uses port 8080 and HTTPS uses port 8443.
Existing lab configurations are preserved. Before upgrading an existing lab,
set `nginx.inputs_dir` to its `maestro/nginx` directory and follow the
[input relocation procedure](migration.md#nginx-input-relocation-in-09).
The smoke test uses a separate temporary directory under `/tmp`.

## Declarative Applications and File Sharing

The Applications page provides a unified view of Quadlet, Podman Compose,
Docker Compose, Incus VM, and Libvirt/KVM workloads. Workloads are authored in
authoritative declarative YAML (`version: 1`).

Maestro enforces:
- Strict referential integrity (guest network bindings, volume mounts, dependencies, and storage references).
- Deterministic change plan review with unique plan fingerprints.
- Transactional apply with recovery journaling and rollback support.
- Full revision history and revert capabilities.

The File Sharing page summarizes configured Samba and NFS network shares and
their underlying service configurations. Network shares are declared directly
within the declarative YAML model (`shares:`) and bound to managed storage
directories.

## Model Context Protocol (MCP) and Agent Integration

Maestro exposes an MCP-compliant JSON-RPC 2.0 interface at `POST /api/mcp` for
autonomous coding agents and external tools:
- `initialize`: returns server capabilities and version.
- `tools/list`: dynamically enumerates available management tools and their input schemas.
- `tools/call`: executes tools with the same argument validation, session authorization, Host/Origin restrictions, and write policies.

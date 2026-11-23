# Native Linux integration

[← Back to README](../README.md)

For testing the native Linux execution path on a development Linux machine,
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
The release build also produces a separately checksummed `maestro-admin-<target>`
helper binary; it is **not installed** by building or starting Maestro.
The helper, package/share verbs and two-mode installer remain in-progress and
must not be deployed on unrelated production hosts.

For development, run `npm --prefix linux-web ci`,
`npm --prefix linux-web run build`, and add `--no-default-features --features linux-ui` to
`cargo run`. Start the binary with a Linux configuration and open its root URL.
Sign in with the configured access token. Browser sign-in accepts local HTTP
or the HTTPS URL listed in `server.allowed_origins`. For HTTPS, the reverse
proxy must preserve the public Host header.

## Non-root Linux mode and optional privileged helper (Phase 2 WIP)

**The main Linux server must run as a non-root workload account.** A production
Linux `serve` invocation started as UID 0 is rejected; this check does not
apply to the ASUSWRT/router executable or isolated simulation mode. The
installer and guest validation for these rules are not yet complete.

The separate, Linux-only `maestro-admin` is a root-owned one-shot program,
invoked exclusively via fixed `sudo -n -- /usr/local/libexec/maestro-admin`
without command-line arguments. It accepts a small versioned JSON request on
stdin, does not listen on a socket and has no general command execution verb.
The root-owned private policy file is fixed at
`/etc/maestro/admin-policy.json`, with the following **illustrative** shape:

```json
{
  "version": 1,
  "workload_uid": 1001,
  "system_units": ["example.service"],
  "package_writes": false
}
```

Replace `1001` with the actual numeric UID of the chosen non-root workload
account. Both this file and its parent directories must be root-owned, and the
policy must be mode 0600. The helper executable must be a root-owned executable
regular file under root-owned, nonwritable parent directories. An appropriate
restricted sudoers line for a **reviewed** account is:

```text
<workload-account> ALL=(root) NOPASSWD: NOSETENV: /usr/local/libexec/maestro-admin ""
```

The default `package_writes: false` denies package mutations in the
root-owned policy. Enabling package operations also requires
`[system].allow_package_writes = true` in the non-root Maestro config,
an authorized helper matching protocol 1 and a supported native manager.
Native package and dependency operations use fixed root-owned executables,
validate 1–32 repository names, disallow URLs/flags/versions/repository
overrides and do not execute a shell. Package refresh and installation may
change the host substantially: review native package-manager state first.
Do not approve this policy on production hosts until native acceptance.

The placeholder must be replaced and the rule reviewed with `visudo`. Do not
copy a blanket sudo rule, add `SETENV`, install a setuid helper or run the
main Maestro process as root. **These contracts describe the current development
boundary, not a production-supported installation procedure.** An explicit
reviewed installer with default no-helper, helper opt-in/removal, policy
validation, upgrades and full disposable-guest acceptance remains required.

Without the helper, monitoring, readable system/user service inventory and
journal, cached package status, user-owned config/storage, user systemd and
rootless/independently authorized workload backends continue subject to normal
account permissions. The protected systemd actions are unavailable with a
machine-readable explanation; the Services page continues to show read-only
inventory and can manage explicitly authorized *user* units. The
`system_capabilities` tool (via HTTP or MCP) and `GET /api/tools` expose
helper readiness and write denials.

## Systemd service actions

Configure exact units under `[services]`:

```toml
[services]
systemd_system_write_units = ["example.service"]
systemd_user_write_units = []
```

Both allowlists default to empty; neither is a grant of root privilege.
User-scope actions invoke the non-root user's systemd manager directly.
System-scope protected actions require **both** Maestro's allowlist and the
root-owned helper's allowlist, an authorized noninteractive sudo rule and fresh
helper checks. The helper validates the exact unit, canonical systemd identity
and fixed start/stop/restart/reload/enable/disable verb before invoking systemctl.
Generated Maestro application units and Maestro's own service are reserved.
Enabling/disabling a unit changes its boot policy without starting/stopping it.
Service details and bounded journals are read without unnecessary elevation.

## Package status, upgrades and dependency installation

Cached APT, DNF, Pacman and Zypper updates remain available under ordinary
permissions without a helper. The Linux-only helper now contains fixed verbs
for metadata refresh, regular native upgrades, named repository package
installation, and predefined Podman/Docker/Incus/libvirt/Samba/NFS dependencies.
The HTTP and MCP tool schemas enforce explicit `confirm=true` for destructive
operations; the helper independently validates them, and execution is denied
unless both non-root configuration and private root-owned policy permit writes.

`[system].allow_package_writes = true` **does not itself elevate Maestro**.
The optional `maestro-admin` must also be installed and narrowly authorized.
The Updates page reports why protected actions are disabled, while cached
status remains visible. Named and dependency installation are serialized
management jobs, subject to native package locks, repository signatures and
transaction interruption. On Arch, named package installation includes a full
native upgrade to avoid partial upgrades.

**Implementation is not native-accepted:** the helper protocol, Linux release
artifact and two-mode deployment remain unverified on disposable guests. No
package transaction has been validated on a host in this increment. Treat
these newly implemented verbs as development-only until acceptance.

## Durable browser session state

Linux sessions now use 256-bit random bearer cookie values and persist only
their SHA-256 digests and bounded expiry times in
`[paths].data_dir/linux-auth/browser-sessions.json`, under an owner-only
directory. A token rotation invalidates previously persisted sessions.
Logout persists revocation; malformed, symlinked or unsafe session state
fails closed. Password-based first-run administrator setup is not yet
implemented: the browser still uses the configured token for initial sign-in,
and the mandatory credential/authentication parity gate remains open.

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

Maestro currently exposes an initial JSON-RPC 2.0 MCP interface at `POST /api/mcp` for
autonomous coding agents and external tools:
- `initialize`: returns server capabilities and version.
- `tools/list`: dynamically enumerates available management tools and their input schemas.
- `tools/call`: executes tools with the same argument validation, session authorization, Host/Origin restrictions, and write policies.

**Phase 2 parity caveat:** the current HTTP MCP adapter is not yet a complete
Streamable HTTP transport and there is no stdio MCP binary. Protocol negotiation,
structured MCP results and official SDK contract parity remain mandatory before
claiming reference compatibility. Native workloads, safe share mutations,
backup capture/recovery and the production Linux installer are also incomplete.
See [Phase 2 parity matrix](plans/proposed/server-man-phase2-parity-matrix.md).

### Systemd service privilege-boundary caveat

The service is always installed under its existing non-root workload
account. The optional, explicitly authorized `sudo -n` helper relies on
setuid privilege transition: a systemd service configured with
`NoNewPrivileges=yes` or an empty `CapabilityBoundingSet` would prevent
the authorized helper from performing root operations. The current
installation unit therefore leaves setuid elevation available **only through
the narrow sudoers command allowlist**; the main process receives no ambient
root capabilities. Avoid blanket sudo, `SETENV`, or additional allowlisted
programs. This boundary requires a disposable-guest security review in both
helper-free and helper-enabled deployments before production use.

## Continued Phase 2 implementation status (unverified)

The Linux release build now stages three separate executables with checksums:
`maestro` (non-root HTTP/UI server), `maestro-admin` (optional installed
restricted root helper), and `maestro-mcp-stdio` (non-root agent transport).
The new `scripts/install-linux.sh` offers a reviewed dry run, non-root
systemd service installation, explicit helper opt-in/removal and conservative
existing-binary rollback. It preserves workload configuration and does not
install packages or change groups automatically. **These new installation
paths have not passed disposable-guest acceptance; do not deploy them on
production hosts yet.**

The privileged helper policy now has independent default-deny flags:

```json
{
  "version": 1,
  "workload_uid": 1001,
  "system_units": [],
  "package_writes": false,
  "samba_password": false
}
```

Protected package writes additionally require
`[system].allow_package_writes = true`; Samba password changes require
`[system].allow_share_writes = true`. These TOML flags authorize neither
capability alone. A matching root-owned helper policy, strict sudoers rule,
native backend readiness and explicit operation confirmation remain required.
The Samba password tool never accepts a selected account; the root helper
resolves the non-root sudo caller and passes the password only via stdin.
Samba/NFS candidate publication and server setup are still unavailable.

Configuration's new backup panel is **metadata-only and non-restorable**.
Native capture, manifests, verification, publication, interrupted recovery
and actual restores must not be inferred from the inventory. The Configuration
page has typed storage/share editor forms over the existing YAML store; it
cannot safely complete every reference definition/next-start workflow yet.
The existing Linux `server` settings still require single-authority migration,
and first-run administrator username/password authentication is missing.

The new shared MCP dispatcher and stdio executable require official SDK
contract tests, including protocol negotiation, schema parity and error
handling. The separate Agent Access and Activity UI flows have not been
browser-accepted. Consult the
[Phase 2 parity matrix](plans/proposed/server-man-phase2-parity-matrix.md)
before claiming completed Server Man functionality.

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
cargo run --no-default-features --features linux -- --config data/linux/maestro.yaml --linux-mode serve
```

The setup script checks for required tools and creates `data/linux/maestro.yaml`,
`data/linux/nginx/nginx.conf`, and isolated state, log, and runtime directories.
It preserves existing files on subsequent runs. New configs bind the API to
`127.0.0.1:3030`, use nginx port `8080`, generate a random API token in the YAML
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
Sign in with the configured access token. Browser sign-in accepts loopback HTTP
and exact HTTP or HTTPS origins listed in `server.allowed_origins`. For LAN
access, add the full origin (for example,
`http://192.168.192.70:3030`) and restart Maestro. HTTP sessions use cookies
without the Secure attribute; use HTTPS when transport encryption is required.
For HTTPS, the reverse proxy must preserve the public Host header.

If Maestro will manage nginx and certificates, bootstrap over LAN HTTP first.
Keep the existing server settings (including the token) and set:

```yaml
server:
  bind: "0.0.0.0"
  port: 3030
  allowed_origins:
    - "http://192.168.192.70:3030"
```

Replace the example address with the host's LAN address and restrict port 3030
to the LAN. Restart Maestro, open that exact origin and complete sign-in/setup.
After configuring nginx and certificates, add the public HTTPS origin to the
list and restart before switching to it. If nginx runs on the same machine,
you can then bind Maestro to loopback to restrict direct access.

## Non-root Linux mode and optional privileged helper (Phase 2 WIP)

**The main Linux server must run as a non-root workload account.** A production
Linux `serve` invocation started as UID 0 is rejected; this check does not
apply to the ASUSWRT/router executable or isolated simulation mode. The
installer and guest validation for these rules are not yet complete.

The separate, Linux-only `maestro-admin` is a root-owned one-shot program,
invoked exclusively via fixed `sudo -n -u root -- /usr/local/libexec/maestro-admin`
without helper command-line arguments. Management operations use Server Man's
one-shot JSON operation/arguments envelope on stdin. No private wire probe or
protocol version is accepted. The helper does not listen on a socket and has
no general command execution verb. Discovery checks the exact sudo authorization
without executing the helper, and reads the same nonsecret root-owned grants
that the helper independently enforces on every request.
The root-owned policy file is fixed at
`/etc/maestro/admin-policy.json`, with the following **illustrative** shape:

```json
{
  "version": 1,
  "workload_uid": 1001,
  "system_units": ["example.service"],
  "package_writes": false,
  "samba_password": false,
  "share_writes": false
}
```

Replace `1001` with the actual numeric UID of the chosen non-root workload
account. Both this file and its parent directories must be root-owned. The
policy contains only authorization grants (no secrets) and the installer
sets mode 0640 with the selected workload account's group, so the daemon
can read its permissions but cannot modify them. The helper independently
rejects group-write, other-user access, symlinks, unsafe parents, invalid
contents and mismatched workload UIDs. Existing 0600 policies remain
readable by the helper but disable daemon discovery until the reviewed
helper installation is repeated; the installer preserves the grant contents.
The helper executable must be a root-owned executable
regular file under root-owned, nonwritable parent directories. An appropriate
restricted sudoers line for a **reviewed** account is:

```text
<workload-account> ALL=(root) NOPASSWD: NOSETENV: /usr/local/libexec/maestro-admin ""
```

The default `package_writes: false` denies package mutations in the
root-owned policy. Enabling package operations also requires
`server.allow_writes: true` and `server.privileged_operations: true` for native Linux YAML (or `system.allow_package_writes: true` in the legacy layout) in the non-root Maestro config,
an authorized helper supporting the reference request/response contract and a supported native manager.
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

Configure exact units under `services`:

```yaml
services:
  systemd_system_write_units:
  - example.service
  systemd_user_write_units: []
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

`server.allow_writes: true` and `server.privileged_operations: true` for native Linux YAML (or `system.allow_package_writes: true` in the legacy layout) **does not itself elevate Maestro**.
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
`<server.state_dir>/linux-auth/browser-sessions.json`, under an owner-only
directory. A token rotation invalidates previously persisted sessions.
Logout persists revocation; malformed, symlinked or unsafe session state
fails closed. Native Linux YAML supports first-run administrator setup and password login.
Setup records the password hash in the authoritative YAML; the token file
remains available for authorized API clients. Native/browser authentication
acceptance remains open.

## Workload writes and recovery

Set `server.allow_writes: true` in native Linux YAML (or
`system.allow_workload_writes: true` in the legacy layout) to allow workload apply, revert,
recovery and lifecycle actions. The default is read-only. Native apply
currently accepts reviewed managed-directory provisioning and supported
Quadlet changes. Plan and revert previews list resources the configured
provider cannot apply. Removing a storage definition removes its ownership
marker and preserves the directory contents.

`managed_apply_status` reads durable progress, including completed operations,
per-resource failure and the journal update time, without running native commands.
It remains readable with management writes disabled. `managed_apply_retry`
requires management writes and `confirm: true`; it verifies native state and uses
the recorded apply mode. It never automatically repeats an interrupted removal.
Inspect that resource explicitly and resolve its native state before retrying.
If interruption occurred after preparing intent but before saving its YAML,
automatic retry discards that uncommitted intent and reports `aborted`. It leaves
configuration and native resources unchanged. A prepared revert instead restores
the earlier pending operation when its replacement YAML was never saved. Retry
of a committed desired revision still inspects native state before doing work.
The Applications recovery panel displays this progress, starts safe retries, and
explains when uncommitted intent was discarded.
Native command output is not stored in the durable failure message.

Native Linux YAML selects `${XDG_CONFIG_HOME:-$HOME/.config}/containers/systemd/maestro`
for rootless Quadlet files. Prepare that directory as the workload account with
owner-only permissions; startup reads settings without creating it. Legacy YAML
can set `paths.quadlet_dir` to an existing owner-only directory in the rootless
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
`paths.data_dir` instead of reading or mirroring the router's
`/opt/etc/maestro/firewall-policy.json`.

## Service adapters and nginx setup

Linux setup installs executable `S80nginx` and `S99maestro` adapters in
`data/linux/init.d`. These use the normal Services discovery and actions. They require
Python 3.11 or newer and default to `data/linux/maestro.yaml`; set
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
`maestro/nginx/nginx.yaml`. HTTP uses port 8080 and HTTPS uses port 8443.
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

### Podman and Docker Compose

Both Compose engines now use the native reference adapter in source. Maestro
generates a private, explicit project definition and a separate linked user
systemd oneshot boot launcher under the applications directory. The launcher
does not supervise containers; Compose uses `restart: no`. Use the managed
service enable/disable action for startup policy, and start/stop/restart for
execution. Saving a definition does not start a stopped application;
`next_start` updates the saved definition and boot policy without recreating
containers. Immediate apply reconciles only previously running members.
Removing a service or member requires stopped containers and never passes
volume deletion options. Exact generated-file ownership and native project
identity are checked before changes.

Rootless Podman uses the local engine and the podman-compose provider.
Docker uses the local system socket (access to this socket is a powerful
host privilege; grant it only to a trusted workload account). Docker automatic
startup requires the native docker.socket unit to be persistently enabled;
Podman automatic startup requires its packaged user network-wait unit.
The daemon remains non-root; neither provider routes ordinary commands
through the privileged helper. Explicit advanced container options are typed
and validated; short bind options require preexisting storage.

These are source-level implementations. Build/fixture and disposable Linux
acceptance, remaining provider-specific readiness, combined log fidelity
and interrupted artifact-transition recovery are not yet established.

### Libvirt/KVM

The native Libvirt provider uses only an explicit local `qemu:///system` or
`qemu:///session` connection. It checks the configured daemon's KVM domain
capabilities rather than assuming the workload account can open /dev/kvm.
Install the native `virsh` client and `virt-xml-validate`; the workload
account needs appropriate access to the chosen daemon and guest storage.

The pinned Server Man guest profile is x86_64, BIOS, KVM, Q35 by default,
host-model CPU, headless VGA and a serial console. Disks reference declared
raw/qcow2 file storage and interfaces reference an explicit network or bridge.
Existing VM UUID and MAC values are retained by typed edits; new guests
currently require explicit UUID and MAC in reviewed YAML. The reference's
automatic identity preparation needs integration with its reviewed candidate
editor. UEFI and direct initial-image provisioning are not part of this
profile; prepare the disk through the storage workflow first.

Saving defines a persistent guest and sets autostart without starting it.
A running hardware change requires Save for next start or a shutdown before
immediate apply. Restart requests graceful shutdown, waits for shutoff and
starts the saved definition. Removal requires a stopped owned guest; no
force-stop, storage deletion, NVRAM deletion or TPM deletion is requested.
The provider compares ownership namespace, UUID, name, managed hardware,
fingerprints, generated artifacts and native autostart before mutation and
during recovery inspection. Unknown/intermediate recovery state remains
blocked pending the reviewed artifact recovery workflow.

Libvirt source implementation and fake-daemon regression tests are present;
build/fixture execution and native guest acceptance remain pending.
Basic stopped-guest XML and declared standalone-disk collection is now part of the Linux backup capture workflow below.

### Incus virtual machines

The Incus adapter uses the explicit local daemon, project, storage pool and
network/bridge bindings. The reference VM profile uses UEFI and Secure Boot,
a locally available image alias or fingerprint, persistent UUID/MAC identity,
explicit devices, no implicit profiles, and pool-managed root storage (20 GiB
by default). QEMU SPICE availability is checked with option help only,
without starting a guest. The workload account requires access to the local
Incus daemon; this access is security-sensitive and must be explicitly granted.

Saving defines a stopped VM. Boot autostart is separate from execution.
Unlike Libvirt, Incus v1 requires the VM to be stopped before hardware or
native description changes, including Save for next start. A running VM
receives only boot policy and definition fingerprint updates. Existing
native volatile settings are retained; the adapter tracks the runtime
revision loaded by the guest. Graceful stop and restart use a bounded
shutdown request without force. Logs are bounded and scoped to the managed
instance. Initial image changes, root shrink, and project/pool moves are
rejected rather than silently reprovisioning or relocating data.

Removing an existing Incus instance is deliberately unavailable until an
explicit data-preserving export workflow is implemented. If creation failed
and no instance exists, only its verified generated definition may be removed.
New guest identities still need explicit UUID/MAC until reviewed-candidate
identity preparation is completed. Native provider compilation, fixture execution and disposable guest
acceptance remain pending. Basic stopped-instance export is implemented in
source; custom volumes/snapshot coverage and intermediate artifact recovery
remain pending.

The File Sharing page summarizes configured Samba and NFS network shares and
their underlying service configurations. Network shares are declared directly
within the declarative YAML model (`shares:`) and bound to managed storage
directories.

## Linux backup capture and recovery

The reference backup inventory and confirmed capture/recovery tools now have
source implementations. Capture is revision-pinned and private, and produces
a checksummed **captured**, not prepared or restorable, bundle. It can gracefully
stop fully running managed workloads in reverse dependency order and resume
only workloads whose stop was attempted, in dependency order. The durable
`<config>.backup.json` journal records stop intent, observed progress, staging
identity, manifest checksum and publication stage. An incomplete journal blocks
normal management. Startup does not automatically resume guests.

The initial native collectors support standalone declared raw/QCOW2 Libvirt
disks and private inactive domain XML (no managed-save, snapshots/checkpoints,
NVRAM, TPM or external/backing-chain state), and portable stopped Incus root
instance exports from the verified local non-clustered daemon with an explicit
project/pool and verified archive index. Custom volumes, snapshot history and
unsupported native devices remain capture blockers. Incus instances are not
deleted by capture or by configuration removal.

The initial filesystem collector supports regular files and directories with
no symlinks, multiply linked files, visible xattrs/ACLs or nested mount
boundaries; these cases require the remaining reference filesystem port.
It records source metadata and preserves zero-filled regions as holes, but
does not claim full metadata or restore equivalence. The capture destination
must be absent, with a real workload-owned non-group/world-writable parent,
outside the configuration and system roots. Publication uses no-replace
rename and sync. Failed/uncertain stages are retained rather than automatically
deleted. Verification checks exact tree contents, hashes, revision, native
object identity and required configuration coverage.

Use `managed_backup_recovery_status` to obtain the current review token.
`managed_backup_recover` resumes only recorded workloads against the pinned
revision, `managed_backup_publish` verifies and finishes the recorded
publication, and `managed_backup_release` acknowledges the recovery blocker
without starting workloads or publishing/deleting data. The current job
boundary is non-cancellable during native capture; the capture deadline and
explicit recovery remain available. Full reference custom-volume/snapshot
discovery, filesystem metadata, existing empty-directory publication,
generated-artifact sidecar review/reconciliation, exact manifest parity
and native fault acceptance remain outstanding. No restore tool is added.

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
  "samba_password": false,
  "share_writes": false
}
```

Protected package writes additionally require
`server.allow_writes: true` and `server.privileged_operations: true` for native Linux YAML (or `system.allow_package_writes: true` in the legacy layout); Samba password changes require
`system.allow_share_writes: true` in the legacy layout. These YAML flags authorize neither
capability alone. A matching root-owned helper policy, strict sudoers rule,
native backend readiness and explicit operation confirmation remain required.
The Samba password tool never accepts a selected account; the root helper
resolves the non-root sudo caller and passes the password only via stdin.
The helper now has source implementations of the Server Man native share
definition validation/publication/rollback and server setup operations.
Publication additionally requires `share_writes: true` in the root-owned
policy. Setup also requires `package_writes: true` and the fixed mandatory
system units in `system_units`: APT Samba uses `smbd.service` and
`wsdd-server.service`; other supported Samba distributions use
`smb.service` and `wsdd.service`; NFS uses `nfs-server.service`.
Optional NetBIOS services are attempted only when authorized.
The Linux daemon now uses the same native share operations for the fixed
reference configuration paths and enabled management policy. The
`managed_share_publish` tool changes the authoritative YAML publication
toggle through the revisioned apply journal; `shares_setup` is a separate
confirmed job. The coordinator compares the complete protocol set against
the reviewed before/after configuration and refuses unknown native state.
Without authorized grants, native writes remain unavailable and read-only
inventory remains accessible within the workload account's permissions.
Native validation and differential helper acceptance remain outstanding.
These are source-level development contracts, not approval to install or
enable the helper on production hosts.

Configuration's new backup panel is **metadata-only and non-restorable**.
Private configuration history retains up to eight recent revisions and displays
their timestamps and validity. It is available only when management writes are
enabled; it contains configuration rather than workload-data backups.
Each history entry can be inspected and reviewed for restoration through
`managed_config_revert_plan` and `managed_config_revert`. Restoration uses the
shared revision-checked apply journal, preserves exact historical YAML bytes,
and archives the outgoing revision. Native changes require provider readiness;
server-setting changes require restarting Maestro. Revert may supersede a
published pending transition when native inspection confirms its removals and
unchanged resources will not be abandoned. Prepared transitions must be recovered
first. Native state outside the reviewed before/after definitions, including an
unchanged but unfinished resource, requires recovery before revert.
`mode: next_start` is implemented for managed directories and configured rootless
Quadlet publication. It records the mode in the recovery journal and publishes
Quadlet definitions without lifecycle commands, allowing existing active units
to use changed definitions at their next start. Active removals and transitional
units remain blocked, and running containers may require a later explicit restart.
Other native providers still require their ownership/lifecycle acceptance work.
Native capture, manifests, verification, publication, interrupted recovery
and actual restores must not be inferred from the inventory. The Configuration
page has typed storage/share editor forms over the existing YAML store; it
cannot safely complete every reference definition/next-start workflow yet.
Unified-YAML startup and administrator setup/password login are implemented;
installer migration and complete native next-start publication remain pending.

The new shared MCP dispatcher and stdio executable require official SDK
contract tests, including protocol negotiation, schema parity and error
handling. The separate Agent Access and Activity UI flows have not been
browser-accepted. Consult the
[Phase 2 parity matrix](plans/proposed/server-man-phase2-parity-matrix.md)
before claiming completed Server Man functionality.

### Phase 2 unified YAML startup (in development)

Linux builds now accept an absolute `.yaml` or `.yml` path through `--config`.
The selected file contains `version: 1`, optional `server` settings and the
workload sections. HTTP and stdio MCP inspect and edit that same document.
The file and its directory must be private to the workload account; `server.token_file`
selects a separate owner-only bearer-token file. Server defaults use loopback
port 8080 and a Maestro directory below `XDG_STATE_HOME` (or `~/.local/state`).
The default token is next to the selected YAML. Invalid workload definitions
remain readable for repair when server settings are valid.

This path is not yet the installer's production configuration contract. The
Configuration page can review and save the authoritative YAML. Save server
settings separately from resource edits; plans report when Maestro must restart.
After saving changed server settings, further managed mutations require that
restart. Next-start publication supports managed directories and prepared
rootless Quadlet directories; other provider and lifecycle paths remain gated.
The sign-in
page can create an administrator using the bootstrap token and a password of
12–256 bytes. Setup preserves authored comments and resource text, stores a
fixed-format password hash in the same YAML, revokes
bootstrap cookies, and switches browser login to username/password. Bearer-token
access remains available for agents. Browser sessions persist across restart;
sign-out revokes the selected session. Setup requires loopback HTTP or an
explicitly allowed HTTP/HTTPS origin.
The existing Linux lab layout remains available after its one-time YAML migration. The parity ledger tracks the
remaining auth, installer, helper and native-provider acceptance work.


### Native generator and offline engine checks

Run `make test-quadlet` to validate Maestro-rendered definitions with the local
Quadlet generator. Run `make test-podman` as the non-root workload account to test
rootless engine lifecycle and fingerprint-label inspection using an offline,
private temporary lab. See [development checks](development.md#checks) for
prerequisites, isolation and cleanup. These targets start no user systemd units.
The engine smoke uses a locally compiled scratch payload and disables registry
pulls, remote connections and networking.

Successful generation and engine execution provide host-specific evidence.
The Podman lab also exercises Maestro's bounded native metadata reader for a
running owned-but-stale container and a stopped foreign name collision. Quadlet
application inventory now reports configured member states and a restart notice
when running fingerprints differ from saved definitions. Unknown identities,
unexpected members and native-name collisions block lifecycle actions. Start
requires current running fingerprints; use an explicitly confirmed Restart for
owned stale containers.

End-to-end Maestro publication/lifecycle in a real user manager, boot policy,
metadata-only/runtime fingerprint separation, interrupted recovery and both native
installer/helper modes remain acceptance requirements.

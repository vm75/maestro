# Server Man → Maestro Phase 2 functional parity ledger

**Status:** active and incomplete — none of the reference capabilities is accepted.
**Frozen reference:** [Server Man commit c1a9f32](https://github.com/vm75/server-man/tree/c1a9f32f5898146ae7ac339043b910b8e2a2c438), audited source and tests on 2026-11-23.
**Maestro branch:** `feature/linux-phase2`; updated plan: [modular-platform-ui-migration.md](modular-platform-ui-migration.md).
**Requirement:** cover *every* implemented reference feature, not just matching tool names; 100% behavioral parity with helper, graceful reduction without it. No Server Man features are waived.

## Evidence rules and current result

- States: `not started`, `modeled` (Rust data/handler only, unproven parity), `gated` (present but deliberately blocks mutations pending security/native validation), `implemented` (domain behavior present, tests incomplete), `fixture verified`, `native verified`, `accepted`. **No `accepted` rows at this revision.**
- Every operation needs reference tests, equivalent arguments/schema/errors/permission policy, Rust unit/API contract, UI path, REST/MCP, a no-helper run and helper-enabled run. For native mutations require disposable Linux guest checks, failure/restart/recovery and observed effects. Source review or a green mock test does not establish native acceptance.
- This is an initial source-level ledger, **not** a complete baseline contract extraction. Many parameter sets are explicitly marked `inspect schema` and need full reference field-by-field fixtures. The Phase 2 gate stays open.
- **Tool-count discrepancy to resolve:** the current plan reports **55** official-SDK tools; the module source registration paths below expose **60 distinct public registry operation names** when all modules are assembled (excluding three helper-only `shares_definitions_*` verbs). Re-run the pinned Go registry/official SDK and compare conditional modules and negotiated discovery before treating either count as authoritative. Every source entry is retained below until reconciled.
- Maestro currently has **31** named Linux tool registrations (23 enum entries plus six systemd and two updates), some named differently and with narrower behavior. No mapping from `workloads_*` to `managed_*` is accepted merely by analogous names.

## Pinned reference public registry — 60 source-declared operations

Columns below are **source-level mapping/evidence**, not successful test results. The reference path and reference test column applies to every operation in its group. Each row must be expanded with exact schema/error fixtures before acceptance.


### Core / jobs

Reference: `internal/app/app.go`; tests: `internal/core/core_test.go; internal/core/runner_test.go`. Maestro: `src/api/tools.rs; src/core/jobs.rs`. UI: Overview / Activity (incomplete).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `system_capabilities` | none | Read module availability, write policy and per-operation permission states. | **modeled**; no acceptance evidence |
| `system_status` | none | Read server identity, version and write policy. | **modeled**; no acceptance evidence |
| `jobs_list` | none | Bounded recent management jobs. | **modeled**; no acceptance evidence |
| `jobs_get` | id (required, UUID) | Read individual job state, bounded output and failures. | **modeled**; no acceptance evidence |
| `jobs_cancel` | id (required, UUID) | Request cancellation; never imply rollback of committed host changes. | **gated**; no acceptance evidence |

### Monitoring

Reference: `internal/modules/monitor/module.go`; tests: `internal/modules/monitor/monitor_test.go`. Maestro: `src/core/monitor.rs; src/core/monitor/processes.rs; src/api/tools.rs`. UI: Overview, Processes, Resources (partial).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `monitor_snapshot` | none | CPU, memory, networking, storage, disk I/O and thermal sampling with bounded history/partial data. | **modeled**; no acceptance evidence |
| `monitor_processes` | sort, filter, limit | Bounded process rows with filtering, stable ordering and partial sampling. | **modeled**; no acceptance evidence |

### systemd

Reference: `internal/modules/services/module.go`; tests: `internal/modules/services/services_test.go`. Maestro: `src/platforms/systemd.rs; src/api/tools.rs; src/platforms/admin.rs`. UI: Services (partial).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `services_list` | scope=system/user | Installed plus loaded units; keep bus/read errors partial and boot enablement separate. | **modeled**; no acceptance evidence |
| `services_details` | unit, scope | Read bounded, non-secret properties without leaking environment or unit contents. | **modeled**; Rust parser/tool and Svelte inspector, no native acceptance evidence |
| `services_logs` | unit, scope, tail<=2000 | Read bounded system/user unit journal, subject to ordinary permissions. | **modeled**; no acceptance evidence |
| `services_start` | unit, scope | Validated exact system service requires helper and both allowlists; user action direct; runtime only. | **gated**; no acceptance evidence |
| `services_stop` | unit, scope | Validated exact system service requires helper and both allowlists; user action direct; runtime only. | **gated**; no acceptance evidence |
| `services_restart` | unit, scope | Validated exact system service requires helper and both allowlists; user action direct; runtime only. | **gated**; no acceptance evidence |
| `services_reload` | unit, scope | Validated exact system service requires helper and both allowlists; user action direct; runtime only. | **gated**; no acceptance evidence |
| `services_enable` | unit, scope | Validated exact system service requires helper and both allowlists; user action direct; boot policy only. | **gated**; no acceptance evidence |
| `services_disable` | unit, scope | Validated exact system service requires helper and both allowlists; user action direct; boot policy only. | **gated**; no acceptance evidence |

### Linux packages / dependencies

Reference: `internal/modules/updates/module.go`; tests: `internal/modules/updates/updates_test.go; internal/modules/updates/packages_test.go; internal/modules/updates/dependencies_test.go`. Maestro: `src/platforms/package_updates.rs; src/api/tools.rs`. UI: Updates (partial).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `updates_status` | none | Cached package/reboot status for APT, DNF, Pacman, Zypper without root. | **modeled**; no acceptance evidence |
| `updates_refresh` | none | Refresh metadata only; Pacman isolated checkupdates db; helper required. | **implemented** in source; no native acceptance evidence |
| `updates_install` | confirm=true / reference global destructive confirmation | Normal native upgrade without changing repo policy; helper required; report interruption. | **implemented** in source; no native acceptance evidence |
| `packages_install` | packages (1–32 names, whitespace-delimited) | Native named repository install with package validation; Arch no partial upgrade. | **implemented** in source; no native acceptance evidence |
| `dependencies_install` | target=podman/docker/samba/nfs/virtualization/incus | Install selected validated native provider dependencies; no unreviewed repo or group changes. | **implemented** in source; no native acceptance evidence |

### Native shares / setup

Reference: `internal/modules/shares/module.go; internal/modules/shares/setup.go; internal/modules/shares/password.go`; tests: `internal/modules/shares/shares_test.go; internal/modules/shares/setup_test.go; internal/modules/shares/password_test.go`. Maestro: `src/core/declarative/shares.rs; src/api/tools.rs; src/platforms/admin.rs`. UI: Shares (partial).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `shares_list` | protocol optional | Read managed and existing Samba/NFS definitions, connection/export state without private files. | **modeled**; no acceptance evidence |
| `shares_samba_password` | password; caller identity fixed by helper | Set password for trusted workload account only; no secret logging or account targeting. | **implemented**; no acceptance evidence |
| `shares_setup` | protocol and confirm; inspect full Go schema | Review/install/setup share service with bounded native change and rollback. | **not started**; no acceptance evidence |

### Managed configuration

Reference: `internal/declarative/module.go`; tests: `internal/declarative/module_test.go; internal/declarative/coordinator_test.go`. Maestro: `src/core/declarative/{model,yaml,store,plan,apply,journal}.rs; src/api/tools.rs`. UI: Applications and Configuration (partial).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `managed_config_read` | none | Read single private authoritative YAML and revision, not private-file contents. | **modeled**; no acceptance evidence |
| `managed_config_validate` | content (YAML) | Reject duplicate keys, custom tags, identity/reference/cycle errors; no mutation. | **modeled**; no acceptance evidence |
| `managed_config_health` | none | Report invalid settings/inputs without blocking inspection. | **modeled**; no acceptance evidence |
| `managed_config_plan` | content and revision; inspect Go schema | Deterministic revision/fingerprint preview with readiness and ownership constraints. | **modeled**; no acceptance evidence |
| `managed_config_save` | content, revision, reviewed plan; inspect Go schema | Revision-checked, journaled write plus controlled native apply and recovery. | **gated**; no acceptance evidence |
| `managed_apply_status` | none | Read durable per-resource pending/failure state. | **modeled**; no acceptance evidence |
| `managed_apply_retry` | none | Retry only safe publication; refuse uncertain removals. | **gated**; no acceptance evidence |

### Managed application lifecycle

Reference: `internal/declarative/module.go; internal/declarative/lifecycle.go`; tests: `internal/declarative/coordinator_test.go; internal/declarative/quadlet_test.go; internal/declarative/compose_adapter_test.go; internal/declarative/libvirt_test.go; internal/declarative/incus_test.go`. Maestro: `src/core/declarative/{composite,quadlet,compose,libvirt,incus}.rs; src/api/tools.rs`. UI: Applications (partial).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `managed_service_start` | service (stable ID) | Route to exact provider/managed owner; current workload lifecycle only. | **gated**; no acceptance evidence |
| `managed_service_stop` | service (stable ID) | Route to exact provider/managed owner; current workload lifecycle only. | **gated**; no acceptance evidence |
| `managed_service_restart` | service (stable ID) | Route to exact provider/managed owner; current workload lifecycle only. | **gated**; no acceptance evidence |
| `managed_service_enable` | service (stable ID) | Route to exact provider/managed owner; boot policy only. | **gated**; no acceptance evidence |
| `managed_service_disable` | service (stable ID) | Route to exact provider/managed owner; boot policy only. | **gated**; no acceptance evidence |

### Configuration health & workloads

Reference: `internal/declarative/status.go`; tests: `internal/declarative/readiness_test.go; internal/declarative/module_test.go`. Maestro: `src/core/declarative/{composite,plan,storage}.rs; src/api/tools.rs`. UI: Applications, Configuration, dashboard alerts (incomplete).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `managed_config_readiness` | none | Native prerequisite/file/mount checks, preserves unknown/stale/blocked states. | **modeled**; no acceptance evidence |
| `managed_services` | none | Managed resource current/boot/configuration state, partial provider errors. | **modeled**; no acceptance evidence |
| `managed_config_check` | none | Explicit fresh native prerequisite/readiness recheck, no mutations. | **not started**; no acceptance evidence |

### Managed application logs

Reference: `internal/declarative/logs.go`; tests: `internal/declarative/logs_test.go`. Maestro: `src/api/tools.rs; src/core/declarative/composite.rs`. UI: Applications (partial).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `managed_service_logs` | service, tail<=2000 | Bounded per-resource logs; avoid unrelated files/guest private data. | **modeled**; no acceptance evidence |

### Managed file shares

Reference: `internal/declarative/share_status.go`; tests: `internal/declarative/shares_test.go`. Maestro: `src/core/declarative/shares.rs; src/api/tools.rs`. UI: Shares (partial).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `managed_shares` | none | Read centralized typed shares plus native publication state. | **modeled**; no acceptance evidence |
| `managed_directory_browse` | path optional | Bounded permitted directory names (not contents), reject unsafe paths. | **not started**; no acceptance evidence |
| `managed_share_publish` | share, published boolean | Revisioned publish/unpublish with native candidate checks/rollback and helper. | **gated**; no acceptance evidence |

### Typed configuration editors

Reference: `internal/declarative/edit.go`; tests: `internal/declarative/edit_test.go`. Maestro: `src/core/declarative/{model,yaml,plan}.rs; src/api/tools.rs`. UI: Configuration / Applications editors (incomplete).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `managed_config_model` | none | Return typed edit models without exposing private file contents. | **modeled**; no acceptance evidence |
| `managed_resource_plan` | kind, id, operation, definition, revision | Review typed add/update/delete without mutation, report downstream impact. | **modeled**; no acceptance evidence |
| `managed_resource_save` | kind, id, operation, definition, revision, fingerprint | Apply reviewed typed edit as safe authoritative transaction. | **gated**; no acceptance evidence |

### Revision history

Reference: `internal/declarative/history_tools.go`; tests: `internal/declarative/history_read.go; internal/declarative/revert_test.go`. Maestro: `src/core/declarative/store.rs; src/core/declarative/apply.rs; src/api/tools.rs`. UI: Configuration history (incomplete).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `managed_config_history` | none | Bounded private revision list, no secret file contents. | **modeled**; no acceptance evidence |
| `managed_config_history_read` | revision / exact target; inspect schema | Read verified byte-exact historical YAML including malformed recovery snapshot. | **modeled**; no acceptance evidence |
| `managed_config_revert_plan` | revision, expected; inspect schema | Review revert of definitions only, not workload data. | **modeled**; no acceptance evidence |
| `managed_config_revert` | revision, expected, reviewed fingerprint | Create new revision, owned-resource reconciliation, preserve data. | **gated**; no acceptance evidence |

### Generated artifact conflict recovery

Reference: `internal/declarative/artifact_recovery_tools.go`; tests: `internal/declarative/artifact_recovery_test.go`. Maestro: `src/core/declarative/journal.rs; src/core/declarative/composite.rs`. UI: Configuration / recovery (missing).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `managed_artifact_review` | resource | Review exact generated artifact drift/sidecar without leaking private bytes. | **not started**; no acceptance evidence |
| `managed_artifact_reconcile` | resource, revision, review token | Metadata-only reconciliation; never overwrite conflicted native bytes. | **not started**; no acceptance evidence |

### Storage provision

Reference: `internal/declarative/provision_tools.go`; tests: `internal/declarative/provision_test.go; internal/declarative/disk_source_test.go`. Maestro: `src/core/declarative/storage.rs; src/api/tools.rs`. UI: Storage provisioning (incomplete).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `managed_storage_plan` | resource/path, revision; inspect schema | Review absent owned directory or raw/qcow2 disk with stopped consumers and source digest. | **modeled**; no acceptance evidence |
| `managed_storage_create` | resource/path, revision, reviewed token; inspect schema | Atomic no-replace directory/disk creation and pinned image clone; no silent parent creation. | **gated**; no acceptance evidence |

### Existing backup capture / recovery

Reference: `internal/declarative/backup_tools.go`; tests: `internal/declarative/backup_inventory_test.go; internal/declarative/backup_prepare_test.go; internal/declarative/backup_manifest_test.go; internal/declarative/backup_verify_test.go; internal/declarative/backup_publication_test.go; internal/declarative/backup_recovery_test.go`. Maestro: `missing — Maestro router src/backup.rs is a distinct feature`. UI: Backup inventory/recovery widgets (missing).

| Reference operation | Input/schema requirements | Behavior and privilege boundary | Current status |
| --- | --- | --- | --- |
| `managed_backup_inventory` | none | Inventory authoritative config, private refs and native dependencies; metadata only; non-restorable. | **modeled**; no acceptance evidence |
| `managed_backup_prepare` | revision, destination, quiesce, deadlineSeconds | Reviewed capture, stopped workload and native exports, checksummed private manifest; non-restorable. | **not started**; no acceptance evidence |
| `managed_backup_recovery_status` | none | Read durable stopped/resumed and staged/uncertain capture journal. | **not started**; no acceptance evidence |
| `managed_backup_recover` | revision, reviewToken | Recover only previously running workloads, no restore/replay of capture. | **not started**; no acceptance evidence |
| `managed_backup_publish` | revision, reviewToken, deadlineSeconds | Verify staged capture then publish only no-replace after review. | **not started**; no acceptance evidence |
| `managed_backup_release` | revision, reviewToken | Explicitly release recovery blocker, no replay/start/delete. | **not started**; no acceptance evidence |

### Helper-only Server Man operations (not extra public MCP tools)

- `shares_definitions_inspect`, `shares_definitions_validate`, `shares_definitions_apply`: `internal/modules/shares/definitions.go`, referenced from `internal/admin/helper.go`. Accept typed protocol/definitions, require candidate validation and owned-fragment transaction safety. **Not started** in Maestro helper; protected share publishing must stay denied until completed and validated.
- The root helper also allows `shares_setup`, `shares_samba_password`, native package/dependency operations and systemd actions. Merely exposing the equivalent REST tool does not imply helper coverage or permission.

## Non-registry capability and native acceptance ledger

| Capability | Reference source/test mapping | Maestro current location | Required verified behavior | State | Acceptance evidence pending |
| --- | --- | --- | --- | --- | --- |
| Registry, auth & permissions | `internal/core/{registry,schema,jobs}.go; internal/auth/{admin,auth,sessions}.go; internal/httpapi/server.go` | `src/api/{tools,sessions}.rs` | First-run admin setup, password hashes, persistent sessions/revocation, Host/Origin, write/destructive confirmations, bounded concurrent jobs, API schema match | **partial/gated** | Go core/auth/httpapi tests; Rust API auth contract tests; two Linux privilege modes |
| HTTP MCP and stdio | `internal/mcp/{http,protocol,stdio}.go; cmd/server-man/main.go` | `src/api/mcp.rs` | Streamable HTTP and stdio, negotiated protocol revisions, structuredContent, tool annotations and official SDK parity | **partial/gated** | tools/mcp/verify.mjs; official SDK compare REST/stdio/HTTP |
| Linux UI & theme | `web/src/pages/{Overview,Applications,Services,Updates,Configuration,ConfiguredShares,AgentAccess}.svelte; web/src/lib/*` | `linux-web/src/{App,Overview,Applications,Services,Updates,Shares,Processes,Resources}.svelte` | Every screen and typed editor, banner/alerts, activity, mobile layout, theme persistence, Agent Access and backup recovery | **partial/gated** | tools/ui/verify.mjs and browser workflows against live app |
| Declarative parser, ownership and recovery | `internal/declarative/{validate,store,preflight,coordinator,journal,artifact_recovery}.go` | `src/core/declarative/*` | One YAML with server settings; duplicate-key and references; ownership sidecars; journal, interrupted apply, preserve unmanaged resources | **partial/gated** | Go declarative unit tests + Rust fixtures + native fault/restart tests |
| Quadlet / Podman Compose / Docker Compose | `internal/declarative/{quadlet,compose_adapter,podman_readiness}.go` | `src/core/declarative/{quadlet,compose}.rs` | User systemd rootless launch, networking, lifecycle, runtime-vs-boot, logs, native ownership, partial group status | **partial/gated** | Native disposable guest Podman/Docker; boot and cancellation |
| KVM/libvirt & Incus | `internal/declarative/{libvirt,incus,incus_backup,libvirt_export}.go` | `src/core/declarative/{libvirt,incus}.rs` | Persistent guest definitions, pool/network/disk readiness, stopped edits, export/boot and privilege ownership | **partial/gated** | Disposable guest provider tests; conflicts/restart |
| Storage and file shares | `internal/declarative/{provision,shares}.go; internal/modules/shares/{definitions,files,nfs,samba,password}.go` | `src/core/declarative/{storage,shares}.rs` | Owned no-clobber directory/new raw/qcow2 storage; mount checks; Samba/NFS managed fragments and password/setup helper | **partial/gated** | Native disposable share/filesystem tests; candidate-validation failure |
| Optional Linux helper | `internal/admin/{client,helper}.go; cmd/server-man-admin/main.go; docs/operations/privileged-helper-verification.md` | `src/platforms/admin.rs; src/bin/maestro-admin.rs` | Non-root workload server, root-owned optional helper, restricted sudo -n, typed validated verbs, dynamic denied reasons | **partially implemented (not verified)** | Unit negative protocol tests drafted; root identity/sudo/permission and live revocation native pending |
| Production installation, upgrade | `internal/installation/install.go; cmd/server-man-install/main.go; install.sh; packaging/systemd/*` | `scripts/setup-linux.sh (lab only); helper binary present` | Opt-in helper, explicit install modes, reviewed package plan/skip, service under workload UID, config-preserving reinstall/upgrade/revoke | **not started** | Disposable Ubuntu and Debian no-helper/with-helper/reinstall acceptance |
| Backup native/capture boundary | `internal/declarative/backup_{inventory,native,prepare,verify,publication,recovery}.go; internal/declarative/artifact_recovery.go` | `missing` | Private checksum manifest; native export/capture and interrupted recovery; capture is NOT restorable | **not started** | Go backup test matrix + native stopped/running workloads + restart |
| Router compile-time isolation | `Cargo feature routing, router build baseline` | `Cargo.toml; src/api/mod.rs; src/platforms/mod.rs` | Router root unchanged; helper executable and all Linux tool paths excluded with no Svelte/dependencies | **implemented but awaiting verification** | cargo tree/check for ASUS; ARM IPK size and matching hardware regression |

## Privilege classification and negative tests

| Operation | Without helper | With helper | Independent guards | Evidence |
| --- | --- | --- | --- | --- |
| Monitoring, process/history, readable configuration | Current non-root permissions | Same; never sudo | Auth, data scope, output bounds | Rust source only; browser and native pending |
| User systemd lifecycle and logs | Direct `systemctl --user` | Direct (never via root helper) | Exact validated unit, configured user allowlist, canonical ID, job lock | Source and fixture tests; native pending |
| System systemd read/log | Direct read where host permits | Direct read | Auth and log bounds | Source only; partial reads pending |
| System systemd write | **Denied** with reason when helper unavailable | `sudo -n -- /usr/local/libexec/maestro-admin`; trusted policy allowlist and canonical ID | Workload-config allowlist **and** helper root policy; no managed units/maestro.service | Client/helper code committed; no native validation |
| Package metadata and cached status | Direct read where permitted | Direct read | Manager detection, parsed bounds, correct stale markers | Fixture tests only |
| Package refresh/upgrade/install/dependency setup | **Denied**; cached reads continue | Conditionally authorized by explicit workload opt-in and private root `package_writes` policy | Policy, exact native programs/verbs, validated package names, lock, bounded output | Implemented in source, not native validated |
| Rootless Podman, user-owned files, eligible Docker/Incus/libvirt sockets | Direct ordinary account | Same without unnecessary sudo | Native ownership, profile, readiness, journal, safe no-replace | Provider source exists; native pending |
| Protected Samba/NFS publication/setup/password | **Denied**; reads/drafts still allowed | **Currently denied pending helper verbs** | Caller binding, private candidate validation, rollback, explicit confirmation | Not implemented in helper |
| Helper installed/removed/revoked/unsafe/protocol mismatch | Graceful unavailable reason, never root fallback | Re-probe before each protected write, helper revalidates | Root-owned nonwritable parents, fixed paths/sudoers, UID, bounded JSON | Source/unit-negative only; native revocation pending |
| Root router mode | No helper or sudo | Not applicable | Compile-time feature isolation, legacy auth/root behavior | Static feature gate only; hardware/CI pending |

**Known root assumptions in the pre-reconciliation branch:** package write readiness was `geteuid()==0` and directly executed package managers; system-scoped systemd action used the daemon's credentials; Linux main had no mandatory non-root check. The current branch rejects a root-running *production* Linux serve, routes opted-in systemd writes through the helper, and fails closed for package mutations. These changes require unit/build/native acceptance and must be audited for all legacy Maestro Linux mutators (NGINX, certificates, firewall, service/upgrade paths); existing-router API endpoints exposed in Linux are not automatically authorized host writes.

## Next ordered implementation / validation backlog

1. **Privileged boundary:** build/test the new helper and its contracts; implement complete narrowly scoped native package/dependency/share/password verbs, helper identity/protocol/policy and safe installer opt-in/revocation; add denied/allowed/failure/restart tests. Validate system/user systemd in two isolated native modes. Do not enable writes by dropping guards.
2. **Core/auth:** shared registry complete operations/schema/error/write/confirmation semantics, credential bootstrap, password verification, persistent sessions, per-action discovery (including MCP) and non-root regression suite.
3. **Declarative correctness:** one authoritative Linux YAML and server settings; typed model/edit/history/revert; app and share native ownership/readiness, journaling and interrupted recovery.
4. **Providers/storages/shares:** rootless Quadlet+Compose, Docker, libvirt, Incus, storage clones, Samba/NFS; preserve runtime-vs-boot, no-clobber native ownership; validate disposable guests.
5. **Release/access:** production two-mode installer/reinstall/upgrade; UI full reference pages; MCP Streamable HTTP and stdio with official SDK schema parity; completed reference backup inventory, checksummed capture, verify/publish/release and interruption recovery.
6. **Evidence closure:** capture per-row Go baseline fixture, Rust unit/API/MCP/UI logs, no-helper and with-helper guest runs, router build graph/IPK/hardware and signoff. Do not mark M2 complete or claim full restorable backup (not implemented in Server Man reference).

## Current increment evidence (2026-11-23)

- Repository reviewed at plan baseline `e4a78158f40e2eee83a2b4ded75f941536d75ef5`. Existing WIP was preserved.
- Commits: `584ccc22ad89a7b5145c876ed67467d509c06d73` (Linux-only helper and non-root guard), `70b9a8cffc58366e30e4f762d1c3c4f7f8d92ef4` (systemd delegation/package fail-closed), `acf9a6ff774bdd47184fa7ea8a3d141a8770866e` (HTTP/MCP capability response and UI reasons).
- **Verified:** connector-confirmed branch commits and source-level path review only. **Not verified:** Rust compilation, fmt, clippy, unit/API/browser checks, root-owned helper install, non-root native acceptance, router IPK/hardware, release, CI. Runtime tools are unavailable in this connector-only session. No positive native operation is claimed.

- Follow-on commit adds `services_details` allowlisted read-only properties, explicit system/user scope, API schema, initial source tests and Svelte inspection. This remains `modeled` pending Rust build/fixture/API/browser/native review.

- Linux build script now exports separately checksummed `maestro-admin-<target>` **without installing or authorizing it**. `docs/linux.md` documents the fixed helper/policy/sudoers contract as a WIP, not production acceptance.

- Shared HTTP/MCP service write actions now require `confirm=true` for `stop`, `restart`, `reload`, `disable`; discovery describes these as destructive and MCP tool annotations expose read-only/destructive hints. User-scope actions follow the same confirmation policy. This is a source change only: test/SDK acceptance remains pending.

- The root helper now independently rejects destructive system-service JSON without `confirm=true`, in addition to the shared REST/MCP check. Authorization remains the bounded caller UID and root policy; a separate privileged sudo-granted workload account should itself be treated as highly trusted.

- Helper client output is now read-bounded at 16 KiB plus one byte before decoding; it never calls an unbounded `wait_with_output`. Client stat-checks the private root policy before sudo, and the helper rechecks internally. The router-only build rejects accidental runtime Linux mode rather than exposing incomplete Linux functionality. No binary/guest verification yet.

### Follow-on source implementation (validation still pending)

- `f68ebb6b2cd4`: Linux helper-only fixed native package operations and pinned reference package-name/dependency mappings, including all four package managers and root-owned isolation; no dynamic command, version, repository selector or script launcher.
- `2e9d498c6180`: versioned helper request adds `package_refresh`, `package_upgrade`, `package_install`, `dependency_install` with independent root policy `package_writes`, confirmation and bounded results.
- `4f59f1c2ec53`, `04526d3c36fe`, `79387c4541d4`: non-root coordinator and HTTP/MCP jobs route protected package operations through helper, preserve cached reads, add named/dependency API schemas and Updates UI controls.
- `f83f16732139`, `652d0bff698d`: owner-only persistent session store, 256-bit browser cookies, expiry checks, token-rotation invalidation and durable revocation; first-run administrator/password support remains missing.
- **All of the above are implementation/source-level only.** Compilation, lint, browser, official MCP SDK and native two-mode package transactions are pending; tests in source are not evidence of execution.

## Additional Phase 2 source increments — 2026-11-23 (not validated)

Implementation continued on `feature/linux-phase2`, without resetting earlier work. The initial audit was expanded with the following source-level work. **No item here is marked accepted or native-verified.**

| Area | New source paths | State | Explicit outstanding work |
| --- | --- | --- | --- |
| Optional privileged package operations | `src/platforms/admin_packages.rs`, `src/platforms/admin.rs`, `src/platforms/package_updates.rs` | Implemented in source; unverified | Review native manager exit codes/locks and dependency mappings, run denied/positive tests in both modes, verify interrupted package transactions |
| Named package and dependency UI/API/MCP | `src/api/tools.rs`, `linux-web/src/Updates.svelte` | Implemented in source; unverified | Protocol/contract and browser tests; no unsafe fallback |
| Durable browser sessions | `src/api/linux_session_store.rs`, `src/api/sessions.rs` | Implemented in source; unverified | Native persistence/revocation, cookie/Host/Origin test, administrator password setup and single-authority integration |
| Linux non-root installer and helper opt-in | `scripts/install-linux.sh`, `scripts/build-release.sh` | Partially implemented; unverified | Clean/install/reinstall/upgrade/revocation in disposable Ubuntu/Debian, checksum authenticity, reviewed backend prerequisites, service/sudoers rollback and least-privilege audit |
| MCP HTTP + stdio | `src/api/mcp.rs`, `src/bin/maestro-mcp-stdio.rs` | Partially implemented; unverified | Official SDK on both transports, protocol headers, error/structured results, cancellation and authentication; no Streamable HTTP native acceptance |
| Configuration operations | `src/core/declarative/edit.rs`, `src/api/tools.rs` | Partially implemented; unverified | Typed native editor coverage, next-start mode, Server Man's server/access stanza, per-resource health and complete historical workflows |
| Configuration, Activity, Agent access | `linux-web/src/{Configuration,Activity,AgentAccess}.svelte`, `src/api/linux_ui.rs` | UI/registry source present; unverified | Browser build/interaction/a11y, typed editors for all kinds, recovery banners and native testing |
| Backup input inventory | `src/core/declarative/backup_inventory.rs`, `src/api/tools.rs` | Modeled metadata-only; unverified | Native VM dependencies, private manifest, actual checksummed capture, verification/publication, interruption/recovery. **Inventory is NOT a backup.** |
| Samba password | `src/platforms/admin_shares.rs`, `src/platforms/admin.rs`, `linux-web/src/Shares.svelte` | Implemented in source behind independent default-deny policies; unverified | Native Samba tests, caller UID binding, no secret leak, permission revocation, helper opt-in and rollback |
| Protected Samba/NFS publication and setup | Reference `internal/modules/shares/definitions.go` | **Not implemented** | Root-owned candidate validation, protected managed fragments, atomic publication/rollback, NFS, native service setup |
| Router runtime/dependency isolation | `asuswrt` feature gates unchanged, new Linux-only binary targets | Not validated | Rust feature graph, cross-build/IPK, on-router resource and API parity |
| Full Server Man backup/capture/recovery | Reference `internal/declarative/backup_*.go` | **Not implemented** beyond metadata inventory | All native capture, verification, publish/release/recover tests and UI/API/MCP |

**Important corrections during static review:** the initial Linux service unit's `NoNewPrivileges=yes` and empty capability bounding set conflicted with optional `sudo -n` escalation; the reviewed installer now preserves a non-root main account while allowing *only* its explicitly scoped sudoers helper. The embedded Linux asset map now includes all new lazy pages. Helper probe struct fields and cancellation rules for protected package/typed writes were corrected. Native code behavior is not implied by these corrections.

**Known remaining blockers and divergence:**
- The `server` settings in pinned Server Man's authoritative YAML still need migration; Maestro's Linux access/server config must not remain in competing TOML/YAML authorities.
- First-run administrator password hashing, credential login/setup, durable administrator sessions and revocation must be integrated with the single-authority configuration.
- Rootless Compose, Docker, libvirt, Incus, managed disk cloning, share publication and VM data acceptance remain unproven. Some providers intentionally gate native mutation.
- `managed_resource_save` currently requires a reviewed fingerprint and supports only immediate apply; `next_start` fails closed rather than pretending parity. The typed UI currently covers only storage/share forms.
- The read-only backup inventory omits actual data capture/verification/publish and explicitly marks native dependencies unknown.
- The API includes fewer than the 60 source-declared Server Man public tools. Verify all reference schemas and response shapes with official SDK fixtures and complete the missing tools.
- No Rust toolchain, npm build, integration or native test environment was available for these source commits. **Do not deploy the new helper/installer or accept Phase 2 based on these changes.**

**Validation recovery instructions:** use a disposable checkout on this exact branch; run formatting, Clippy, `cargo test --no-default-features --features linux --all-targets`, `cargo test --no-default-features --features asuswrt --all-targets`, Linux UI `npm ci && npm run build`, separate release binaries (including helper/stdin MCP), API/browser/official MCP fixtures, restricted sudo helper-free/installed guest tests, then resource measurements on ASUS hardware. Fix the resulting compiler/browser/native findings before enabling production operations.

# Server Man → Maestro Phase 2 functional parity ledger

**Status:** active and incomplete — none of the reference capabilities is accepted.
**Frozen reference:** [Server Man commit c1a9f32](https://github.com/vm75/server-man/tree/c1a9f32f5898146ae7ac339043b910b8e2a2c438), audited source and tests on 2026-11-23.
**Maestro branch:** `feature/linux-phase2`; updated plan: [modular-platform-ui-migration.md](modular-platform-ui-migration.md).
**Requirement:** cover *every* implemented reference feature, not just matching tool names; 100% behavioral parity with helper, graceful reduction without it. No Server Man features are waived.

## Evidence rules and current result

- States: `not started`, `modeled` (Rust data/handler only, unproven parity), `gated` (present but deliberately blocks mutations pending security/native validation), `implemented` (domain behavior present, tests incomplete), `fixture verified`, `native verified`, `accepted`. **No `accepted` rows at this revision.**
- Every operation needs reference tests, equivalent arguments/schema/errors/permission policy, Rust unit/API contract, UI path, REST/MCP, a no-helper run and helper-enabled run. For native mutations require disposable Linux guest checks, failure/restart/recovery and observed effects. Source review or a green mock test does not establish native acceptance.
- This is an initial source-level ledger, **not** a complete baseline contract extraction. Many parameter sets are explicitly marked `inspect schema` and need full reference field-by-field fixtures. The Phase 2 gate stays open.
- **Historical tool-count discrepancy (registry now resolved below):** the current plan reports **55** official-SDK tools; the module source registration paths below expose **60 distinct public registry operation names** when all modules are assembled (excluding three helper-only `shares_definitions_*` verbs). Re-run the pinned Go registry/official SDK and compare conditional modules and negotiated discovery before treating either count as authoritative. Every source entry is retained below until reconciled.
- Maestro currently has **50** named Linux tool registrations (42 enum entries plus six systemd and two updates), some named differently and with narrower behavior. No mapping from `workloads_*` to `managed_*` is accepted merely by analogous names.

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
| `managed_directory_browse` | path optional, max 4096 characters | Sorted visible directory/file names, cap 256, metadata only as workload account. | **fixture verified**; Rust unit and MCP integration checks; Linux UI build passes; native browser acceptance pending |
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
- First-run administrator hashing/login/setup and durable revocation are now fixture verified with the unified YAML; authored-comment preservation and native browser/installer acceptance remain open.
- Rootless Compose, Docker, libvirt, Incus, managed disk cloning, share publication and VM data acceptance remain unproven. Some providers intentionally gate native mutation.
- `managed_resource_save` currently requires a reviewed fingerprint and supports only immediate apply; `next_start` fails closed rather than pretending parity. The typed UI currently covers only storage/share forms.
- The read-only backup inventory omits actual data capture/verification/publish and explicitly marks native dependencies unknown.
- The API includes fewer than the 60 source-declared Server Man public tools. Verify all reference schemas and response shapes with official SDK fixtures and complete the missing tools.
- No Rust toolchain, npm build, integration or native test environment was available for these source commits. **Do not deploy the new helper/installer or accept Phase 2 based on these changes.**

**Validation recovery instructions:** use a disposable checkout on this exact branch; run formatting, Clippy, `cargo test --no-default-features --features linux --all-targets`, `cargo test --no-default-features --features asuswrt --all-targets`, Linux UI `npm ci && npm run build`, separate release binaries (including helper/stdin MCP), API/browser/official MCP fixtures, restricted sudo helper-free/installed guest tests, then resource measurements on ASUS hardware. Fix the resulting compiler/browser/native findings before enabling production operations.

## Local validation recovery — 2026-11-23

The branch is now being checked in a local Rust/Node environment. These results
supersede the earlier environment-unavailable statements only for the checks
listed here; native parity and production installation remain unaccepted.

- Corrected Linux compilation: management job tool names are already string
  slices; MCP response errors now support test diagnostics.
- Applied Rust formatting to the previously unformatted Phase 2 additions.
- Linux and ASUS all-target Clippy checks pass with warnings denied (Rust 1.99).
- Linux UI `npm run check` reports zero errors/warnings and `npm run build`
  succeeds. The typed resource apply screen uses the shared job contract and
  reports immediate failures and cancellation as well as polled failures.
- Linux library suite passed 192 tests (one ignored); the corrected HTTP MCP
  integration flow passed with the required Accept and initialization fields.
  A subsequent focused session suite passed all three tests, including a new
  malformed Unicode digest regression that previously could panic at startup.
- The backup inventory fixture now lets the workload store create its required
  private directory rather than precreating it with public permissions.
- Offline ASUS UI composition and JavaScript checks pass.
- Both full profile suites are being rerun with loopback access because the
  default sandbox denies the TCP binds needed by nginx integration fixtures.
  These runs and pinned Rust 1.85 validation must be recorded before acceptance.

No native helper, installer, provider, official MCP client, or router acceptance
is implied by these local build and test results. The remaining parity rows
and M2 gate stay open.

## Frozen registry contract extraction — 2026-11-23

The pinned Go `app.Registry` was executed with a non-nil monitoring sampler,
a temporary state/configuration directory, and an Executor that rejects every
command. Both `PrivilegedOperations=false` and `true` register **60 tools**, with
identical serialized schemas. This resolves the source registry count; the
historical 55-tool SDK report is not the current frozen registry contract.
Official-client transport acceptance remains open. No management tool was invoked.

Exact names, descriptions, modules, read/write/destructive flags and input schemas
are captured in `test-fixtures/phase2/server-man-tools.json`. This replaces
“inspect schema” as the source for tool argument contracts, but does not establish
behavioral parity. `test-fixtures/phase2/extract-reference-contract.go` reproduces
the extraction: copy it into `cmd/maestro-contract/main.go` in a disposable checkout
of the frozen Server Man commit, then run `go run ./cmd/maestro-contract`. It emits
both modes; the fixture stores their verified identical tool list once.

A comparison of the reference MCP dispatcher also exposed silent acceptance of
non-string `tools/list` cursors. Maestro now returns JSON-RPC invalid-parameters
errors for incorrect cursor types and unsupported pagination, covered through the
HTTP integration flow. That focused flow passed on Rust 1.85.0.

The default-toolchain full Linux and ASUS suites completed successfully, including
all 13 ingress tests per profile. The session-digest regression was verified
separately after the full Linux run. Sequential Rust 1.85 full suites are in
progress; no minimum-toolchain full-suite acceptance is claimed yet.

The sequential Rust 1.85 Linux full suite subsequently passed: library (193),
main binary (225), stdio (1), API (33), firewall (5), ingress (13), nginx render
(3), and restore CLI (2), with two ignored unit-test instances. This run includes
the session-digest and cursor regressions. Further MCP parser parity changes
(empty methods, response envelopes, malformed notifications) have focused tests;
their verification is tracked separately from this full-suite result.

## Directory browser implementation — 2026-11-23

`managed_directory_browse` now has a shared HTTP/MCP operation and a Configuration
UI directory browser. It reads entry names/types as the workload account, defaults
to `/`, normalizes relative paths from `/`, excludes hidden names, classifies
symlink entries as files, and returns sorted directory/file names with the
reference's combined 256-entry cap. It retains at most 257 candidate entries
while scanning rather than allocating the full directory listing. No file
contents, helper calls, native commands or filesystem mutations are involved.

The focused tests cover metadata-only output, hidden names, symlink classification,
path normalization and stable limiting. The MCP integration checks discovery
against the frozen path schema and exercises a temporary directory listing.
Linux UI type checking and production build pass with the new browser. Native
browser interaction and full share/storage selection workflows remain open.

The ASUS full suite on Rust 1.85 also completed successfully. All three focused
MCP parser regression tests passed on Rust 1.85. These results do not close any
native mutation, installer, privilege or router hardware acceptance gate.

The directory browser's focused Rust 1.85 unit tests and HTTP/MCP integration
passed, including exact equality of its input schema with the frozen fixture.
Rust 1.85 Clippy additionally identified the session encoder's format/collect
pattern; it now emits the same hexadecimal bytes directly, with session
persistence/revocation tests rerun after that change.

## Single-authority settings prerequisite — 2026-11-23

`src/core/declarative/server.rs` now models the frozen reference's server fields
and validates their bounded administrator hash/username format, local libvirt
URI, systemd scope, package selection, fixed helper paths, sample interval,
listener and public-origin rules. Its separate reader validates YAML structure
and server settings while leaving invalid workload definitions repairable.
Defaults are supplied explicitly rather than read from process-global environment.
The private startup-file loader bounds input, rejects final symlinks and special
files, and requires owner-only permissions and workload-account ownership.

This is an integration prerequisite, **not a completed configuration migration**:
current startup still reads TOML, the workload parser still rejects `server`, and
administrator bootstrap/password authentication remain absent. Do not enable
server edits or claim the one-authority requirement until startup, store location,
restart-required planning, auth and installer all use this same YAML document.
The new module is not connected to a production startup path yet.

## Explicit authority store prerequisite — 2026-11-23

The atomic YAML store now accepts a selected absolute YAML filename through
`Store::at_path`, preserving revision conflicts, private-file protections and
history. Apply/recovery reads and publication use that selected name. Recovery
journals bind the pending operation to its authority filename; legacy journals
without that field retain `workloads.yaml` identity. A different authority cannot
inspect/recover its candidate or publish while that operation is pending.
Backup inventory reports the selected authority path instead of a hardcoded name.

Rust 1.85 declarative tests passed (50 passed, one ignored), including selected
filename publication/history/stale revisions and cross-authority pending-operation
rejection. Linux all-target Clippy with warnings denied passed. Architecture
notes describe the new recovery identity. Runtime callers still choose the legacy
workload store location; connecting startup, server edits and auth remains open.

## Unified YAML runtime wiring — 2026-11-23

Linux `AppConfig::load` now recognizes selected absolute `.yaml`/`.yml` paths.
It loads bounded private server settings and a private bearer token, maps runtime
state/nginx paths into the workload state directory, and makes HTTP and stdio MCP
use `Store::at_path` for that same authority. Legacy TOML mode retains its store.
The workload parser validates and preserves `server`; native plans reject changes
to it pending restart-aware editing. Monitoring interval, default systemd scope,
package-manager selection and local libvirt URI use the selected server settings.
Configured administrator credentials currently block startup so password protection
cannot silently degrade to token browser login before auth integration.

The initial Linux Rust 1.85 library suite passed (204 tests, one ignored), Linux
all-target Clippy passed, and the ASUS all-target compile check passed. A focused
startup/store fixture verifies exact shared YAML bytes, workload revision/history,
settings preservation, stale-edit protection and readable invalid workloads. Its
HTTP extension is being validated separately. Installer conversion, password auth,
restart-aware edits, full permission mapping and native acceptance remain open.

The startup/store fixture's authenticated HTTP extension also passed on Rust
1.85: `workloads_get` returned the exact selected-authority revision and invalid
workload text for repair, without creating a competing workload document.

## Administrator authentication integration — 2026-11-23

Unified-YAML installations now expose bootstrap-token `POST /api/auth/setup`,
password `POST /api/session`, and actual administrator state from `/api/auth/mode`.
The Linux UI selects token/setup/password forms from that state. Credentials use
the reference's fixed PBKDF2-SHA256/600000-iteration, 16-byte-salt, 32-byte-key
format, implemented with the Linux-only direct `ring` dependency. The router
profile does not enable this new direct dependency. A Python hashlib fixture
checks compatibility independently of generated password hashes.

Setup writes once under the shared authority lock, retains invalid resource
values for repair, rechecks revision, and records history. Durable browser
session scope includes administrator credentials and bearer-token identity;
setup clears bootstrap cookies, password login replaces token browser login,
and agent bearer access remains supported. Peer-address attempts are bounded
(five per five minutes, at most 128 tracked addresses); password computations
run off the async runtime with at most four in flight. Forwarded headers do not
supply the peer identity.

The focused Rust 1.85 HTTP flow passed setup, bootstrap-cookie invalidation,
wrong/correct passwords, forbidden token browser bypass, persistent administrator
sessions after restart, logout and rate limiting. Linux UI typecheck/build pass.
The library suite and profile/lint regression checks are being rerun after the
concurrency bound. Native browser/installer acceptance remains open. Setup currently
re-serializes YAML and does not preserve authored comments; this reference behavior
gap remains to be resolved before authentication/configuration parity acceptance.

The updated Rust 1.85 Linux library run passed all 205 tests (one ignored),
including the complete administrator HTTP flow and independent PBKDF fixture.
Linux all-target Clippy with warnings denied and ASUS all-target compilation
passed. Existing Linux API integration tests are being rerun as a separate
regression check; native browser/installer acceptance is still required.
The subsequent Rust 1.85 Linux API integration suite passed all 33 tests,
including legacy token-cookie login/revocation and the shared MCP flow.

## Authored YAML preservation during bootstrap — 2026-11-23

Administrator setup now edits credential fields through bounded libyaml source
spans instead of serializing the whole document. It retains authored comments,
resource text, UTF-8 text/BOM and CRLF line endings. Coverage includes absent,
null, block and flow `server` mappings, existing empty credential scalars and
empty block scalars with header comments. The original revision remains subject
to the same private-store lock, history and concurrent-edit checks.

Both focused source-edit tests and the full administrator HTTP lifecycle fixture
passed on Rust 1.85, and Linux all-target Clippy passed. A subsequent complete
library run is validating the actual store-level comment assertions. This closes
the earlier bootstrap comment-loss implementation gap; native browser/installer
acceptance and restart-aware server editing remain open.
The complete Rust 1.85 library run passed 207 tests (one ignored), including
store-level comment preservation and administrator session lifecycle. The final
Linux all-target Clippy rerun also passed with warnings denied.


## Restart-aware server configuration — 2026-11-23

`managed_config_plan` and `managed_config_save` now expose the frozen reference's
exact input schemas. Effective desired server settings are compared with the
running settings and live bootstrap credentials; default-equivalent authoring
changes do not create a false restart requirement. Server settings must be saved
separately from resource changes. Settings-only saves publish through the shared
journal/store and report `restartRequired`; subsequent managed mutations refuse
until startup reloads the authority. Legacy TOML mode refuses server-setting edits
rather than silently ignoring them. Historical journal plans default the added
restart field when it is absent.

The Configuration page now includes a raw authoritative-YAML review/save flow,
tracked job completion, apply-mode selection and restart explanations. Native
resource publication in `next_start` mode is still **not implemented**; both API
and UI reject it explicitly without publication. Settings-only next-start saves
are verified, not evidence of native deferred-provider parity.

Rust 1.85 library tests pass (208 passed, one ignored), including separate server
and resource edits, exact tool schema fixtures, next-start settings publication,
stale-runtime mutation denial, reload readiness and bootstrap without false
restart detection. Linux all-target Clippy, ASUS all-target compilation, Linux
UI checks/build pass. Existing API integration regressions are being rerun.
Native browser/installer acceptance, invalid-resource repair planning and complete
provider apply modes remain open; no parity row is accepted by these checks.
The subsequent Rust 1.85 Linux API regression suite passed all 33 tests.

## Compose health and dependency contract — 2026-11-23

The typed model now validates the frozen reference's health command forms,
argument/count bounds, retry bounds and positive Go-style duration grammar,
including signed-nanosecond overflow. Healthy dependencies require an existing
upstream healthcheck, and unknown/self dependencies are rejected before publication.
Compose renders the health command, interval/timeout/start period/retries and
`service_healthy` conditions, preserving literal `$` values through Compose escaping.
Other unsupported container fields and native ownership gates remain explicit.

`test-fixtures/phase2/health-durations.json` captures Go standard-library results
from a standalone copy of the frozen `containerDuration` predicate; its companion
Go source reproduces that fixture. It is not a native provider or pinned-package
integration run. Rust tests consume the captured cases and exercise rendered
health/dependency fields, missing healthchecks and malformed command/timing input.

Rust 1.85 library tests pass (212 passed, one ignored), Linux all-target Clippy
passes with warnings denied, and the ASUS all-target compile check passes. API
integration regressions are being rerun. Native Compose lifecycle, ownership,
boot policy and deferred apply acceptance remain open; this increment establishes
validation/rendering evidence only.
The subsequent Rust 1.85 Linux API integration regression suite passed all 33 tests.


## User-directed YAML convergence and migration — 2026-11-23

The user explicitly selected YAML as the single operator configuration format
on both platforms, with a one-time migration. Earlier notes retaining TOML
runtime readers are superseded. Main configuration and managed nginx global/site
inputs now use YAML; the TOML dependency is retained only for `migrate-config`.
The command preserves values and relative path semantics, records exact private
`.toml.bak` originals, publishes without overwriting different destinations,
retires active TOML sources, and supports idempotent retries. Existing legacy
Linux layouts are format-converted without silently changing their runtime paths.
New native Linux installations continue using the unified authoritative YAML.

The router installer migrates while stopped, examples/fixtures and packaging
select YAML, Python lab tooling declares PyYAML, and local diagnostic scripts query
configuration through Maestro instead of parsing TOML. Diagnostic archives omit
the private primary configuration. Native nginx files, state JSON, certificates
and Cargo build metadata retain their appropriate formats.

Both complete Rust 1.85 profiles passed after format conversion, including API,
nginx ingress/render and restore regressions. Linux's new migration CLI test also
passed separately. Both profile Clippy checks, the offline UI checks, IPK packaging
smoke test and 16 Python nginx tests (including live loopback nginx) passed.
A final migration directory-safety regression and Linux smoke run are being
completed. These checks do not establish real-router footprint or native helper
acceptance, and no production files were migrated during development.
The final directory-safety migration suite passed all five cases. The updated
YAML-configured unprivileged Linux smoke test passed with real temporary nginx,
authentication, metrics and API. Serverless native Linux YAML dispatch and
router-only rejection tests passed separately. Migration CLI validation succeeded
under both feature profiles. No deployment or actual router migration was run.

## Bounded private configuration history — 2026-11-23

History now follows the reference's eight-revision retention and recency behavior,
including refreshing a reused base. `managed_config_history` returns revision,
UTC modification time, validity and safe recovery errors rather than a hash-only
list. Reads verify private files and content hashes, expose damaged/invalid
entries without their contents, and reject unexpected directory entries. Pruning
keeps foreign files untouched and uses bounded scans. Explicitly selected YAML
authorities have separate filename-scoped history; legacy workload stores retain
their existing history directory. No reference history is declared accepted.

The modern history schemas match the frozen fixture. Legacy revision-list clients
retain their response shape, while both modern and legacy history paths enforce
management-write policy. Configuration UI displays timestamps/validity and
explains disabled history. Source tests cover retention/order, reused bases,
corruption, invalid YAML, symlinks, unknown files and cross-authority isolation.

Rust 1.85 Linux library tests passed (223 passed, one ignored), and all 34 API
integration tests passed, including metadata/legacy shapes and disabled-policy
checks. Linux UI checks/build passed. Focused final schema/store tests and Clippy
are being completed after the foreign-file pruning review. Official-client,
browser/native acceptance and complete revert/supersession behavior remain open.
The final 11 store tests, exact history schema HTTP fixture, and Linux all-target
Clippy passed on Rust 1.85. Foreign files remain unchanged during retention writes.


### Configuration revert tools and UI increment

The reference-named `managed_config_revert_plan` and `managed_config_revert`
tools now expose schemas matching the frozen registry. Verified historical bytes
flow through the shared authoritative configuration preview/apply path, including
revision conflicts, provider readiness, destructive confirmation, restart checks,
and tracked jobs. History entries offer private YAML inspection, plan review and
confirmed restoration in the Linux UI. An outgoing revision is archived.

This is not accepted reference parity: pending-transition supersession and native
`next_start` publication remain unimplemented. Both revert endpoints refuse a
pending journal, preserving its candidate and unresolved removals. Native/browser
and official-client acceptance remain open. Focused tests cover exact-byte
restoration, retained outgoing history, stale revisions, extra fields, missing
confirmation, disabled policy and preservation of pending journals. The two new
input schemas are compared directly with the frozen fixture.

Validation for this increment: Rust 1.85 Linux library tests passed (224 passed,
one ignored), all 35 API integration tests passed, and Linux all-target Clippy
passed. Linux UI type checking/build and formatting checks also passed. These
checks do not establish native or browser acceptance.

### Interrupted-apply supersession increment

Revert can now supersede a published pending apply after inspecting removals
that would otherwise disappear from tracking. Native state already matching the
selected historical definition is skipped. An unchanged resource affected by the
prior transition must already match its published definition; other unknown or
drifted states remain gated pending broader native reconciliation. Prepared and
unpublished legacy transitions must be recovered first.

Version 3 journals use two bounded candidate slots and retain a non-nested prior
journal during replacement preparation. A crash before replacement YAML commit
restores the prior journal/candidate; a crash after commit reconciles the new
transition. Both modern and legacy revert paths use this coordinator. A resume
job reports whether restoring prior intent left additional recovery pending.

Six focused source tests cover matching-state skips, unresolved and completed
removals, both publication crash windows, repeated slot alternation and invalid
replacement rejection. These are fixture-provider tests, not native acceptance.
The preceding 65 declarative tests (one ignored) and all 35 API integration tests
passed; the final six-test supersession subset also passed after the two added
candidate-slot checks. Native next-start publication, broader drift repair,
official clients, browser acceptance and platform gates remain open.


### Persisted next-start publication increment

The coordinator now persists `now`/`next_start` in version 4 journals and routes
preflight, publication, revert/supersession and recovery inspection through the
recorded mode. Old journals retain `now` semantics. Provider defaults reject
unsupported deferred operations before intent or configuration publication.
The modern YAML save, typed-resource save and revert tools accept both modes;
the UI exposes them and identifies the mode of pending recovery.

Managed directories support either mode with the same ownership markers and
data-preserving removal. Rootless Quadlet next-start updates verify owned prior
files and loaded units, publish exact new definitions, and reload the user
manager without lifecycle commands. Active units with retained source files may
keep running; active removals and transitional states remain blocked. Native
Linux YAML now derives its Quadlet directory from the account's XDG config root
(or ~/.config) without startup filesystem writes. The directory must be prepared
privately before use.

Focused tests exercise active-update publication with a recording systemctl
fixture, refusal of active removals, immutable deferred recovery mode, unsupported
provider refusal before writes, and an HTTP YAML-save/typed-edit/revert round trip
that preserves existing directory data. UI checks/build passed. This is not native
acceptance: real user-manager/Podman generation, runtime fingerprint convergence,
boot policy, other backend publication and official-client/browser gates remain
open. Earlier blanket next-start-unavailable statements describe the prior
implementation and are superseded only for the paths above.

Final validation for this increment on Rust 1.85: 234 Linux library tests passed
(one ignored), all 36 API integration tests passed, and all-target Clippy passed.
Linux UI type checking/build, Rust formatting and documentation audit passed.
The Quadlet test uses a recording executable, not a live user manager or Podman.


### Durable apply status and safe retry increment

`managed_apply_status` and `managed_apply_retry` now expose input schemas compared
with the frozen registry. Status is read-only metadata for both pending and
completed transitions and remains available when management writes are disabled.
Retry requires explicit confirmation and management writes, retains the recorded
apply mode, and refuses automatic replay of every interrupted removal unless
native observation already verifies absence. The legacy explicit fingerprint
resume path retains its prior behavior. Already-complete retry is idempotent.

Version 5 journals retain a failed resource and a UTC update time. Status derives
per-resource registered/applying/pending/failed progress and a fixed diagnostic;
it does not persist native error output. New failure metadata on older journals
does not change their publication ordering. Applications displays durable progress
and uses the reference-named safe-retry endpoint. The broader native/runtime,
output-contract, official-client and browser acceptance gates remain open.


The broader run exposed transient lock retention after storage publication.
Authoritative, storage, Quadlet and Compose directory locks now release explicitly
at transaction scope exit rather than relying only on descriptor closure. A
regression test retains a duplicate descriptor across scope exit and verifies
that another publication can acquire the lock without waiting for that duplicate
to close.

Validation after the lock-release correction: Rust 1.85 Linux library tests
passed (237 passed, one ignored), and all 36 API integration tests passed. Coverage
includes exact new input schemas, argument/confirmation/write-policy checks,
idempotent completed retry, durable failure readback, automatic removal refusal,
owned-update retry, legacy publication ordering after another failure, and lock
release with a retained duplicate descriptor. Linux UI checks/build, formatting
and documentation audit passed. This does not establish native or browser parity.


### Prepared automatic retry increment

Automatic retry now distinguishes prepared intent from committed desired YAML,
following the frozen coordinator's prepare/commit boundary. Version 6 journals
add terminal `aborted` status. Prepared standalone intent whose YAML still matches
its base is discarded without reading the unused candidate or inspecting native
resources. This works when the initial YAML was never created. Prepared revert
supersession instead restores the prior pending journal and candidate slot.
Committed desired YAML proceeds through native verification and recorded-mode
retry; unrelated YAML revisions leave intent untouched. Aborted intent permits
new saves and is not exposed as pending. Explicit legacy fingerprint resume
retains its earlier reviewed publication behavior.

Status includes metadata for a superseded journal, and the retry job/UI explains
when an uncommitted intent was discarded. Focused tests cover both modes, existing
and missing initial YAML, corrupted unused candidates, unrelated external edits,
post-commit retry, subsequent saves, and both supersession crash windows through
automatic as well as explicit recovery. Native/runtime/output-contract and
browser/official-client acceptance remain open.

Validation: Rust 1.85 Linux library tests passed (240 passed, one ignored), all
36 API integration tests passed, and final targeted supersession-metadata and
HTTP aborted-result checks passed. The HTTP check verifies the explanatory job
output and preservation of YAML/operator data. All-target Clippy and Linux UI
checks/build passed; formatting and documentation audit passed. Read-only live
Podman/user-manager probes encountered sandbox runtime/bus restrictions, so no
native acceptance is inferred from this increment.


### Native rootless generator/engine probe increment

Elevated read-only transport reached the workload account's actual user manager
(systemd 262.2-1-arch). An isolated Podman info probe verified rootless mode with
fresh temporary storage/runtime roots. The previously ignored
`native_generator_accepts_rootless_artifacts` test passed against the installed
native generator, and is reproducible with `make test-quadlet`.

`make test-podman` now runs a real offline rootless engine smoke. It compiles a
static scratch payload, disables remote connections and registry pulls, uses
network-none containers and private temporary runtime/storage/configuration/auth,
and verifies exact roots before creation and cleanup. The final run passed on
Podman 6.2.0 with create/start, graceful stop (exit code 0), restart, fingerprint
label inspection and removal. Cleanup succeeded within the verified private
roots. The generator test also passed via its Make target on Rust 1.85.

This supersedes the earlier sandbox-unavailable observations only for these
checks. Verified scope is native generated-definition syntax and engine behavior
on this host. Actual Maestro publication/lifecycle in a real user manager,
running-versus-loaded fingerprints, boot policy, cross-backend ownership,
interrupted recovery, both installer/helper modes and disposable guest acceptance
remain open. No positive privileged helper operation or router hardware behavior
is claimed.


### Quadlet running identity/fingerprint increment

Quadlet inventory and lifecycle preflight now combine exact generated/loaded-unit
ownership with bounded local Podman metadata. The new reader validates the
service/member/native-name/fingerprint tuple, rejects duplicate or unexpected
members and oversized inventories, and checks exact-name absence for missing
filtered members. A stopped foreign name collision is never treated as an absent
managed member. Podman queries force local mode, use separate arguments and
bounded deadlines, and expose only configured member names/states and match flags.

Inventory distinguishes running, partially running, stopped and unknown state,
with consistency checks against each corresponding loaded unit. Owned stale
running fingerprints produce a restart-required notice; Start refuses stale
runtime and foreign/unknown identity blocks lifecycle commands. The Linux UI
shows member states and the restart notice. Publication and recovery modes are
unchanged; runtime/boot-only/metadata fingerprint separation remains open.

Focused unit fixtures cover malformed/foreign/duplicate/unknown identity,
oversized output, foreign names, engine failures, partial/stale/stopped/current
groups, swapped unit/member activity, and no lifecycle command on foreign runtime.
`make test-podman` now runs real Maestro reader tests within its private offline
engine lab, including a running stale member and a stopped foreign object whose
state must be preserved. Real user-manager provider actions, boot policy and the
remaining native/two-mode/official-client/browser gates remain open.


Validation for the running-identity increment: Rust 1.85 Linux library tests
passed (245 passed), and all 36 API integration tests passed. Both real reader
checks passed within the offline Podman 6.2.0 lab: the owned stale member was
reported correctly, and the stopped foreign object retained its created state
after refusal. Native engine lifecycle and verified-root cleanup also passed.
Linux UI checking/build, formatting and documentation audit passed. Full native
provider and guest acceptance remains open.


### Source-guided Linux monitoring increment

Ported the pinned Server Man monitor parser and sampler semantics into Linux-only code: strict CPU counters, genuine zero available memory, cache/buffer and swap reporting, all network interfaces, loopback-excluded aggregate rates with missing/reset-counter propagation, optional uptime/load warnings, process-count metadata and bounded time-window history. Added focused Rust fixtures from the Go monitor tests and extended the Linux resources UI. Router system_usage and router UI are unchanged. Rust/Svelte build and native acceptance are pending; this is not a parity sign-off.


### Source-guided backup inventory increment

Translated the pinned reference's declared input and consumer enumeration, explicit native VM input placeholders, per-container image and enabled-share quiescence requirements, overlapping input coverage, capture-relative paths and state collision diagnostics. The existing stricter no-symlink metadata policy is retained. Private inventory is now restricted by the same workload policy as private configuration reads. UI displays source and bundle paths, consumers, covered inputs and allocated size. Native dependency discovery, capture, verification, publication and recovery remain unavailable; compile, fixture and native checks are pending.


### Source-guided native share inventory increment

Ported Server Man's read-only effective Samba configuration, connection-count, NFS configured/registered export parsers and protocol filter into the Linux backend. Native reads use bounded ordinary-account commands and file reads, never the helper. Authoritative share definitions are now served by `managed_shares`; the Linux Shares UI separates declared and observed native state. Unverified native ownership, missing utilities and unavailable runtime data are explicit; the native share write gate remains closed. Added parser and API fixtures. Managed fragment byte-integrity verification, ownership classification, setup, publication/rollback and helper transactions remain incomplete. Rust and browser/native validation pending.


### Source-guided managed lifecycle API increment

Exposed the reference's individual start, stop, restart and scoped log tool contracts through the existing tracked-job/provider path, retaining compatibility with the prior application API. Stop and restart require affirmative confirmation; managed IDs and argument shape are validated. Quadlet logs return the actual bounded reader's truncation flag and source, and the UI uses the reference contracts. Added HTTP/MCP contract fixtures. Native ownership gates remain unchanged. Enable/disable are not yet ported: the reference requires boot-only changes without altering runtime identity, which needs separate fingerprints and native verification. Build and native acceptance remain pending.

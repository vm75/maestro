# Modular platform and UI migration: Router Hub to Maestro

**Configuration decision (2026-11-23):** The user selected YAML across platforms with an explicit one-time TOML migration. Earlier TOML runtime-preservation wording is superseded by value-preserving conversion and private exact-byte backups. Native service configuration and state JSON retain their formats.

**Status:** Phase 1 and Phase 1.5 accepted. Phase 2 implementation is active on `feature/linux-phase2`. Compile-time platform profiles, Linux Svelte shell & operations bridge, systemd services, resource monitoring, package updates, and declarative engine foundation have initial implementation and validation; full Server Man feature parity and native Linux acceptance are **not** complete. Phase 2 requires 100% of the **existing, implemented** Server Man feature set, including partial/in-progress shipped capabilities. See the Phase 2 parity contract and hard gate below.
**Phase 1.5 branch:** `refactor/rebrand-maestro`  
**Baseline:** `vm75/router-hub` `main` at `5e2013f031c965cf0381f868c541d60bb5eac602` (v0.10.4)  
**Source projects:** [router-hub](https://github.com/vm75/router-hub), [server-man](https://github.com/vm75/server-man)  
**Selected product name:** **Maestro** (`maestro`). The existing Go/Svelte **Server Man** remains a separate Phase 2 reference, not the chosen Rust project name.

## Executive decision and stopping points

Create one modular Rust management application with common modules, platform-specific implementations, and two lightweight-at-deployment UI shells. The ASUS router must retain all existing functionality. Linux gains Server Man's Svelte UI and Linux management capabilities **only in Phase 2**.

**Implement in this order:**

0. Baseline and test inventory (before structural edits).
1. **Phase 1, step 1:** backend capability/module contracts and logical UI contribution contracts.
2. **Phase 1, step 2:** reorganize Router Hub's Rust features and platform adapters without changing behavior.
3. **Phase 1, step 3:** modularize the existing ASUS UI into a common shell, one dashboard, and composable feature tabs/sections.
4. **STOP M1:** complete automated, package, and real ASUS-router feature-parity acceptance. Do not begin the next workstream or merge an incomplete Phase 1.
5. **Phase 1.5:** rebrand the validated project, including packaging/upgrade compatibility and GitHub repository collision resolution. Do not port Server Man features during the rebrand.
6. **STOP M1.5:** verify rebranded releases and existing-install migration. Only then begin Phase 2.
7. **Phase 2:** establish the router baseline and compile-time profile gate; inventory **every existing Server Man capability**; port Linux Svelte/UI/auth, operations, **non-root workload-account execution with optional installation of a restricted Linux-only privileged helper**, services/monitoring/updates/dependency installation, complete declarative Applications/VM/storage/shares/configuration management, production Linux installation, MCP HTTP+stdio and existing backup inventory/capture/recovery behavior; then prove 100% behavioral parity and native acceptance in both **without-helper** and **with-helper** modes. Full restorable backup/restore, not yet supplied by Server Man, is a separate future milestone.

**CI hold:** No automatic GitHub Actions CI on the planning/implementation branch before all three Phase 1 steps are implemented. The existing `ci.yml` triggers are already limited to pushes on `main` and `ci/**`, plus explicit `workflow_dispatch`; there is no ordinary PR trigger. Keep this branch named `refactor/modular-architecture`, **do not push it to `ci/**`, do not merge it into `main`, do not push `v*` tags, and do not manually dispatch CI until Phase 1 implementation is complete and ready for validation. No workflow edits are needed merely to achieve this hold. Keep existing `main` CI and separate manually triggered NGINX/toolchain workflows intact.

## Scope, boundaries, and non-goals

- **One backend:** Rust modular monolith, one service process per host. No dynamic Rust/JS plugins, microservices, or permanent Go sidecar.
- **Platforms:** `asuswrt` and `linux` initially; model future routers as adapters rather than branching feature logic throughout the code. **Runtime identity/privilege is platform-specific:** Maestro runs as root on the router and as a non-root workload-owner account on Linux. Linux-only protected operations require the **optional** restricted helper; without it those operations are unavailable rather than elevating the server. Model privileged execution behind a narrow platform abstraction so a future OpenWrt adapter could differ; **do not add OpenWrt support to Phase 2**.
- **Core vs platform:** Maestro is one modular application with shared, router-specific and Linux-specific feature modules, not a router application and a separate Server Man application. Feature behavior and API contracts are shared where possible; platform operations depend on explicit adapters. Module target metadata determines availability and UI contribution independently of the renderer and build profile. Shared features are first-class on both targets, not router-only compatibility features.
- **UI:** one persistent Dashboard; navigation tabs are logical groups that contain one or more feature sections. Each feature may contribute zero or more dashboard widgets. UI availability is driven by backend capabilities and permissions, never trusted as backend authorization.
- **ASUS:** preserve current build-free, offline-capable, embedded HTML/JS + ASP integration; do not require npm/Svelte, an external CDN, or runtime asset downloads on a router.
- **Linux:** keep current Router Hub real-Linux integration operational during Phase 1. Svelte 5/Vite is introduced in Phase 2 and compiled to static assets at build/release time.
- **Configuration:** preserve current Router Hub TOML and JSON, NGINX sites, state paths and migrations in Phase 1. For Linux Phase 2, preserve Server Man's **single authoritative private YAML** semantics for Linux server settings and managed applications/storage/shares, including revision-checked edits and restart-required server settings. Router configuration remains unaffected. Any translation into existing Maestro internals must retain an equivalent one-authority Linux user workflow; do not silently split Server Man settings across competing TOML/YAML documents.
- **Name:** do not rename packages, executables, API paths, menu labels, or config directories until after the M1 parity gate.
- **No functionality ported from Go during Phase 1:** no Docker/Podman/Incus/libvirt, Samba/NFS, Linux package-management features, new browser-session auth, or MCP.
- **No unplanned backend changes:** no new firewall implementation, NGINX renderer replacement, authentication redesign, storage-format conversion, or upgrade-flow redesign during structural work.

## Target module model

Three independent concerns must not be conflated:

| Concern | Responsibility | Example |
| --- | --- | --- |
| Feature/backend module | Domain logic, operations, data, requirements and health | NGINX, certificates, services, applications, Ban Shield |
| Platform/provider adapter | Executes operations for a target environment | Entware init vs systemd; ASUS DNSmasq vs Linux; Podman vs Docker vs libvirt vs Incus |
| UI contribution | Tab section, action, notification and optionally dashboard widget | Certificates section in Web tab; failed-services dashboard summary |

A **tab is not a backend module**. An Applications tab will aggregate all supported workloads; NGINX and certificates can share a Web tab. A feature need not register a tab of its own. A feature need not contribute any dashboard widget.

### Initial module ownership / target availability

**User clarification (2026-11-23):** the platform profiles select a composition of shared and target-specific modules. The direct Server Man port expands the Linux module set; it does not replace or fork Maestro's shared modules. The following is the intended target ownership, not a claim of current Linux native verification.

| Module | Ownership | ASUS target | Linux target |
| --- | --- | --- | --- |
| System monitoring and dashboard | Shared | Existing lightweight metrics | Common metrics plus Linux-specific richer Server Man sampler/process details as needed |
| NGINX sites, ingress, rendering, rollback | Shared | Existing behavior | Same domain, native paths/service adapter as needed |
| Certificates / ACME | Shared | Existing behavior | Same certificate domain, native paths/service adapter as needed |
| Ban Shield / firewall | Shared | Existing ipset/iptables implementation | Reuse domain; validate native backend and permission requirements, never grant unbounded privilege |
| Services | Shared logical feature with distinct native implementations | Router services / Entware init | Server Man systemd inventory and lifecycle |
| Wake-on-LAN | Existing shared target metadata | Existing behavior | Preserve existing compatible behavior; no new port scope |
| AdGuard Home / DNS | Router-targeted for now | Existing AdGuard and ASUS DNSmasq integration | Sharing may be added later if needed; no new Linux integration required in this port |
| WireGuard | Router-targeted for now | Existing behavior | Sharing may be added later if needed; no new Linux integration required in this port |
| ASUS UI, menu/ASP integration, Entware router/NGINX upgrades | Router-specific | Existing behavior | Not exposed as Linux features |
| Server Man applications, containers and VMs | Linux-specific | Unsupported | Direct port of declarative coordinator and native providers |
| Server Man storage, Samba/NFS shares, workload configuration/recovery | Linux-specific | Unsupported | Direct port |
| Linux OS package updates/dependencies, systemd-specific management | Linux-specific | Unsupported | Direct port |
| Server Man MCP/agent access and optional privileged helper | Linux-specific initially | Unsupported | Direct port and integration |

The existing registry currently advertises AdGuard and WireGuard for both targets. The target direction is router-first; do not use this clarification to remove already validated Linux compatibility or introduce an unrelated breaking change. Defer new Linux work on these integrations unless explicitly needed. Where a shared feature needs Linux privileges, use ordinary delegated permissions or narrowly scoped authorized operations as appropriate; do not run the Linux server as root or expand the helper to arbitrary commands. If a shared feature lacks a working Linux adapter, record that limitation separately from Server Man parity and do not report it ready. Sharing domain logic does not require identical UI rendering, native command implementations or resource consumption on every platform.


Every module should distinguish **unsupported** (not a target), **disabled** (configured off), **unavailable/degraded** (supported but dependency or permission missing), and **ready**. Do not hide missing dependencies inside generic disabled status. The backend rejects operations for unsupported/disabled modules even if a caller bypasses the UI.

### Backend contracts (illustrative, not prescribed public API)

- Static registry with immutable IDs, supported targets, required capabilities, enabled state, availability/health and dependencies.
- Adapter contracts for `ServiceManager` and other truly platform-specific operations; preserve existing semantics rather than forcing a misleading lowest-common-denominator abstraction.
- Services: inventory/details/status/logs, start/stop/restart, enable/disable, optional reload. Keep runtime state and boot enablement distinct.
- Optional additive `GET /api/modules` (or equivalent discovery endpoint) exposing non-secret capabilities/health and feature groups. Keep **all existing** `/api/*` endpoints and response contracts stable.
- Domain functions should not import HTTP handlers, browser components, or ASUS menu code. API handlers are adapters; command execution remains bounded and argument-based.
- Module assembly must preserve dependency ordering, background tasks, error behavior and shutdown.
- For future Linux app-owned systemd units, route actions through the Applications coordinator, not direct generic systemctl operations.

### UI contribution contract

A module declares stable identifiers, applicable backend capability IDs, optional tab sections and optional dashboard widgets. Composition is deterministic; no runtime code loading or plugin marketplace.

**Initial logical tabs on the ASUS UI (proposed grouping, subject to parity tests):**

| Tab | Sections | Dashboard |
| --- | --- | --- |
| Dashboard | Core system overview + contributed widgets | Always present |
| Services | Entware services | Optional service summary |
| Web | NGINX sites/settings/logs; certificates | Optional NGINX/certificate summaries |
| Security | Ban Shield, firewall policy/rules | Optional active-ban summary |
| Network | AdGuard, WireGuard, Wake-on-LAN | Optional integration/status summaries |

All currently accessible independent views (Dashboard, Services, NGINX, Certificates, Wake-on-LAN, Ban Shield, AdGuard, WireGuard) must remain accessible after grouping. If preserving existing view/tab IDs as aliases is simpler or safer than immediate grouping, implement aliases and migrate navigation gradually. Preserve all forms, actions, modals, error/success feedback, accessibility, refresh behavior, deep links, IDs used by scripts, and ASUS ASP rendering. The visual layout may be reorganized; **functional parity is mandatory**.

For Linux in Phase 2, the same logical contract composes shared Dashboard/Monitoring, Web (NGINX and certificates) and Security (Ban Shield) contributions with Linux Services and Server Man's Applications (Podman Quadlet, Podman Compose, Docker Compose, libvirt, Incus), File Sharing, Updates, Configuration and Agent Access. Use the existing Svelte workflows for the direct port and integrate shared feature views at the same navigation seam. Do not create one top-level tab per provider or a separate Server Man UI disconnected from Maestro. Expose only supported and authorized actions; preserve existing router rendering.

**Dashboard rules:**

1. Dashboard is always available even if all optional contributors are unavailable.
2. A module may contribute zero, one or multiple compact summaries; no contribution is mandatory.
3. Each widget has its own loading/error/refresh lifecycle and cannot block other widgets.
4. Failed reads are surfaced as unavailable/stale, not silently treated as zero or healthy.
5. Summaries link to their owning tab/section where applicable. Avoid a global dashboard that duplicates all module pages.
6. Widget placement/order is deterministic in Phase 1; user customization is deferred.

## Phase 1 implementation record (M1 not signed off)

The implementation branch now contains static module discovery and UI
contributions, a protected additive `/api/modules` endpoint, legacy-compatible
domain and platform adapters, and a build-free split ASUS UI source composed
into the same self-contained `web/index.html`. Existing API routes, CLI
commands, package name, version and persistent configuration are unchanged.

The compatibility inventory and remaining manual/automated acceptance work are
tracked in [Phase 1 compatibility and validation](../../phase1-compatibility.md).
A JavaScript syntax check has been performed; the full Rust test suite, ARM IPK,
native-Linux smoke, browser workflows, GitHub CI and physical ASUS validation
**must still be run**. No Phase 1.5 rebrand or Phase 2 port is included.

## Baseline / pre-work (gate B0)

Before changing architecture, capture comparisons against an unchanged `main` checkout:

- Record current Rust version (`1.85+`), package version, CLI help/options, exact startup/shutdown/upgrade command restrictions, normal and test-mode configuration behavior.
- Record existing API routes and representative success/error JSON, auth/origin enforcement, configuration defaults and validation errors.
- Record current UI navigation and browser workflows; capture standalone rendered HTML and injected ASP (including the current `<style>` / `<body class="rh-page">` extraction expectations); identify inline handlers and DOM selectors used in tests.
- Inventory persistent state files and update/migration paths; include rollback behavior and crash/restart recovery where existing.
- Run available local fixtures and tests: `cargo fmt --all -- --check`, `cargo test --locked`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo build --release --locked`, `scripts/test-nginx-ui.mjs`, NGINX template tests when local dependencies are present, `scripts/test-package-ipk.sh`, `make test-linux` where host dependencies permit.
- Capture a representative ASUS hardware baseline **on the matching router**, if available: Entware service actions, menu injection, NGINX lifecycle, HTTPS/certificate flows, Ban Shield, DNSmasq and AdGuard, WireGuard, WOL, idle RSS/CPU, cold start and package contents. Avoid writing production firewall rules solely for a baseline; use safe/observational checks and a disposable test scope where applicable.
- Keep baseline artifacts and review notes in safe test fixtures or private test records; **do not commit tokens, private keys, real router logs, `data/`, or live configuration secrets**.

Acceptance: a reproducible inventory and test plan exists before a file move. Existing failing tests or unavailable dependencies are recorded, not silently reclassified as passing.

## Phase 1 — behavior-preserving Router Hub modularization

### Step 1 — Declare module/capability and UI composition contracts

**Deliverables**

- Define stable backend module IDs, platform targets, capability/availability states, and module dependency validation.
- Introduce a minimal static registry and additive discovery representation, avoiding speculative plugin loading.
- Define framework-independent UI contribution manifest: tab IDs, section IDs, widget IDs, ordering, prerequisites and lightweight read-only status.
- Establish a compatibility map of current URL paths, API routes, menu names, command names and persistent paths.
- Add tests for module assembly on `asuswrt` and `linux`, unsupported/disabled checks, duplicate IDs, dependency resolution and serialization.
- Document upgrade/rollback strategy and baseline measurements.

**Acceptance:** existing configs and route behavior remain unchanged. Router build has no new runtime/frontend dependencies; the registry can describe current features without changing their execution.

### Step 2 — Restructure Rust backend and platform adapters

**Deliverables**

- Move code in small, reviewable units to `core/`, `modules/` and `platforms/` as justified. Begin with simpler boundaries (system usage, WOL, Services), then larger domains.
- Centralize module initialization and route assembly; keep current API endpoints as compatibility routes.
- Extract platform-specific services (Entware init) and ASUS integration (menu/ASP, DNSmasq/NVRAM, Entware-only upgrades) without changing the underlying operations.
- Introduce `ServiceManager` for Entware first; **do not implement the Server Man systemd service manager in Phase 1**. Keep existing real-Linux behavior working with its existing compatible scripts.
- Preserve NGINX render/rollback, certificate/HTTP-01 sessions, Ban Shield enforcement/ownership, DNS settings, WireGuard and WOL behavior and error surfaces.
- Preserve command runner safety, locks, atomic stores, filesystem symlink protections and startup/reconcile ordering.
- Keep the default platform/`platform = "asuswrt"` serialization and current `--linux-mode` / `--test-mode` compatibility rules.
- Keep version, binary name, init scripts, IPK metadata, updater/release lookup and packaging unchanged.

**Acceptance:** source reorganized without functionality changes; existing tests pass locally, API contract fixtures match, Linux integration smoke works, and the ASUS package remains self-contained. Do not claim hardware equivalence yet.

### Step 3 — Modularize existing ASUS UI

**Deliverables**

- Extract shell/navigation/dashboard and feature-specific view/render/event logic from `web/index.html` into maintainable source pieces with a deterministic **build-free** embedding/composition path.
- Retain a **single self-contained offline UI payload** at runtime, compatible with `src/asus_ui.rs` and `config/asus-wrt.asp.template`. Do not require npm, a CDN or runtime file fetching.
- Implement tab/section composition and optional dashboard contributors using the same backend capability identifiers.
- Consolidate logical tabs as appropriate (e.g. Web includes NGINX and certificates; Network includes AdGuard, WireGuard and WOL). Preserve previously reachable features and any required existing selectors/deep-link behavior via mapping/aliases.
- Keep existing token handling, login/auth errors, visual status, user feedback, NGINX editors, modal flows, and browser/Axum API behavior. Avoid introducing a new browser authentication mechanism on ASUS.
- Add UI-level tests covering tab registration/ordering, all enabled feature sections, empty dashboard contributors, partial failures, unsupported modules, standalone and ASP rendering.
- Add test coverage for composed HTML/JS correctness; verify inline functions and browser global scope still work when mounted into ASUS firmware.

**Acceptance:** no missing, broken or inaccessible router workflows; offline standalone/ASP pages work; no extra runtime asset requests; UI still packages in the Entware IPK. This completes implementation of Phase 1 **but not yet hardware sign-off**.

### M1 — hard feature-parity gate (PAUSE HERE)

Run validation only after Steps 1–3 are implemented. The first GitHub CI run is an **explicit manual `workflow_dispatch`** on the branch or its PR number; do not create `ci/**` branches merely to make CI run automatically. Fix all failures on the feature branch. A passing CI run is necessary but not sufficient.

| Functional area | Required parity evidence |
| --- | --- |
| Router startup / ASUS menu | Boot/init script, ASP rendering, menu integration, restart/shutdown unaffected |
| Dashboard / UI | Every original feature, form and action reachable; all widgets optional and independent |
| Auth / API | Bearer token and origin protections; unchanged existing routes, JSON and errors |
| NGINX | Test, start/reload/stop, site CRUD, inherited settings, ingress/access previews, logs, render validation and rollback |
| Certificates | Issue/renew, DNS-01/HTTP-01, dehydrated locking, scoped firewall challenge window and failure recovery |
| Ban Shield / firewall | Observe-only, policy CRUD, allowlists, bans/unbans, reconciliation and bounded log processing |
| AdGuard / DNSmasq | Config, filtering/protection timers, rewrites, router-specific hosts and DNS restart behavior |
| WireGuard / WOL | Interface/peer controls, status, device CRUD/reachability/wake |
| Services | Entware listing, lifecycle, enable/disable, logs, protected Router Hub init entry |
| CLI / storage | backup/restore, schema compatibility, state persistence, router-only upgrade and upgrade-nginx guards |
| Linux compatibility | Existing native-linux smoke, no unexpected ASUS NVRAM/menu/firewall effects |
| Packaging | IPK layout, postinst ordering/migrations, clean install and in-place upgrade |
| Footprint | Measure ARM router binary size, startup time, idle CPU and RSS; explain/reject material regressions |

**Exit requirements:** local checks + manually invoked GitHub CI pass, regression fixtures pass, matching ASUS router hardware is tested, operator signs off behavior and footprint, rollback/recovery instructions are verified, and Phase 1 commits are reviewed. **No Phase 2 code or rebrand is mixed into M1.** Once validated, Phase 1 can merge to `main` (which intentionally invokes the normal main CI/release policy) and receive its own stable tag/release before rebranding.

## Phase 1.5 — Maestro rebrand (implementation completed; validation pending)

Phase 1 is merged on `main` as v0.10.5. This rebrand is implemented on
`refactor/rebrand-maestro` and must not be mistaken for a released, hardware-validated package.
The new Rust project identity is **Maestro**, including the `maestro` crate, binary,
Entware IPK, `S99maestro` init script, new-install config path and UI. The
existing artwork is merely renamed and can be replaced by the maintainer.

The GitHub repository has been renamed to **`vm75/maestro`**. The historical
Phase 1 baseline was developed under `vm75/router-hub`, so that identifier is
retained above only to describe the original source project. Both the Rust
self-updater and the custom NGINX updater now target `vm75/maestro` releases. The unrelated Go project at
`vm75/server-man` remains untouched and may be used as a Phase 2 port reference.
The earlier Server Man repository-name collision is no longer relevant.

**Migration strategy (one private deployment):** Maestro v0.11.0 is a
clean break. The CLI, packaged files, browser auth, static assets, NGINX
settings markers, firewall chains and UI no longer recognize Router Hub
aliases. The Entware package retains `Conflicts: router-hub` solely to
prevent unsafe co-installation. A separate, backup-gated, operator-invoked
`scripts/migrate-router-hub-once.sh` stages the original configuration and
data in Maestro paths; it is neither packaged in nor invoked by Maestro.
After verifying copied data and generated NGINX settings, remove old JFFS
hooks and the Router Hub package, reboot to clear runtime firewall objects,
then install a verified Maestro IPK. The original data and an off-router
backup remain available for manual rollback. No unattended legacy import.

Full instructions and rollback procedures:
[Maestro migration guide](../../maestro-migration.md).

**M1.5 exit gate:** Verify fresh install, Router Hub v0.10.5 upgrade,
restart/boot, API/auth/UI/ASP and router feature parity, existing NGINX
configuration and certificates, firewall state continuity, native Linux smoke,
Entware metadata and safe rollback. Run local CI/tests and release packaging.
Do not start porting the Go Server Man capabilities until acceptance.

## Phase 2 — port Server Man features into Maestro

**Implementation strategy (user-directed, 2026-11-23): direct port first, integration second, refinement later.** Server Man is a functional prototype and the implementation source, not merely a requirements document. Reuse as much of the pinned working implementation as possible. Prefer a faithful, mechanical Go-to-Rust translation of complete modules over a new design or a fresh implementation inferred from feature descriptions. **Deliver all existing reference features, not a subset.**

### Corrective integration pass — supersedes earlier incremental redesign

**User authorization:** undo, replace or delete Phase 2 implementation where it is faster to transplant working Server Man behavior. The existing branch is a source of reusable work, not a mandatory architecture or a requirement to preserve every commit's resulting code. Preserve history through ordinary corrective commits (no force-push or broad reset); do not revert accepted Phase 1/1.5 router behavior, user changes, or unrelated work.

Use the original reference package boundaries as the default implementation layout for Linux-specific functionality. Keep Maestro platform/module registration and build-profile boundaries thin. Keep existing shared modules (monitoring, NGINX, certificates, Ban Shield) available to both platform compositions; reuse their domain logic, with native adapters and renderer contributions as necessary. Integrate Server Man monitoring extensions with the common monitoring surface instead of maintaining duplicate dashboards. The port is additive to shared Maestro features, not a replacement Linux product. Do not force the translated Linux domain to conform to router-oriented internals or unfinished Rust module shapes when a small adapter would suffice. A complete working provider is preferred over several modeled/gated partial implementations.

| Area | Corrective action |
| --- | --- |
| Core/registry/HTTP/MCP | Reuse equivalent Maestro infrastructure; implement exact Server Man tool arguments, schemas, results/errors and job behavior at the integration seam. Use the same dispatch/domain logic across Linux transports. |
| Privileged helper | Port `cmd/server-man-admin`, `internal/admin`, schema validator, native handlers and tests directly. Preserve the one-shot `operation` + `arguments` JSON request, optional `result/output/truncated/error/code/status` fields, input size, fatal stderr/exit behavior and trusted caller semantics. Binary branding and implementation language must not change the wire protocol. A private Maestro probe/versioned/flattened envelope is not part of the target; replace it with an inert installation/authorization check outside the helper wire protocol or a reference-compatible check. Keep existing root-owned policy and fail-closed restrictions effective until a safe equivalent is integrated; assess optional hardening separately. |
| Providers/declarative | Transplant complete model/store/plan/apply/recovery and native adapters, using reference tests as the acceptance oracle. Reuse equivalent Rust primitives, not divergent implementations solely because they exist. |
| Linux UI | The reference Svelte app already has complete editors, page workflows, shared components, styles, job tracking and a nearly identical API client. Bring the page/component tree across as a cohesive unit. Adapt only branding, shell/module discovery and action availability; keep the upgraded Vite/package lock. Make backend contracts fit the existing UI, not the reverse. Keep the ASUS renderer unchanged. |
| Installation | Reuse reference shell logic, fixed native paths and setup flows; adapt binary/service names, authoritative YAML and optional-helper installation. Do not invent alternative deployment mechanisms. |

**Order:** (1) consolidate helper and shared native services/updates/shares around the reference contract; (2) port declarative engine and remaining providers/recovery as complete packages; (3) bring the reference UI/editor tree over once matching backend operations are present, retaining equivalent components; (4) complete installer and MCP compatibility; (5) validate both Linux deployment modes and router regression gates. Work in coherent source batches where dependencies permit. Temporary shims belong at integration seams and must be removed when the reference path is available; no long-lived duplicate protocol/domain implementations.

The Rust helper has partially switched management operations to the reference envelope (source checkpoint `38c5f164b38f48050147373b074860cb77dbcf67`); its old internal client types and custom capability probe still exist, and five native share operations are gated. This is not helper equivalence. Do not build further features around that transitional design. Confirm all 20 reference helper operations, including the three helper-only share-definition verbs, against the same request/response fixtures and native effects before claiming parity.

### Mandatory direct-port workflow

1. **Read the complete working reference module first.** Include its supporting types, shared utilities, caller/coordinator, native adapters, tests, fixtures and Svelte workflow. Follow the actual dependency graph and control flow in the pinned source; do not reconstruct behavior from the parity ledger, README or tool descriptions alone.
2. **Translate the whole cohesive module.** Retain the reference algorithms, branching, validation order, state transitions, native command arguments, ownership checks, output parsing, recovery and error handling. Keep analogous function names and file/module boundaries where useful for review. Mechanical changes for Rust ownership, types, errors, async execution and standard-library APIs are expected; a literal line-for-line translation is acceptable when it preserves working behavior. Reuse shared reference helpers rather than implementing a new variant in each adapter.
3. **Prefer direct reuse of language-independent assets.** Port the existing Svelte/TypeScript screens, components, editor and client workflows with the smallest integration changes; reuse shell installer logic, templates, fixtures and sample configuration where compatible. Do not redesign the Linux UI or invent an alternative workflow just to integrate it with Maestro.
4. **Integrate at the existing Maestro boundaries.** Adapt the translated module to the Rust command runner, job/registry, authoritative Linux YAML, authentication, module discovery, Linux-only optional privileged helper and platform feature flags. Keep the existing router modules, UI, behavior and build footprint intact. No Go runtime, Go binary, sidecar or second configuration authority is required.
5. **Review and replace incomplete Rust work when necessary.** Existing work on `feature/linux-phase2` is not a design constraint. Keep it where it is already equivalent; replace incomplete, divergent or unnecessarily complicated code with the direct translation whenever that is the faster and safer path to parity. Entire Phase 2 packages or page trees may be replaced in one coherent change; prior incremental implementations need not be salvaged. Do not discard verified working behavior or strengthen a gate by permanently disabling a reference feature. Retain stricter safety controls where needed, and adapt the reference operation to satisfy them.
6. **Port the reference tests alongside implementation.** Translate existing positive, negative, malformed-input, permission, drift, interruption and recovery cases; reuse golden fixtures wherever possible. Add targeted integration tests only for the differences introduced by Maestro. Record the source paths, corresponding Rust/Svelte paths, any material deviations and their rationale in the parity ledger.
7. **Finish the first-pass port before optional refinement.** Avoid speculative abstractions, generic frameworks, premature optimization, unrelated dependency changes, style-only rewrites and expanding beyond the frozen feature set. Fix correctness, security and compatibility problems during the port; defer nonessential Rust-idiomatic cleanup and improvements until functional parity and native validation. Do not defer required safety guarantees or privilege separation.

**Exception rule:** a fresh implementation is justified only by a concrete incompatibility, a security/correctness defect in the reference, an unavoidable language/platform constraint, or an already equivalent Maestro facility. Document the reason and preserve the reference's externally observable behavior. A different Rust design preference is not sufficient.

**Execution on the current branch:** complete coherent implementation batches without stopping for user validation after each small translation. The operator will compile and test the accumulated changes locally, and the implementation agent should fix the reported findings. Continue source implementation while local validation is pending; maintain the distinction between implemented, compiled, fixture-verified, native-verified and accepted. All M2 gates remain mandatory before merge or release. Never enable unverified native mutations just to make a partial implementation appear complete.

## Phase 2 Linux 100% Server Man parity contract

**Non-negotiable product decision (2026-11-23; optional-helper clarification):** Maestro's Linux process runs **unprivileged** as the selected workload account **whether or not a helper is installed**. Maestro's router process continues to run as **root**; router behavior is unchanged. Linux has an **optional-to-install**, separately packaged, root-owned **Maestro privileged helper** for specifically authorized host changes; the Linux web/API process must never elevate itself. The helper's implementation is **mandatory in Phase 2** so a full-capability deployment can achieve **100% of the features actually implemented in Server Man** at the frozen reference baseline. A deliberately helper-free installation is an explicitly supported *reduced-permission deployment*, **not** an excuse to omit capabilities from the product. This section supersedes earlier optional-port wording anywhere in this plan.

**Reference snapshot:** `vm75/server-man` `main` commit `c1a9f32f5898146ae7ac339043b910b8e2a2c438` (2026-11-19). Keep that repository unchanged as the specification/fixture reference. A newer Server Man commit is not silently added to scope: explicitly review any desired re-baseline. Trace all features, tools, UI paths, backend behavior and tests from this **specific commit** before declaring the inventory complete. “100%” means feature and safety-semantic parity. A close Go-to-Rust translation is the preferred implementation method, but identical source structure is not an acceptance requirement. Maestro remains a Rust application; installing Go alongside Rust is not part of deployment. Retain existing Maestro features in addition to these Linux features.

### Scope clarification: shipped behavior vs future aspirations

- **Required:** Everything that works in the reference revision, including incomplete-but-implemented workflows, guardrails, error handling, tested edge cases, authentication and installation, UI, HTTP/MCP, release and operational tooling. Partially completed Server Man subsystems must retain their **actual capabilities and limitations**; do not silently omit them just because their broader project remains in progress.
- **Required backup-related scope:** Existing backup input inventory, native dependency/export inspection, capture/preparation where implemented, checksummed private manifests/verification, backup journal and interrupted capture/publication/recovery operations and blockers, including corresponding UI/MCP behavior. Preserve the “captured is not restorable” distinction. Do not claim full backup/restore support.
- **Not yet a Server Man feature:** Full verified end-to-end restorable backup/restore, unsupported provider types, native Arch certification and other proposals not implemented at the baseline. Track them as separate future work rather than making them a pretext to skip existing capture functionality. Existing Server Man test/acceptance gaps remain explicit and must be validated in Maestro's supported environments.
- **No silent substitution:** If Maestro chooses a different internal implementation, configuration storage location, API path, or UI grouping, document a one-to-one behavior/operation mapping, migration/compatibility implications, and equivalent validation. No feature is considered waived without an explicit user-approved scope change. No unapproved “optional,” “stretch,” “future” or “out of scope” label for existing reference behavior.

### Mandatory parity inventory before additional provider implementation

Create and maintain `docs/plans/proposed/server-man-phase2-parity-matrix.md` (or an equivalent version-controlled, linked matrix) **as the first Phase 2 plan-hardening deliverable**. Seed it by auditing the reference commit's executable/installer/helper entry points, `internal/app` registry, all `internal/modules` and `internal/declarative` operations, `internal/auth`, `internal/mcp`, all `web/src/pages`/shared UI components, tests, README, architecture and design/operations documents. Do not rely only on README headings or approximate tool counts. Include every reference tool (Server Man's current SDK suite reports 55), arguments/schema, HTTP/MCP transport semantics, UI user journey and observable host effect.

For **each distinct capability and important error/recovery case**, record:
1. Stable reference ID, exact Server Man source/test paths and relevant reference test names.
2. Expected behavior (inputs, outputs, permissions, state, boot vs current-run semantics, safety failures and recovery); whether baseline behavior is partial or fully operational.
3. Maestro Rust module, tool/API, Linux Svelte view and test/evidence links (or `missing`); record direct translation, reuse of an equivalent existing module, or an explicitly justified adaptation. Map source functions/tests to their destinations and explain material deviations.
4. Status: `not started` / `modeled` / `gated` / `implemented` / `fixture verified` / `native verified` / `accepted`. “Present in code” and simulated/mock success never equal `accepted`.
5. Tests: Go contract comparison, Rust unit/API/schema parity, browser flow, **helper-free read-only/rootless acceptance and helper-enabled privileged acceptance**, native disposable-host test when a host mutation is involved, cold boot/restart and recovery/fault test as relevant.
6. Explicit blockers, ownership/risk, and any user-approved exception with rationale and source commit.

The **minimum** inventory categories are below. These are *starting categories*, not an exhaustive list; source/test inspection must add omitted operations and behaviors.

| Reference subsystem | Must be inventoried and ported |
| --- | --- |
| Linux dashboard/monitor | CPU, memory, networking, storage, disk I/O, thermal metrics, bounded sampler/history, process inventory/filter/sort, partial/stale/error presentation, responsive pages, light/dark theme and persisted UI expectations |
| Auth/security/core | First-run administrator setup, credentials/password hashing, token fallback/access, durable sessions/logout, Host/Origin protections, limits, registry schema validation, read/write/destructive policy, serialized jobs, output bounds, audit and cancellation behavior |
| Services | System and user unit inventory/details/logs, start/stop/restart/reload/enable/disable, generated-unit ownership routing, partial reads, boot semantics and least-privilege escalation |
| Updates/dependencies | APT/DNF/Pacman/Zypper detection, cached listing, refresh/full upgrade, reboot indication, named package installation, selectable provider dependency installation, native repository/version policy, explicit share-server setup and error/interrupt recovery |
| Declarative core | One authoritative Linux YAML/server settings, strict parsing and repair, private references, stable IDs, revisions/history/revert, validated plans, journaling and recovery, generated-artifact ownership/conflict inspection, metadata-only reconciliation and blocked mutation states |
| Applications | Podman Quadlet, Podman Compose, Docker Compose, libvirt/KVM and local Incus VMs; typed definitions/editors, source/registered/runtime states, start/stop/restart vs boot policy, group partial state, logs, active/deferred change behavior, dependency ordering and failures |
| Native prerequisite/readiness | Configuration-wide and per-resource actionable checks, missing binaries, permissions, cgroup/user manager/bus, engines/sockets, ports, mounted/readonly/missing data, network bindings, VM pools/networks/disks, stale/unknown, automatic global alert and explicit refresh |
| Storage/VM data | Managed new directory and raw/qcow2 disk creation, expected-mount checks, safe no-replace publication, pinned local-image cloning, source checks, VM identities and native ownership, capacity and privilege boundaries |
| Samba/NFS | Native read-only inventory, declarative owned fragments, typed share editors, publish/unpublish, candidate validation/rollback, preserving unmanaged data, Samba-password administration, package/service setup and helper delegation |
| UI/operator workflow | All reference screens: Overview, Applications, Services, Updates, Configuration, File shares, Agent access, Activity/jobs, shared editors/confirmations, recovery, installation/readiness affordances and mobile/browser tests |
| Installation/release | Source/release installer, dry-run/confirmation, backend selections, system-service installation under workload account, **optional** helper/sudoers installation with explicit consent, helper-free install/reinstall, preserved private config/token, package dependencies, reinstallation, upgrade and rollback/verification boundary |
| MCP/API | Shared registry parity, HTTP JSON tools, MCP Streamable HTTP **and stdio**, official client negotiation/schema tests, tool error/structured results, connection instructions, Agent access UI, jobs and auth/write policy |
| Partial backup/capture | Existing inventory/preflight, partial native/filesystem collection, private manifests, offline verification, captured-vs-restorable status, dependency/stopped-state checks, interrupted backup/workload/publication journals, reviewed resume/release/publish and startup safety blockers |

### Linux privilege and runtime separation: optional deployment component, mandatory full-capability implementation

- **Main Linux server:** Run the web/API/MCP and declarative coordinator as an ordinary, configured **non-root workload account**. Preserve the same user's rootless Podman, user systemd manager and user-owned files; never perform blanket `chown`, run Podman as root, or silently switch an application to rootful execution. If the process starts as UID 0 in Linux production mode, refuse by default (provide only an explicit, documented, test-only exception if strictly needed); the root router build is unaffected.
- **Optional Linux helper:** Implement a small root-owned `maestro-admin` executable as a **Linux-only optional install component**; include it in a Linux distribution artifact where appropriate but **do not install it or any sudoers rule without explicit user selection/confirmation**. Its absence must not prevent the non-root main server from installing, starting, upgrading or using unprivileged features. When installed, invoke only on demand via a narrowly scoped `sudo -n` rule. **No setuid**, root web daemon, listening socket, general shell executor, arbitrary path/command arguments, dynamic plugin loader or persistent elevation. Its binary and parent directories must be root-owned and non-writable by the workload account.
- **Trusted request contract:** Versioned, size-bounded, typed structured requests over stdin; closed allowlist of fixed verbs, validated resource identities and canonical paths, fixed executable discovery/argv and timeouts; no unchecked environment variables or arbitrary shell. Validate again **inside** the helper after elevation, not only in the Rust caller. Keep the command and schema surface as small as possible; do not route read-only or user-owned actions through root.
- **Allowed privileged domains:** Selected system-scope systemd operations, native package refresh/upgrade/named/dependency installation, and protected Samba/NFS candidate validation/publication/reload/server setup and Samba-password operations as required by the reference. **When the helper is not installed, unauthorized, outdated or invalid, these *specific privileged mutations* must fail closed and show clear unavailable reasons; their read-only or independently authorized functions stay available.** Rootless containers, user services, user-owned workload YAML/history and ordinary monitoring remain unprivileged. Libvirt/Incus/Docker use correct delegated daemon/socket/group permissions where possible; no blanket elevation of their CLIs and do not unnecessarily disable these backends if their normal account permissions suffice.
- **Defense/recovery:** Helper-local lock and bounded output, safe file ownership/path handling, atomic no-replace writes and rollback, fixed sudoers executable path, audit without secrets, no privilege leakage via stderr/logs/MCP, robust timeout/interrupt semantics and distinct diagnostics for **not installed**, **not authorized**, **invalid/unsafe ownership**, **protocol mismatch** and **temporarily unavailable**. Preserve the application's separate global job lock and approval/confirmation gates. Distinguish helper success from observed native-state verification; never silently fall back to root execution, `pkexec`, interactive sudo, arbitrary host commands or insecure permission changes.
- **Installation:** Review-before-apply/dry run configures the non-root workload account, approved groups, private state/config and Linux systemd service **running as the workload owner**, including user-bus/lingering prerequisites. Provide explicit **install without helper** and **install/enable helper** choices; default to no helper or require explicit opt-in to enable it, and never silently add/remove sudoers privileges. Helper-free setup must not attempt privileged dependency installation or protected service setup automatically; instead report actionable manual prerequisites and unavailable operations. Adding/removing a helper later is an explicit reviewed operation, preserves configuration/data and does not need reinstallation of the main server. Do not silently change account groups, enable unrelated daemons or weaken filesystem access. Test a normal non-root browser/MCP client in both modes, including revoking sudo permissions while the service is running.
- **Router isolation:** The `asuswrt` Cargo profile has **no Linux helper, sudo logic, native Linux package installer, systemd service installer, Svelte assets or helper dependency in its runtime/build/deployment graph**. It continues to run as root using existing router command/platform adapters. Preserve its validated size/memory/startup/behavior.


### Capability behavior and user experience without the helper (required in Phase 2)

**Installation opt-in is the only optional part.** Implement all privileged tools, helper protocol and UI/MCP workflows in Phase 2 even if a particular host intentionally does not install the helper. Full **Server Man functional parity** is measured with the helper properly configured; **graceful reduced-permission operation** is separately mandatory and verified with the helper absent.

- **Detection:** Use a fixed, explicitly configured helper path (never a user-controlled PATH lookup) and an **inert, read-only** version/capability probe. Validate file/parent ownership and modes, expected executable identity/protocol, authorized noninteractive sudo invocation and required policy before reporting privilege readiness. Bound probe duration and output; do not run a protected write merely to test availability. Probe on startup, on explicit refresh, after an installer/helper change, and immediately before each privileged write (with server-side revalidation by the helper). Report unavailable/unknown rather than assuming success if a probe times out or cannot establish authorization. If an operation loses authorization between probe and execution, fail closed without retrying as root.
- **Discovery and authorization:** Expose a structured capability state and reason per operation: `ready`, `disabled`, `unavailable` or `unknown`, distinguishing **unsupported platform**, **helper absent**, **helper unauthorized**, **helper unsafe/mismatched**, **dependency missing**, **policy disabled** and **transient failure**. Module discovery, HTTP tool schemas/permissions, MCP discovery and server-side execution checks must agree. Maintain the reference tool/API surface and schemas: a helper-required tool may remain discoverable but must reject execution with a stable machine-readable permission/unavailable error and safe explanation. Never interpret hiding/disabling a UI button as authorization.
- **UI presentation:** Keep supported feature tabs, settings and read-only inventory visible. Disable **only the actions whose needed privilege is unavailable**, with a concise reason and an explicit setup/recheck path. Preserve partial data, last good readings, readiness warnings and affected dependants. Never globally mark Applications, Services, Updates or File Sharing unsupported just because a helper is missing; do not claim an unavailable mutation succeeded. The UI may offer **instructions** to install the helper, but must not silently elevate, install or modify sudoers on click.
- **Works without helper, subject to ordinary account/backend permissions:** CPU/memory/process/network/storage monitoring, application UI/auth/MCP/jobs, user systemd inventory and user-unit management, systemd system-unit inventory/journal where readable, cached package status, rootless Podman/Compose, user-owned configuration/storage/history and other user-owned actions, and Docker/Incus/libvirt access already legitimately delegated to the account.
- **Requires helper for protected writes:** System-scope systemd actions that the account cannot otherwise perform, system package refresh/upgrade/install and backend dependency installation, protected Samba/NFS candidate publication/service setup and Samba password operations. Do not attempt an insecure substitute to restore these functions. A particular read-only operation can remain available if the host independently grants safe read permissions.
- **Policy is independent of helper availability:** A configured global write-disabled state, action-specific allowlist, destructive confirmation or provider-readiness blocker still denies writes **even if** the helper is installed and working. Conversely, an unavailable helper must not block an operation that can safely run with the ordinary account's own privileges.
- **Transitions:** Installing/upgrading/removing the helper, revoking sudo, changing helper ownership or changing its protocol must update capability discovery and enforce the new limits on subsequent requests. An in-flight privileged operation follows normal bounded job semantics and records whether native effects were committed; never report cancellation/rollback it cannot guarantee. Keep drift and recovery blockers visible across helper transitions.

**Feature-specific no-helper examples (not an exhaustive substitute for the matrix):**

| Feature | No-helper expected behavior | Helper-enabled behavior |
| --- | --- | --- |
| Overview, processes, monitoring | Available to the extent allowed by normal Linux permissions | Identical, no unnecessary elevation |
| Services | System/user inventory and logs where readable; user-scope actions still work; protected system-scope writes disabled | Explicitly allowlisted protected system-scope writes available |
| Updates | Cached/package information where readable; privileged refresh/install/dependency actions disabled | Reviewed privileged refresh/install/dependencies available |
| Applications / VMs | Rootless and independently delegated Docker/Incus/libvirt operations work; no global feature disablement | Same plus separately authorized host-side setup actions |
| Storage | User-owned safe provisioning and read/write workflows continue | Same; protected host paths only if the reference authorizes a helper operation |
| Samba / NFS | Read-only status/configuration and user-owned draft/edit/review available; protected publication/setup/password operations disabled | Reviewed protected publication/setup/password operations available |
| MCP and API | All reference read/write policy and transport support retained; unauthorized protected mutations rejected with structured reasons | Full authorized reference operations subject to independent policy |

**Acceptance:** Execute the same parity matrix in two real non-root Linux deployment modes: **(A) no helper/no sudo rule** and **(B) helper installed, owned, version-matched and authorized**. Verify expected capabilities/actions, UI and MCP parity, error codes, read-only continuity, rootless workloads, protected operation success in B, protected denial in A, no silent elevation, non-root UID, dynamic revocation/re-enablement, and negative ownership/protocol/timeout cases in disposable native guests. Both modes are release-blocking, while the 100% feature coverage and implementation parity target is verified in B.

### Cross-reference and acceptance rule

For each parity row, **Go reference test and implementation → Rust backend/API → Linux UI/MCP → native safety/privilege test → recorded evidence** must form a continuous chain. Every incomplete row is a Phase 2 blocker, unless marked as a *not-implemented-at-reference* future feature or covered by a specific user-approved scope exception. Do not treat an unavailable backend, simulated mode, mock, green CI, generated UI card, provider renderer or a documentation-only checklist as native acceptance. Reconcile documented feature lists, actual registry/tool surfaces, README and completed checkboxes before declaring any milestone done.


### Current Phase 2 branch review

The `feature/linux-phase2` worktree is based on Maestro v0.11.0 and contains
in-progress tracked and untracked changes. The table records implementation
present in that worktree. Native acceptance is tracked separately below.

| Area | Implementation present | Adjustment after plan review |
| --- | --- | --- |
| Linux shell and operations | Embedded Svelte shell, tool registry, token browser sessions with Host/Origin checks, tracked jobs and audit | Finish administrator credentials, durable sessions and complete reference tool/UI/permission contracts. |
| Systemd, monitoring and updates | System/user inventory, journal, allowlisted actions, sampled metrics and package status/refresh/install across four managers | Verify mutations, boot persistence and package writes in disposable native guests. |
| Declarative workloads | Typed YAML, exact history, review, apply, journal, resume and reviewed revert | Validate provider recovery end to end and finish migration of the legacy server stanza. |
| Providers and UI | Quadlet, Podman and Docker Compose, libvirt, Incus, Samba/NFS, storage provisioning, and Applications and File Sharing pages are present | Reviewed managed-directory provisioning is enabled. Native Compose, libvirt, Incus, disk provisioning and share mutations remain gated pending ownership-safe implementation. Verify ownership, rollback, runtime and boot behavior for each native provider. |
| Build, registry and routing | The `linux` feature gates the tool router and native modules. `linux-ui` also embeds Svelte. CI has separate profile checks. | Validate the release artifacts, compile-time dependency graph and ASUS hardware resource deltas. |
| Agent access | HTTP JSON-RPC MCP shares the Linux tool registry | Port full existing HTTP/stdio MCP contracts and Agent access UI; capture/inventory/recovery parity is required, while *full restorable backup/restore* is a separate future project. |


### Phase 2 reconciliation increment — 2026-11-23 (unverified implementation, M2 open)

See [server-man-phase2-parity-matrix.md](server-man-phase2-parity-matrix.md) for the pinned-reference tool ledger, missing operations, privilege split and evidence gaps. An initial source audit enumerates 60 public registration names plus 3 helper-only share-definition verbs; the earlier documented 55-tool SDK count must be reconciled against a live pinned Go registry and official MCP client. This is not accepted parity.

The existing branch has been **preserved**. Linux-only `maestro-admin` and a non-root production runtime guard were introduced, and systemd system-scope writes now require root-owned helper policy *and* Maestro's unit allowlist; user-scope operations remain ordinary account operations. Discovery, status and Services UI expose unavailable reasons. Package writes now fail closed rather than relying on the main daemon's root UID, but helper package/share verbs, two-mode installer and native helper testing are not yet complete. **Do not install or authorize the helper on production systems from this preliminary code.**

Source commits: `584ccc22ad89a7b5145c876ed67467d509c06d73`, `70b9a8cffc58366e30e4f762d1c3c4f7f8d92ef4`, `acf9a6ff774bdd47184fa7ea8a3d141a8770866e`.

**Follow-on work, still not accepted:** `fdf5d63691ae` source-level 60-operation pinned reference parity ledger; `dd5b8a61d172` and `f9ba2d8582bb` read-only systemd details/parser/UI; `f0209ad33c53` Linux-only checksummed opt-in helper artifact and Linux privilege docs; `9dfee69fc35f` and `b426ff171a00` shared API/MCP and helper-side destructive systemd confirmation; `289dcbcec849` bounded helper output, fixed policy ownership/size probe, and router-only profile rejection of unsupported Linux mode. Latest observed branch head: `289dcbcec849e27a6bb94cb6819043ccbe28c783`. GitHub comparison confirms 10 commits ahead of the audited WIP baseline with no CI, release, version or main changes. GitHub Actions reported **0 workflow runs** for `feature/linux-phase2` at this review. No Rust compiler, browser toolchain build or native disposable guest was available in this session; fmt/clippy/cargo tests/UI and both Linux deployment acceptances remain **not run**. Nothing is marked native verified or complete.

**Next execution:** restore a Rust-enabled disposable Linux workspace; compile `--no-default-features --features linux` and `linux-ui` and router default with `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` separately; use lint/compiler findings to correct this unverified work. Then run non-root/no-helper tests, helper install with restricted sudo in an isolated guest, and revocation/ownership/timeout/confirmation tests. Only then proceed to implement/enable package, dependency, native share, backup and installer helper verbs. Do not enable protected mutations just to pass fixture checks. Tests/CI, Rust compilation, disposable guest native validation, browser acceptance and hardware resource checks **have not been run for these commits**. Existing earlier test records cannot verify the new changes. All unfinished checklist items remain unchecked.

### Phase 2 continued implementation stream — 2026-11-23 (verification pending)

The branch continued beyond the initial privilege increment. See the [living parity matrix](server-man-phase2-parity-matrix.md) for concrete source paths, operation-by-operation states and outstanding native gates. Added in source: fixed helper package refresh/upgrade/named/dependency installation with independent root-owned opt-in; source-level Samba password administration bound to the trusted sudo caller; private durable browser sessions; a reviewed non-root Linux installer with optional helper selection; shared MCP dispatcher and Linux stdio binary; missing Configuration/Agent Access/Activity screens; strict typed resource preview/apply and read-only non-restorable backup-input inventory. Protected Samba/NFS publication, full backup capture/recovery, administrator setup and 100% provider parity are **not complete**. Deferred typed publication still refuses safely.

The non-root systemd service template was corrected so the explicitly authorized `sudo -n` helper is not accidentally blocked by `NoNewPrivileges`/empty capability bounding settings. It still runs the main daemon as the chosen workload owner and requires the restricted root-owned sudoers/policy boundary. This is a design correction **pending native security acceptance**, not a privilege-boundary signoff. Lazy Linux UI assets are explicitly embedded only in the `linux-ui` build.

**No build or native acceptance was executed for the continued source commits in this connector-only session.** Phase 2 checkboxes remain unchecked. Under the subsequent direct-port instruction, continue the still-missing source implementation in reference dependency order while the operator handles local compilation and testing; incorporate findings and execute the full acceptance gates before M2. Do not merge/release or change CI triggers.

### Step 4 prerequisite — Platform builds and router resource baseline

Complete this prerequisite before adding more Linux-only dependencies or
continuing native provider work. Preserve the existing Linux slices and apply
the boundary at their module and dependency entry points.

- Keep **Axum and Tokio** as the shared HTTP and async infrastructure. Share
  domain modules, REST contracts, configuration and command execution where
  both targets use them. Keep the ASUS ASP renderer, Entware init scripts,
  router upgrade and router-targeted integrations in the router build.
  Shared feature modules remain available to both target compositions. Gate
  the Server Man-specific richer sampler and provider dependencies separately
  from the existing lightweight common monitoring code.
- Use `asuswrt` and `linux` for platform modes and their matching Cargo
  features. Keep `asuswrt` as the default for
  existing build commands and Entware packaging. Select `linux` with
  `--no-default-features --features linux`. Make `linux-ui` imply `linux`
  and select it for the Linux UI release. Allow backend-only Linux builds
  for tests and development. Keep release artifacts and CI target directories
  distinct. Test the two shipped profiles independently.
- Apply `#[cfg]` to Linux-only modules and routes, including the tool bridge,
  session middleware, management jobs, systemd, resource sampler, package
  managers, declarative coordinator, provider adapters and Svelte assets.
  Mark Linux-only dependencies optional and attach them to `linux`. The
  branch adds `quick-xml` for Zypper and `serde_yaml` plus
  `unsafe-libyaml` for workload YAML. Audit their transitive dependencies with
  Cargo's resolved graph. The explicit `asuswrt` dependency check confirms
  that the router graph excludes all three. Apply the module gates in both
  the binary and library entry points. Shared command, system usage and
  configuration modules stay available where existing callers need them.
- Retain runtime discovery for features present in the selected build.
  Report build exclusion as unsupported and preserve the existing distinctions
  between configuration-disabled and operationally unavailable capabilities.
  Test actual route assembly as well as discovery. A runtime platform check,
  an empty configuration or a stripped symbol list alone cannot establish
  compile-time exclusion.
- Replace Tokio `full` with the smallest proven feature set across both
  builds. Start with the observed runtime, networking, Unix signal, process,
  filesystem, I/O, synchronization, time and macro uses. Audit features
  re-enabled through Axum, Reqwest and test dependencies before claiming a
  reduction. The v0.11.0 manifest explicitly enabled Axum `macros` and `ws`.
  Phase 2 selects Axum's HTTP/1 server, JSON, query extraction, Tokio and
  tracing features and specifies Tokio features explicitly. The router CI
  check rejects WebSocket dependencies and unused Axum or Tokio features
  if a dependency enables them again.
  Retain any feature required by existing behavior.
- Start clients, tasks and caches only for compiled, enabled capabilities
  that use them. Verify the router creates no Linux sampler, browser session
  store, job store, package probes or provider state. Check the shared startup
  path for eagerly initialized optional integrations. Disabled AdGuard
  skips LAN IP discovery during startup and nginx site changes. Retain the current
  release optimization settings initially. Compare further tuning against
  measured resource and functional results.
- If Rhai is later included, put it and its adapters behind an optional
  `scripting` Cargo feature excluded from default router builds. Keep it
  independent of the platform profiles so a future explicit router opt-in can
  be evaluated against hardware measurements.

**Implementation order.** Record the v0.11.0 baseline and capture current
build graphs first. Add the profiles and route/module/dependency gates next.
Then trim unused runtime features and check startup and discovery. Pass the
profile and router parity gates before resuming Step 4 gaps, Steps 5–6 native
acceptance and Steps 7–10 in their existing order.

### Step 4 — Linux UI shell, operations bridge and authentication

- Reuse Server Man's Svelte 5/Vite shell and responsive styles; compile to static assets only in the Linux release. Keep ASUS build and IPK free of the Svelte build toolchain/assets.
- Adapt the existing Svelte `invokeTool`/`startJob` client: preserve or provide a compatible `POST /api/tools/{name}` transport, tool/module discovery, tracked jobs and predictable JSON error codes. Keep Maestro's existing REST endpoints working.
- Port the core registry concepts (schema validation, read/write policy, bounded output, management-job locking, audit, cancellation) into Rust incrementally.
- Port **complete** Linux authentication: one-time token-authorized administrator setup, username/password authentication, token fallback, durable HttpOnly/SameSite session cookies and logout/revocation, Host/Origin checks, first-use setup and tests. Do not change ASUS bearer-token behavior. Require explicit permission boundaries for privileged Linux actions.
- Establish a shared navigation/dashboard contribution contract; Linux tab assembly uses Svelte components and lazy loading, while ASUS keeps its embedded renderer. Port every Server Man screen and shared UX workflow (including typed editors, Activity, Configuration, Agent access, readiness alerts and theme switching), not only the current five Linux pages.
- Gate with Linux UI login/auth, discovery, job lifecycle, responsive routing and no regression to ASUS paths.

### Step 4a — Linux unprivileged execution and optional privileged-helper deployment (mandatory implementation)

- Implement and package the Linux-only `maestro-admin` restricted root-owned helper described above, with a reviewed typed protocol and no persistent elevated service. **The helper is optional to install, not optional to implement;** include a helper-free deployment path. Linux Maestro itself runs as the normal workload owner in both modes; ASUS remains root with no helper.
- Port Server Man's privileged-operation routing, helper request validation, fixed allowlisted operations, **opt-in** host installation/sudoers rules, audit, error reporting and rejection tests. Implement helper health/capability discovery and per-operation disabled reasons across Linux UI, HTTP and MCP. Verify read-only/rootless continuity without helper access.
- Route protected system units, package writes, Samba/NFS and Samba-password/setup operations through the helper. Keep user-scope systemd, Podman rootless and unprivileged functions direct. Never use helper access as a shortcut around managed-resource safety.
- **Gate:** non-root native guest tests prove **both intentional helper-free and helper-enabled deployments**, read-only/rootless continuity, successful authorized privileged operations with helper, rejected arbitrary requests/paths, denied sudo policy, helper removal/revocation/unsafe permissions, no helper route in router build, and no process-wide Linux root requirement.

### Step 5 — Linux systemd Services adapter and shared Services UI

- Port Server Man's system/user `systemctl` and `journalctl` implementation under the common `ServiceManager` interface.
- Preserve installed vs loaded units, partial inventory warnings, bounded logs, validated `.service` names and explicit boot enablement semantics.
- Preserve restricted system privilege routing through the Linux-only helper **when present and authorized**; leave readable system inventory and ordinary user-unit actions available without it. Application-owned generated units must be directed to the declarative coordinator once Applications exists; native system/user service details and ownership distinctions must retain Server Man behavior.
- Compare outputs and behavior against original Go tests and native systemd environments.

### Step 6 — Linux monitoring and package updates

- Port bounded Linux resource/process histories, metrics and dashboard widgets with isolated refresh/error handling. Start the shared sampler on the first monitoring request so startup and deployments that never read metrics avoid periodic host probes.
- Port distribution-aware package status/refresh/install for apt, dnf, pacman, zypper, with exact validated operations, job locks, privilege checks, reboot evidence and read-only cached queries. **Also port named package installation, backend dependency presets, server setup actions and installer package-selection behavior**, using the Linux helper for privileged writes **only when installed and authorized**; otherwise preserve readable status and disable protected writes with explicit reasons.
- Preserve router `opkg`/self-upgrade behavior as separate platform operations; do not confuse router-specific binary upgrades with Linux OS updates.

### Step 7 — Applications declarative foundation (no provider shortcut)

- Port typed workload YAML `version: 1`, validation (duplicate keys, custom tags, refs, dependency cycles, identity rules), revision-checked owner-only store, exact-byte history and conflict handling.
- Preserve Server Man's **single authoritative Linux YAML** for its server/access settings **and** managed workloads, storage/bindings, private file references and shares, or an explicitly approved behaviorally equivalent single-authority mapping. Keep unrelated existing Maestro/router TOML configuration intact. Implement an explicit migration from Server Man's `server` stanza and make server-only edits, restart-required state and mixed-edit protections behaviorally equivalent; never maintain two competing authorities for the same Linux setting.
- Port plan/review/apply, deterministic resource fingerprints/ownership markers, dependency ordering, private journal, preflight/readiness, startup recovery blockers and reviewed revert. Include native artifact checksums, drift/conflict review, metadata-only reconciliation, stale revisions, pending job/reboot guards and explicit recovery paths found in the reference commit.
- Port safe command execution, bounded output, durable jobs and privilege separation first. Do not advertise workload creation until underlying mutation/recovery invariants are validated.

### Step 8 — Applications providers, one tab

Port complete reference adapters in dependency order, sharing translated helpers (native inventory/identity, artifact publication, launcher/boot, dependency ordering, validation and recovery). Translate each provider's full working behavior before optional redesign; test in coherent batches. The ordered provider list is:

1. **Podman Quadlet:** generated owned user definitions, rootless readiness, boot activation.
2. **Podman Compose:** explicit user-scope startup launcher and networking prerequisites.
3. **Docker Compose:** owned definitions, engine/Compose readiness, lifecycle and independent boot policy.
4. **KVM/libvirt:** existing-disk profiles, stable UUID/ownership, persistent definitions, current state and native autostart.
5. **Incus VM:** local project/profile semantics, stopped-guest edits, runtime/boot separation and safe handling of native-owned resources.

All providers appear in **one Applications tab** with type/backend filters and typed editors; no provider-specific top-level tab. Preserve start/stop distinct from enable/disable, partially running group diagnostics, logs, configuration-drift/status separation, safe stopped/deferred edits, failed launcher diagnostics, host port/storage/network preflights, readiness and conservative refusal on unverified native ownership. Include all Server Man-supported v1 fields/actions, stable identities, container/VM lifecycle semantics and UI recovery flows; test cold boot and process/daemon restarts for each provider.

### Step 9 — Storage provisioning, Samba/NFS shares and configuration health

- Port typed storage/binding prerequisites and explicitly reviewed creation of missing owned directories/disks; include supported checksum-pinned initial disk-image cloning, read-only filesystem/mount identity and permissions checks; do not invent missing data or mount points.
- Port Samba/NFS ownership markers, bounded native inventory, conflict detection, candidate validation, publication/rollback, typed UI editors, Samba-password operations and privilege-helper restrictions. Preserve unmanaged server configuration/data and independent share service setup. Without helper, keep read-only and user-owned editing/review available while disabling protected publication/password/server-setup actions.
- Expose File Sharing and Configuration workflows, typed resource editors, global/per-resource configuration-health alerts and explicit repair actions. Dashboard widgets remain optional; **existing Server Man UI features are not optional**.
- Share model/recovery/preflight infrastructure with Applications rather than creating a second source of configuration truth.

### Step 9a — Production Linux installation, packaging and upgrade (mandatory)

- Port Server Man's native source/release installer as a Maestro Linux distribution workflow: dry-run/plan/confirmation, selected backend dependencies, non-root workload account, reviewed group/sockets/lingering prerequisites, **explicit optional helper/sudoers installation**, systemd service, private single-authority configuration, package/version policy and checksum-verified release artifacts. A no-helper installation must be supported without attempting privileged package/setup changes, and must show actionable manual prerequisites. The helper can be added or withdrawn later without replacing the main Maestro installation.
- Preserve existing credentials/configuration and managed data on reinstall/upgrade. Verify **both no-helper and helper-enabled** clean installation, repeated installs, helper opt-in/removal, denied/malformed sudo policy, permission failure, package absence, safe no-op and rollback/recovery guidance in disposable Ubuntu/Debian environments. Do not require build tools on the target when using a release archive.
- Keep Linux installation/package management entirely out of the router/IPK build and runtime.

### Step 10 — MCP/agent interface and existing backup/capture features (mandatory)

- Port **all existing** Server Man MCP features: Streamable HTTP and stdio transports, supported protocol negotiation, schema and tool-operation parity with the JSON registry, structured/error results, auth/write/confirmation policies, jobs/cancellation, Agent access UI and documented standard-client setup. Test with the official MCP SDK; do not claim parity from a custom JSON-RPC handler alone.
- Port Server Man's **implemented** backup inventory, capture, native/portable metadata collection, verification and interrupted workload/publication recovery semantics and corresponding UI/API/MCP operations as inventoried at the pinned reference commit. Preserve safeguards that block conflicting mutations when recovery is pending, even if complete backup/restore is unavailable.
- **Full, successfully restorable backup/restore is not implemented by reference Server Man and is a separate future milestone.** Capture artifacts must never be advertised as restorable backups. No completed Server Man functionality can be deferred without explicit user approval.
- Retain source/test references to the original Go version until production Linux acceptance is complete.

## Phase 2 acceptance gates

For **every inventoried Server Man feature**, require contract fixtures compared to Go, Rust unit/API/operation-schema tests, Linux UI tests, non-root permission tests in **helper-free and helper-enabled** configurations, native Linux guest acceptance wherever host mutations or real providers are involved, failure rollback, restart/boot persistence, and interrupted-operation recovery where applicable. Ubuntu is the first deployment target; Debian follows according to available native evidence; Arch is not claimed validated merely because package mappings exist. Keep host-specific tests in disposable VMs or other isolated environments.

**Hard M2 completion rule:** Zero unresolved *existing reference feature* rows in the parity matrix. Every row is `accepted` with evidence or has a specifically recorded user-approved change to the 100% requirement; `modeled`, `gated`, `unit-tested` and `CI green` alone never qualify. Ensure **full Server Man parity with the optional helper configured** and **correct limited-privilege operation without it**. Both deployment modes must run the main Linux process as non-root; user-owned apps remain rootless. Protected writes require the helper and separate policy approval; no-helper mode must keep independent read-only/rootless functionality and deny protected writes with actionable reasons. Exercise helper missing/unsafe/mismatched/revoked states, missing-backend/data, permission-denied, artifact drift, rollback, interrupted journal and restart conditions. Confirm the reference backup/capture limitations explicitly rather than dropping the implemented functions. Verify every Server Man UI workflow, tool/schema/transport and non-destructive setup path; check native Ubuntu and Debian gates per the reference's accepted support scope, with Arch explicitly unverified unless separately approved.

**Scope freeze and sign-off:** Keep the Server Man reference pinned; changes to matrix classification, accepted behavior or non-implementation exceptions require review. Do not merge, tag or announce Phase 2 feature parity until the complete matrix, optional-helper threat/sudoers review, **both deployment-mode** Linux production installer tests and native acceptance record are signed off. **Do not defer helper detection, no-helper fail-closed behavior, least-privilege authorization or parity safety checks to a hardening Phase 3.** No code changes or runtime permission escalation are authorized by this documentation update alone.

### Router resource and profile acceptance

Use the published **Maestro v0.11.0** artifacts and immutable tag as the
router baseline. Record the tag commit, artifact checksums, toolchain, target,
strip settings, router model and firmware, configuration and workload. Rebuild
the tag with the same target and toolchain as the candidate when needed to
separate compiler effects from source changes. Use matching ASUS hardware for
the runtime comparison and repeat samples under equivalent conditions.

The [published v0.11.0 IPK](https://github.com/vm75/maestro/releases/tag/v0.11.0)
is 4,258,037 bytes and contains an 8,874,336-byte executable. The downloaded
IPK passed its published companion checksum. Its SHA-256 is
`ebf12ca1732e6cdde2fc59918c33fbd569657baa55b741b19a257d901e6a60ac`.
The local v0.11.0 tag resolves to `4c8b4eb2013b6b60d297bb9c5bc894e3453efde1`.
The release workflow at that tag specifies Rust 1.85.0 and
`aarch64-unknown-linux-musl`. The workspace's separate local IPK differs
from the published artifact. Use the published sizes for release acceptance.
Hardware measurements remain pending.

With explicit `asuswrt` features and the reduced Axum feature set, the
Phase 2 candidate built locally to an 8,791,264-byte stripped executable.
Three repeated IPK packaging runs ranged from 4,135,541 to 4,135,571 bytes,
with a median of 4,135,546 bytes. Compared with the published v0.11.0
artifacts, the executable and median IPK were 83,072 bytes (0.94%) and
122,491 bytes (2.88%) smaller, respectively.

Both the candidate and a rebuild of the immutable v0.11.0 tag used local
Rust 1.99.0, cross GCC 16.2.0 and `aarch64-unknown-linux-musl`. The rebuilt
baseline measured 8,856,800 bytes for the executable and a median of
4,167,464 bytes for the IPK. With matching local tools, the candidate's
executable was 65,536 bytes smaller (0.74%) and the median IPK was 31,918
bytes smaller (0.77%). The candidate passed package checksum, payload
and Linux asset exclusion checks. The pinned release-toolchain CI result
and on-router measurements remain part of acceptance.

| Measure | Evidence to capture |
| --- | --- |
| Stripped executable and IPK size | Record byte counts for the deployed stripped executable and compressed Entware IPK. Compare identical target and packaging options with `wc -c`, inspect the IPK payload, and report absolute and relative deltas. |
| Idle and peak RSS | After readiness, sample `/proc/<pid>/status` for `VmRSS` in a fixed idle window and during representative active workloads. Record `VmHWM` and the peak sampled RSS. Use fresh starts per scenario and identify helper process memory separately. |
| Idle and active CPU | Take deltas of Maestro process CPU ticks from `/proc/<pid>/stat` and elapsed monotonic time over identical idle and active windows. Normalize consistently and identify helper process CPU separately. Use the same representative dashboard/API, NGINX and firewall/log workloads. |
| Startup | Time from service launch to the first successful health response and authenticated readiness request using the same polling cadence. Record repeated cold starts and service restarts, with median and range for both runs. |
| v0.11.0 parity | Run the M1 router matrix against the baseline and candidate, including ASP/menu entry, token/API behavior, nginx, certificates, firewall and security integrations, init scripts, optional modules, IPK install/upgrade and rollback. Verify actual router hardware and representative persisted state. |
| Dependency and startup isolation | Compare `cargo tree -e normal,build,features` under explicit features and production targets. Assert that Linux-only crates are absent from the router's resolved build graph even if listed in `Cargo.lock`. Verify no Linux routes/assets in the router runtime or IPK, no Node invocation in router builds, and no Linux tasks, probes or state files on router startup. Verify Linux features resolve and start only in their intended runtime configuration. |

Record raw measurements and run-to-run variance with the acceptance results.
Investigate unexpected increases and require reviewer sign-off for any
justified regression. Establish the baseline before setting numeric limits.
Acceptance requires no unexplained resource regression, proven dependency
isolation and v0.11.0 router parity.

Add separate CI checks for the default and explicit
`asuswrt` profile, and for the explicit `linux` profile with its UI feature.
For the router, check formatting, run locked tests and Clippy, cross-build the AArch64
musl release, verify the IPK and assert graph, asset and route isolation. The
IPK check packages the actual cross-built executable, verifies the payload and
checksum, and reports executable and IPK byte counts in the CI log. For
Linux, build and check the locked Svelte frontend, run Rust and browser
contracts, build the native Linux artifact, and run the unprivileged nginx
smoke test against that release executable. Keep hardware resource
measurements and native mutation acceptance as separate evidence from CI.
Use existing manual CI rules and preserve release triggers and tag policy.

The final acceptance matrix must cover:

- Router: unchanged ASP/lightweight UI, APIs, package upgrades, NGINX and security integrations, no Svelte assets, and accepted v0.11.0 resource and isolation results.
- Linux: Svelte UI, auth sessions, system/user systemd, monitoring, native package updates, all intended Applications providers, storage/share management and necessary recovery semantics.
- Cross-platform: correct capability discovery, tab/section composition, optional dashboard contributors, and isolation of unsupported modules.
- Release: reproducible build artifacts, platform-specific packaging and migrations, stable versioning and safe update paths.

## Workflow, PRs, and CI discipline

- **Phase 1 branch was for the plan and Phase 1 implementation.** Use small feature-oriented commits; split into reviewable PRs if desired without merging incomplete work into `main`. Draft PRs do not trigger the current CI workflow because `pull_request` is not configured.
- During Steps 1–3, run tests **locally** as needed and record results. GitHub Actions CI stays untriggered on `refactor/modular-architecture`. Do not use auto-CI branch names (`ci/**`) or manual dispatch.
- After Steps 1–3 are complete, request a **manual** CI run for the branch or PR using the existing workflow dispatch. Fix failures before M1 acceptance. No automatic trigger is added.
- Phase 1 was merged and tagged v0.10.5 under Router Hub before this rebrand. A push to `main` normally triggers CI and may trigger auto-version tagging; coordinate version and release policy accordingly.
- The Maestro Phase 1.5 rename is in its own dedicated branch and will require a separate PR and release. Re-run all relevant tests and acceptance.
- Start Phase 2 only after both M1 and M1.5 are accepted. Use separate implementation branches for Linux features/providers.
- The NGINX standalone build (`.github/workflows/build-nginx.yml`) and toolchain-image build remain **manual only**, unrelated to Phase 1 CI policy.
- If workflow triggers are changed in the future, re-audit these assumptions **before pushing**, because branch name alone is not an enforcement control against repository-level automation.

## Risks and mitigations

| Risk | Mitigation |
| --- | --- |
| ASUS menu/ASP stops working after JS modularization | Preserve self-contained rendered body/style/script shape; regression test both standalone and ASUS embedding; test on hardware |
| Router binary size, memory, CPU or startup regresses | Gate Linux code and dependencies at compile time and compare repeated v0.11.0 hardware measurements |
| UI hides feature operations | Inventory all existing screens, command handlers, modal flows and deep links; matrix-based browser tests |
| Platform abstraction changes firewall/NGINX semantics | Extract adapters without rewriting operations; stable API and rollback fixtures |
| Existing config/data or upgrade break | Keep paths/schemas through M1; separate versioned rebrand migration with rollback |
| Legacy Router Hub installation conflicts with Maestro | Keep the IPK conflict guard, run an explicit one-time data transfer, remove old boot hooks, reboot to clear old firewall chains, and preserve offline rollback artifacts |
| Go workload port loses transactional safety | Preserve revision, ownership, preflight, journal and review gates; native guest acceptance |
| Existing Server Man features silently omitted or mislabeled optional | Pin reference commit, inventory every tool/UI/backend/recovery feature and block M2 on incomplete parity rows |
| Linux process accidentally runs as root or invokes overly broad sudo | Non-root server requirement in both modes; optional separately owned helper with explicit opt-in, typed root-side validation, restricted sudoers and negative tests |
| Linux installation/update breaks rootless ownership or host data | Explicit reviewed installer with helper-free and helper-enabled paths, workload account/group/lingering checks, preserved config/data and disposable-guest reinstall/upgrade acceptance |
| Missing/unauthorized helper disables whole features or permits an unsafe fallback | Per-action capability discovery and fail-closed authorization; maintain read-only/rootless paths; negative tests for missing, revoked, unsafe/mismatched helper and MCP/UI parity |
| CI accidentally runs before M1 | Keep branch outside `main`/`ci/**`, no tags or manual dispatch; audit changed workflows |

## Completion checklist

- [ ] B0 baseline/test inventory completed.
- [ ] Phase 1 step 1: backend and UI module contracts implemented.
- [ ] Phase 1 step 2: Rust feature/platform modules reorganized with parity.
- [ ] Phase 1 step 3: ASUS dashboard and tabs modularized, still offline/build-free.
- [ ] M1: local suite and manually initiated CI pass after implementation.
- [ ] M1: ASUS hardware, package upgrade and resource parity accepted.
- [x] Phase 1 release merged/tagged under original name (v0.10.5).
- [x] Phase 1.5: Maestro selected; original `vm75/server-man` untouched as a Phase 2 reference.
- [x] Phase 1.5: source, UI, CLI and package rebrand with controlled migration implemented (GitHub repository rename deferred).
- [x] M1.5: rebrand compatibility and router deployment validated (operator confirmation).
- [x] Phase 2 prerequisite: `asuswrt` and `linux` profiles and CI isolation checks implemented. Local AArch64 build, IPK smoke test, dependency and route isolation, and the full Rust 1.85.0 `asuswrt` test suite (322 tests) passed.
- [x] Phase 2 AArch64 guest smoke: installed the candidate IPK with Entware in an offline Alpine/QEMU overlay. The service started in `asuswrt` mode, rendered the ASUS ASP and menu entry, served health and authenticated runtime requests, returned 404 for Linux paths, and created no workload state. The guest used lab OpenSSL and loopback settings.
- [ ] Phase 2 router acceptance: pinned-toolchain CI and matching-hardware resource and parity measurements against v0.11.0.
- [x] Phase 2 local Linux artifact: pinned Rust 1.85.0 `linux-ui` release built and passed the unprivileged loopback smoke test for Linux tool discovery, token API authentication, metrics and nginx.
- [x] Phase 2 local Linux browser smoke: headless Chromium verified token sign-in, narrow-viewport navigation, lazy Applications loading, session persistence after reload and sign-out against the loopback release binary. A follow-up run against the updated debug build verified that edits invalidate the current workload preview and a blocked native Compose resource hides Apply.
- [ ] Phase 2: Svelte shell, complete Server Man UI workflows, operation registry and Linux auth accepted. Token sessions are implemented; administrator credentials and durable sessions remain pending.
- [x] Phase 2: systemd Services + monitoring + Linux package updates implemented and tested.
- [x] Phase 2: declarative coordinator/storage model and Quadlet provider implemented. The native Podman generator accepted the rendered rootless units in dry-run mode. Systemd lifecycle and boot persistence need native guest validation.
- [ ] Phase 2: Applications UI and native Podman Compose, Docker Compose, libvirt and Incus acceptance.
- [ ] Phase 2: native Samba/NFS, provisioning and configuration-health acceptance.
- [x] Phase 2: MCP JSON-RPC protocol interface and agent tools integration.
- [ ] Phase 2: **100% reference parity inventory** completed against pinned Server Man `c1a9f32f5898146ae7ac339043b910b8e2a2c438`, with traceability to all reference tools, UI, native behavior, tests, partial backup/capture and setup/release operations.
- [ ] Phase 2: Linux server runs as non-root workload account **both with and without** the optional restricted root-owned helper; router profile isolation and denied/positive native tests verified.
- [ ] Phase 2: Helper-free Linux installation, start/restart, readable inventory, rootless operations, action-level disabled explanations, UI/API/MCP parity, and helper availability refresh verified.
- [ ] Phase 2: Opt-in helper installation/removal, restricted sudoers, helper version/ownership/protocol probes and privileged feature parity verified, including live revocation without silent root fallback.
- [ ] Phase 2: All Server Man native operations (packages/dependencies/share setup, applications/VMs/storage/shares, boot, recovery, native readiness) and operator UI workflows accepted.
- [ ] Phase 2: Complete administrator authentication, durable sessions, production Linux installer/reinstall/upgrade, and both HTTP+stdio MCP verified with official clients.
- [ ] Phase 2: Existing Server Man backup inventory, capture, manifest verification, pending-journal and reviewed publication/recovery functionality ported with explicit non-restorable status.
- [ ] **M2 hard parity gate:** Every implemented reference feature is accepted with linked test/native evidence; no unapproved omission or downgraded safety contract.
- [ ] Phase 2: Linux native acceptance, router parity and releases completed.

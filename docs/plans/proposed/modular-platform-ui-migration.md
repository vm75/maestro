# Phase 2 remaining acceptance

Phase 1 (modular architecture) and Phase 1.5 (Maestro rebrand) were accepted.
The completed proposals are retained in Git history, not as active plans.
Phase 2 implementation is on `feature/linux-phase2`; native acceptance is
not yet complete. The [reference parity ledger](server-man-phase2-parity-matrix.md)
records source and test evidence against Server Man commit
[c1a9f32](https://github.com/vm75/server-man/tree/c1a9f32f5898146ae7ac339043b910b8e2a2c438).

## Required behavior

- One modular application: shared nginx, certificates, monitoring and BanShield;
  router-specific ASUS UI, Entware, AdGuard and WireGuard; Linux-specific
  applications, shares, native host services, package updates and helper.
- Preserve all deployed Maestro 0.11.0 router behavior and lightweight router
  builds. Native Linux must cover the existing implemented Server Man feature
  set, including the original helper operation contracts and existing UI,
  REST and MCP workflows.
- Linux runs non-root, with a mandatory-to-implement but optional-to-install
  privileged helper. Both deployment configurations are supported; each
  protected operation requires the appropriate root-owned grant.
- Native Linux uses one private version: 1 YAML authority; router uses its own
  YAML schema. No Linux migration or legacy fallback. Convert deployed router
  TOML and relocate nginx inputs offline as documented in installation.
- Preserve operational backup, restore, journal recovery, revision history,
  reconciliation and import. Include earlier Maestro runtime features
  (LXD/unmanaged guests and browser terminal); exclude the old Arch installer.

## Verification before merge

Run both Cargo profiles, formatting, Clippy, unit/API/contract tests, Svelte
typecheck/build, packaging tests and router lab tests. Validate actual Linux
providers with disposable workloads, with and without the helper; test negative
authorization, backup/export metadata, cancellation, recovery and restart
behavior. Check browser workflows, authentication, nginx and certificate
publication. Validate router behavior and resource usage on real hardware.
Retain unresolved findings in the parity ledger. Source implementation and
mock tests do not establish native acceptance.

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


## Release discipline

Do not merge until the acceptance gates are met. Build the release binary before
bumping the version; the next release must be greater than 0.11.0 (recommended
0.12.0). Do not retag an existing release or auto-deploy to a router. Follow
[release instructions](../../releases.md).

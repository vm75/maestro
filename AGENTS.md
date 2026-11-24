# Maestro agent guide

## Project overview

Maestro is a Rust service for administrators of Asuswrt-Merlin routers. It
serves a standalone or mounted web UI and a protected Axum API for Entware
services, nginx configuration, certificates, Wake-on-LAN, firewall bans, and
optional AdGuard Home integration. Router-sensitive state is kept in YAML and
atomic JSON files. Managed nginx site inputs live in `<nginx.inputs_dir>/sites.yaml`;
custom nginx objects remain authored configuration files.

## Repository map

- `src/main.rs`, `src/config.rs`, `src/state.rs`, `src/platforms/{upgrade,nginx_upgrade}.rs` — startup, configuration, runtime state, and router-only upgrade commands.
- `src/api/` — authenticated HTTP API handlers and route map.
- `src/modules/{nginx,nginx_sites}.rs`, `src/nginx_render.rs` — nginx filesystem workflows, Jinja rendering, and settings.
- `src/modules/firewall.rs`, `src/ban_attack/` — firewall management and log-driven bans.
- `src/command.rs`, `src/platforms/asus_ui.rs`, `src/storage.rs` — privilege boundary, ASUS page integration, and persistence.
- `src/core/modules.rs`, `src/core/system_usage.rs`, `src/modules/`, `src/platforms/` — capabilities, feature code and platform-specific adapters.
- `web/src/`, `scripts/build-web-ui.py`, `web/index.html` — modular UI sources, deterministic offline composer, and the embedded generated artifact.
- `config/`, `scripts/`, `test-fixtures/` — configuration, router installation, and test-mode support.
- `tests/` — API and firewall integration coverage.
- [`README.md`](README.md) — operator setup and usage.
- [`docs/architecture.md`](docs/architecture.md) — current boundaries, flows, and invariants.

## Working commands

Rust 1.85 or newer is required.

```sh
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
python3 scripts/build-web-ui.py --check
node scripts/test-web-ui.mjs
```

Useful repository shortcuts are `make check`, `make test`, `make help`,
`make fmt`, `make clippy`, `make test-nginx`, `make test-router`, and `make release`.
`make test-nginx` needs Python 3.11+, Jinja2, PyYAML, nginx, and OpenSSL and uses temporary
files and loopback servers. `make run-simulated` (`make run`) starts x64 test mode
with `test-fixtures/etc/maestro/maestro.yaml`; it does not execute router commands or send
Wake-on-LAN packets.

For native Linux development with the Svelte UI, use `make init-linux` once
and `make run-linux` as non-root. For the AArch64 QEMU router guest, use
`make init-router` (builds the IPK and runs installation) then `make run-router`.
Run one environment at a time because their ports overlap. `make test-router`
checks guest scripts with mocked QEMU, without booting a guest. Legacy
`init-lab` (requires a prebuilt IPK), `run-lab`, and `test-lab` remain available.

`make init-linux` uses `scripts/linux-dev.sh` to install Arch/Debian-family
dependencies through sudo, preferring existing mise for Rust/Node and uv for
isolated Python dependencies. Its fallback uses rustup, distro Python and a
checksum-verified local Node archive when needed. `.dev-tools/` is ignored.
Verify the bootstrap without installation using
`python3 -m unittest discover -s tests -p 'test_linux_dev.py' -v`.

For **real Linux integration** use `platform: linux` (or
`--linux-mode`) with `test_mode: false`. `make test-linux` runs a
temporary unprivileged real-nginx smoke test; it does not exercise live
firewall enforcement, DNS providers, or router firmware. Keep real firewall
tests in isolated network namespaces/VMs and always use lab-only paths.

`make test-quadlet` validates rendered definitions with the installed native
Quadlet generator without starting units (`MAESTRO_QUADLET_GENERATOR` overrides
its path). `make test-podman` requires a non-root account, rootless Podman and a
C compiler with static libc and Rust/Cargo. It builds an offline scratch image,
checks engine lifecycle/fingerprint labels and runs the native metadata-reader
ownership tests using only fresh verified temporary storage/runtime roots, no pulls and no networking. It cleans up that private lab; it does not
configure user units. These checks do not establish complete provider acceptance.

For a release artifact, use `./scripts/build-release.sh <rust-target>`.
Use `make ipk` to build and package the AArch64 musl Entware IPK and checksum
in `dist/`, deriving the package version from `Cargo.toml`.
The `scripts/package-ipk.sh` helper and `scripts/test-package-ipk.sh` smoke test
build and validate a static-musl AArch64 IPK without an Entware toolchain.
The repository contains a helper default for `aarch64-unknown-linux-musl`,
but a target is supported only after testing it on the matching router CPU and
firmware. Legacy MIPS support is not promised.

## Release workflow

CI runs on every push to main and opt-in `ci/**` branches. Other PRs are
manual: workflow_dispatch accepts an open PR number to check its proposed merge,
or no number to check the selected branch. Successful push CI on current main
automatically tags a new stable version matching `Cargo.toml` and `Cargo.lock`.
Tagging requires the release GitHub App credentials documented in `docs/releases.md`.
Annotated version tags trigger the separate full release checks and publication
of AArch64 musl archives/IPKs with SHA-256 checksums. Manual tags remain supported.
Do not commit `dist/` output or retag published versions; never deploy to
production routers automatically.

The standalone Entware NGINX builder has a **manual-only** GitHub Actions
workflow (`.github/workflows/build-nginx.yml`). Its uniquely tagged
`nginx-v*` GitHub Releases must always be created with `--latest=false`;
stable Maestro releases remain Latest for the router-only upgrade command.
Do not introduce automatic NGINX build triggers or router deployment.

## Engineering constraints

- Follow KISS and YAGNI. Make the smallest change that satisfies the concrete request.
- Keep context lean: search first and read only task-relevant files; do not scan generated, vendored, or build-output trees by default.
- Do not read, search, or modify the `data/` directory unless explicitly requested by the user. It contains configuration and logs mirrored from the deployment router.
- Preserve existing behavior and user changes. Do not reset or overwrite unrelated work.
- Build the release binary (`./scripts/build-release.sh` or `cargo build --release`) before committing whenever the version is bumped.
- Run privileged programs through `CommandRunner` as a program plus separate arguments. Never use `sh -c`, interpolation, `eval`, or arbitrary API-supplied command strings.
- Keep router paths configurable and provide a test-mode override for every new router-specific path.
- Preserve atomic JSON and nginx writes, path/symlink protections, token authentication, and bounded log reads.
- Do not claim router compatibility without an actual matching architecture and firmware test.
- Keep the self-contained UI offline-capable; do not add a frontend package manager or CDN dependency. Edit `web/src/`, then regenerate `web/index.html` with `python3 scripts/build-web-ui.py`.
- Phase 1 was merged as v0.10.5. Phase 1.5 rebrands the application to Maestro on `refactor/rebrand-maestro`. Preserve legacy installed configuration, state, NGINX inputs and firewall identities. Keep the existing Go `vm75/server-man` as a Phase 2 reference; do not port its features in Phase 1.5.

## Phase 2 source-guided migration

The user has directed a corrective port-first pass on `feature/linux-phase2`. Server Man at `c1a9f32f5898146ae7ac339043b910b8e2a2c438` is the working implementation baseline, not just a specification. The goal is the fastest safe route to feature parity inside Maestro's existing modular architecture.

- Maestro is one modular application, not two platform-specific products. Shared feature modules include system monitoring, NGINX, certificates/ACME and Ban Shield; reuse their domain logic on both targets, with platform-specific execution adapters only where necessary. Router services and ASUS UI/Entware integration are router-specific. AdGuard and WireGuard are router-targeted for now and may become shared later; do not expand their Linux integration during this port. Preserve existing validated compatibility rather than indiscriminately disabling it. The direct Server Man port is Linux-targeted, reusing existing shared features rather than introducing duplicate implementations. Module targets, dependencies, capabilities and permissions govern routes, tasks and UI contributions; Cargo platform profiles govern build inclusion. Keep the router lightweight.
- Translate complete Go packages and their tests with the same types, algorithms, control flow, validation, native commands, result/error contracts and recovery. Preserve compatible file/function structure; adapt at integration boundaries. Do not create replacement designs, protocols or model layers just for Rust idioms.
- Keep existing Phase 2 code only where it reduces work toward parity. The user authorizes undoing or replacing divergent Phase 2 implementations, including entire modules or Linux UI pages. Use ordinary corrective commits, not history rewrites; leave accepted Phase 1/1.5 router code, user changes and unrelated work intact.
- Preserve the original Server Man helper operation names, JSON request/response contract, fatal stderr/exit semantics and tests. Do not introduce helper protocol versions, alternate operation names or a probe-only wire extension as part of the target design. Keep existing privilege safeguards effective until an equivalent safe integration is implemented.
- Reuse Server Man's Svelte page/component tree, scripts, styles and fixtures wherever possible. Make the Rust API match the pinned reference rather than rewriting pages to fit partial Rust API shapes. Keep the Maestro navigation/module-discovery seam, router UI and upgraded Linux frontend dependencies.
- Use one reference-compatible implementation for the Linux daemon and one-shot helper where they share operations; do not duplicate backend logic. Reuse equivalent Maestro command/job/auth/platform facilities through thin adapters.
- Complete coherent implementation batches, port tests alongside source, and incorporate the operator's local compile/test feedback. No operator pause after each minor commit. Record actual blockers and evidence in the existing parity ledger; do not create more planning layers.
- Retain non-root Linux, optional helper installation, action-specific permission gating, single authoritative Linux YAML with explicit legacy migration, and router build isolation. Defer optional enhancements and extra hardening not needed for safe parity, except the operator-directed old Maestro runtime feature additions explicitly included in Phase 2. Do not port the old Arch installer. Do not claim native parity from source changes alone.

The corrective strategy in `docs/plans/proposed/modular-platform-ui-migration.md` governs Phase 2; older implementation-history entries are not instructions to preserve divergent work.

## Documentation routing

- Update `README.md` for the user-facing overview and quick start; keep detailed installation, configuration, development, Linux integration, and release guidance in the linked `docs/` guides.
- Update `AGENTS.md` for agent workflow, commands, navigation, or safety constraints.
- Update `docs/architecture.md` when component boundaries, data flow, or invariants change.
- Update `CHANGELOG.md` for user-visible release history when the project’s release policy requires it.
- Do not create design, decision, container, or CI documentation without repository evidence or a concrete requirement.

## Definition of done

- Relevant tests, formatting, Clippy, and builds pass when available.
- Release binary build (`./scripts/build-release.sh` or `cargo build --release`) is completed prior to committing version bumps.
- New behavior has focused tests when practical.
- Documentation describes the repository after the change and contains no placeholders or machine-local links.
- No unrelated files, dependencies, generated artifacts, or claims were added.

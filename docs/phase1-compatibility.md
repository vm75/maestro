# Phase 1 compatibility inventory and M1 acceptance

Reference: `main` at `5e2013f031c965cf0381f868c541d60bb5eac602`
(`router-hub` 0.10.4). Implementation branch:
`refactor/modular-architecture`.

## Contract / rollback inventory

- **Binary and package:** `router-hub` executable, Cargo/IPK version 0.10.4,
  existing Entware init name `S99router-hub`, release lookups and existing
  updater workflows unchanged. No automatic release/tag is issued by this work.
- **CLI:** `serve`, `check-config`, `init-nginx`,
  `migrate-nginx-inputs`, `render-ui`, `mount-ui`, `unmount-ui`,
  `backup`, `restore`, `upgrade`, and `upgrade-nginx` retain names
  and options. `upgrade` and `upgrade-nginx` remain live-ASUS-only.
  `--linux-mode` and `--test-mode` remain mutually exclusive.
- **Configuration and storage:** root `platform = "asuswrt"` default,
  existing `router-hub.toml`, stored JSON, nginx inputs `sites.toml`,
  certificate/dehydrated paths, and original migration/rollback code remain
  authoritative. No schema conversion or new persistent files.
- **Public HTTP:** `/`, health, version and image assets retain paths.
  Protected routes for `/api/runtime`, `/api/dashboard`,
  `/api/system/usage`, `/api/services*`, `/api/nginx*`,
  `/api/certificates*`, `/api/wol*`, `/api/firewall*`,
  `/api/adguard*`, and `/api/wireguard*` stay under their existing
  middleware and request/response contracts. Added only:
  `GET /api/modules` (bearer protected; returns platform, feature
  availability, dependencies and non-secret UI contributions).
- **ASUS integration:** same rendered ASP path, firmware template injection,
  token treatment, menu tree entry, standalone UI, and embedded offline asset
  policy. Global JavaScript action functions, view DOM IDs, `data-view`
  selectors, forms/modals and original `?tab=<feature>` deep links
  remain intact. Group tabs consolidate Web and Network features.
- **Linux:** existing init-script compatibility retained. This phase
  does **not** introduce systemd, Svelte, Docker, Podman, Incus or libvirt.
- **Rollback:** do not overwrite state/config to roll back. Stop service,
  restore previously packaged v0.10.4 binary/IPK (and, if needed, an
  operator-verified pre-upgrade backup), restart using existing Entware
  init. There is no Phase 1 data migration to reverse. Preserve a copy
  of the installed executable and configuration before any router test.

## Validation matrix — not yet signed off

| Evidence | Command or procedure | Status |
| --- | --- | --- |
| Rust formatting | `cargo +1.85.0 fmt --all -- --check` | Pending |
| Rust tests | `cargo +1.85.0 test --locked` | Pending |
| Rust Clippy | `cargo +1.85.0 clippy --all-targets --locked -- -D warnings` | Pending |
| Release build | `cargo +1.85.0 build --release --locked` | Pending |
| Offline UI composition | `python3 scripts/build-web-ui.py --check` | Source round-trip checked; command pending |
| JS syntax and feature inventory | `node scripts/test-web-ui.mjs` | JavaScript syntax checked; full command pending |
| Browser / ASUS ASP | `node scripts/test-nginx-ui.mjs` with lab binary + Chromium; inspect generated ASP and offline requests | Pending |
| NGINX/Jinja regressions | `make test-nginx` | Pending |
| Linux non-privileged smoke | `make test-linux` | Pending |
| IPK package layout | `make test-ipk`; inspect offline assets | Pending |
| Explicit manual GitHub CI | `workflow_dispatch` on this branch only after Steps 1–3 | Pending |
| Matching ASUS hardware | Menu, services, NGINX, HTTP-01/DNS-01, Ban Shield, DNSmasq/AdGuard, WireGuard, WOL and restart/recovery | Pending |
| Router footprint | Compare ARM binary size, startup, idle RSS/CPU to baseline | Pending |
| Operator sign-off | Feature parity and rollback evidence | Pending |

No CI has been dispatched as part of the implementation commits. Existing
`ci.yml` auto-runs only on `main` and `ci/**`; keep this branch name and
do not merge/tag until the M1 acceptance gate succeeds.

## Feature parity risks to inspect on hardware

Service stop/restart must not kill the controlling API before detach; NGINX
test/reload must retain rollback; HTTP-01 temporary access must close after
failure; Ban Shield rule changes must not accidentally enforce in observe-only
mode; ASUS menu rendering must work both before and after firmware asynchronous
menu injection; standalone/ASP navigations must preserve legacy deep links;
unsupported features must not become actionable by hiding only the UI.

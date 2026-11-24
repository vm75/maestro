# Development

[← Back to README](../README.md)

## Prerequisites

- Rust 1.85 or newer.
- A Unix-like environment with the standard Rust tooling.
- For debugging in VS Code, install the CodeLLDB extension (`vadimcn.vscode-lldb`):

  ```sh
  code --install-extension vadimcn.vscode-lldb
  ```

- Python 3.11 or newer (for Linux lab setup scripts and nginx template validation). You can use `uv` to set up a virtual environment and install dependencies:

  ```sh
  uv venv
  source .venv/bin/activate
  uv pip install -r requirements.txt
  ```

## Run locally

Use `make help` to list the three development environments:

| Environment | First-time setup | Start | Browser access |
| --- | --- | --- | --- |
| Simulated router | None beyond Rust | `make run-simulated` | <http://127.0.0.1:3030>, token `maestro-test-token` |
| Native Linux | `make init-linux` | `make run-linux` | <http://127.0.0.1:3030>, token from `data/linux/maestro.yaml` |
| QEMU router | `make init-router` | `make run-router` | <http://127.0.0.1:8080/user20.asp>; see the [guest guide](../lab/aarch64/README.md) for credentials |

Run one environment at a time: they share API port 3030, and Linux nginx uses
the QEMU HTTP/HTTPS forwarding ports. Stop the foreground server with Ctrl+C;
exit QEMU with Ctrl+A then X. Configuration and state are separate for each
environment.

Simulated router mode uses fixtures, simulates external commands, skips ASUS
menu changes, and does not send Wake-on-LAN packets. `make run` remains an alias.

Native Linux mode executes real operations as a non-root account. On Arch and
Debian 12+ (including compatible derivatives), `make init-linux` installs system
dependencies with `sudo` and prepares local configuration and frontend
dependencies. Arch uses the existing pacman database; update the system normally
before setup if it is out of date. Package managers may start the distribution's
nginx service; Maestro's development nginx uses separate configuration and ports.

When installed, mise manages Rust and Node; uv manages Python 3.11 and its
dependencies in `.dev-tools/python`. Otherwise setup reuses compatible Rust or
installs stable with rustup, uses distro Python/Jinja2/PyYAML, and reuses compatible
Node/npm or downloads an official Node 22 archive with SHA-256 verification into
`.dev-tools/node`. Toolchains and npm run as your account. Global tool defaults
and existing Maestro configuration are preserved. Both init and run select the
same tools without requiring shell activation. Each run rebuilds the Svelte UI
before compiling and starting Maestro. See the [Linux integration guide](linux.md).

QEMU router mode runs the real AArch64 router binary with Entware and mocked
ASUS firmware interfaces in an Alpine guest. Setup builds the release IPK,
downloads guest media and runs the guest installer; it requires the AArch64
cross-compilation toolchain and [QEMU prerequisites](../lab/aarch64/README.md).
After installation powers off, use `make run-router` for subsequent boots.
This environment exercises packaging, service startup, ASUS UI integration and
kernel WireGuard; it does not establish actual Asuswrt firmware compatibility.
`make init-lab` and `make run-lab` remain available; the former requires an
already-built IPK. `make test-router` checks guest scripts using mocked QEMU
without booting a VM (`make test-lab` is an alias).

## Sync router data

Copy `.env.example` to `.env` and set `SYNC_DATA_HOST` to your SSH host or
alias. Set `SYNC_DATA_ROUTER_HOME` to the remote user's home directory if it
differs from `/opt/home/admin`; this also sets the certificate mirror path.
The script sources `.env` as trusted Bash assignments. `.env` and its local
variants are ignored by Git.

```sh
./scripts/sync-data.sh -t get nginx
./scripts/sync-data.sh -s user@router-host get nginx
```

`-s` overrides the configured host. `get` and `put` require a host; help and
`zip` work without one. Mirrored files and archives stay in ignored `data/` and
can contain credentials, certificates, and private logs.

## Checks

```sh
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Shortcuts are available through `make check`, `make test`, `make fmt`,
`make clippy`, `make run`, and `make release`.

Run `make ipk` to build the AArch64 musl release binary and package the Entware
IPK using the version in `Cargo.toml`. The package and its SHA-256 checksum are
written to `dist/maestro_<version>-1_aarch64-3.10.ipk` and `.ipk.sha256`.
This uses the same cross-compilation toolchain as `make release`.

With the Python environment active, run `make test-nginx` for the bundled
nginx policy and loopback tests. `make test-linux` exercises the API with real
nginx, including managed-site persistence, regeneration, and HTTPS.

`python3 -m unittest discover -s tests -p 'test_linux_dev.py' -v` checks Arch/Debian
bootstrap selection, tool manager preference, version gates and download checksum
handling with mocked commands. It installs no packages and changes no live state.

`make test-quadlet` runs the opt-in native Quadlet generator test against rendered
Maestro definitions. It generates into a temporary directory and starts no units.
Set `MAESTRO_QUADLET_GENERATOR` when the generator is installed somewhere other
than `/usr/lib/podman/quadlet`.

`make test-podman` runs an offline rootless engine smoke test. It requires Rust/Cargo, Podman,
a C compiler with static libc, tar, sha256sum and GNU timeout, and must run as a
non-root workload account on a host permitting rootless containers. The script
builds its own scratch image, disables remote connections and networking, and
uses `--pull never`. Storage, runtime, auth and user configuration are scoped to
a fresh private temporary lab. It checks exact storage roots before creating
containers and before resetting that lab during cleanup. Existing user units and
normal Podman storage are preserved. Engine commands have bounded deadlines.

The Podman smoke checks create/start/graceful stop/restart/removal and fingerprint
labels. It also executes Maestro's real bounded metadata reader against a
running owned container with a deliberately stale fingerprint, then verifies
refusal to adopt a stopped foreign object using the configured name. The reader
checks leave that foreign object unchanged before the lab removes it. These
checks establish native engine, metadata-reader and generated-definition behavior
on the executing host. Maestro native ownership, runtime-versus-boot reconciliation,
interrupted operations and the two installer/helper modes require further
provider acceptance in disposable guests.

The emulated nginx examples are managed inputs in
`test-fixtures/etc/maestro/nginx/sites.yaml`. Test mode still writes fixture
files when you save settings; the ASUS page is generated at startup and ignored
by Git.

## Build cache size

Development and test builds use limited debug information (`debug = 1`) to
reduce `target/` disk usage. Backtraces retain file and line information, but
debugger type and variable inspection is limited. Incremental compilation stays
enabled for faster rebuilds after source changes.

For a debugging session that needs full debug information, run
`CARGO_PROFILE_DEV_DEBUG=2 cargo build`.

Cargo can retain artifacts from old compiler, dependency, and feature settings.
With no builds running, remove `target/debug/incremental/` to clear just the
development and test incremental cache. Run `cargo clean --profile dev` to
remove development and test artifacts, or `cargo clean` to remove all build
artifacts. The next build recreates what it needs.

## Ingress and offline UI regression checks

`cargo test --test nginx_ingress_test` exercises managed ingress, temporary
HTTP-01 sessions, concurrency, failure recovery and real loopback nginx. The
certificate WAN unit tests use `test-fixtures/bin/netfilter-mock.py`; they never
execute host firewall tools. These tests do not certify firmware behavior.

For the optional browser workflow check, install Node 22+ and Chromium, build
with `cargo build`, then run:

```sh
node scripts/test-nginx-ui.mjs target/debug/maestro /usr/sbin/chromium
```

The script starts a temporary test-mode server and checks normal form controls,
source previews, advanced-route preservation, draft retention, narrow layout,
keyboard access and offline failures. It uses built-in Node APIs and Chromium's
debugging protocol without npm packages. Pass `--keep` to retain the lab and its
mobile screenshot. Browser and live-nginx checks require loopback socket access.

## Certificate staging tests

To exercise the dehydrated command and DNS hook against Let's Encrypt staging,
use the temporary test harness. It uses the same `--cron --accept-terms
--config` invocation as Maestro and leaves router certificate data untouched:

```sh
DEHYDRATED=/path/to/dehydrated \
  DNS_PROVIDER=duckdns DUCKDNS_TOKEN=... \
  ./scripts/test-dehydrated.sh '*.example.duckdns.org'
```

Use repeated `--env KEY=VALUE` options when you want the hook settings written
into the temporary dehydrated config, as they are by Maestro. The harness
uses the staging CA by default and supports `--method http`, `--hook`,
`--force`, and `--keep`.

## API overview

The API is primarily consumed by the bundled UI. Unauthenticated endpoints are
`GET /healthz` and `GET /api/version`; the standalone page is token-gated.
Management endpoints require the bearer token and are grouped as follows:

- `/api/services` — Entware service discovery, actions, and logs;
- `/api/nginx` — nginx status, lifecycle, global settings, site objects, and logs;
- `/api/certificates` — certificate definitions, issue/renew operations,
  dehydrated lock handling, and script update;
- `/api/wol` — machine management, wake, and status;
- `/api/firewall` — policy, rules, allowlists, bans, counters, and status;
- `/api/adguard` — AdGuard configuration, protection, staged rewrites, and
  `hosts.add` management (saving hosts restarts dnsmasq);
- `/api/dashboard`, `/api/runtime`, and `/api/auth/check` — authenticated
  status views.

The authoritative route map is [`src/api/mod.rs`](../src/api/mod.rs).

See [architecture](architecture.md) for boundaries and invariants, and
[CI and releases](releases.md) for publishing.

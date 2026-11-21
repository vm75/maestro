# Nginx AArch64 Cross-Builder

A standalone host-side tool for building **Nginx and matching dynamic modules** for the ASUS RT-AX88U (Asuswrt-Merlin) using Entware's `aarch64-k3.10` toolchain. It is kept alongside router-hub for version control, but **does not depend on router-hub, modify its configuration, or install anything on the router**.

## Repository layout

```text
maestro/
└── tools/
    └── nginx-builder/
        ├── build-nginx-entware-aarch64-reverse-proxy.sh
        └── README.md
```

Run `tools/nginx-builder/build-nginx-entware-aarch64-reverse-proxy.sh` from any directory; its build cache and default output stay **outside the repository**.

## Requirements

- **Host:** x86_64 Debian, Ubuntu, or Arch Linux; run as a regular user with `sudo` access.
- Internet access and approximately **10+ GiB** of free disk space.
- Missing build dependencies are installed via `apt` or `pacman` unless `--no-install-deps` is used.

## Build

From the Maestro repository root:

```bash
chmod +x tools/nginx-builder/build-nginx-entware-aarch64-reverse-proxy.sh

# Latest stable release (default; detected from nginx.org)
tools/nginx-builder/build-nginx-entware-aarch64-reverse-proxy.sh

# Latest mainline release
tools/nginx-builder/build-nginx-entware-aarch64-reverse-proxy.sh --mainline

# Specific upstream release
tools/nginx-builder/build-nginx-entware-aarch64-reverse-proxy.sh --version 1.28.0
```

Other options: `--channel stable|mainline`, `--jobs N`, `--workdir DIR`, `--output DIR`, `--no-update`, `--no-install-deps`, `--toolchain-only`, `--help`.

The persistent cache defaults to `~/.cache/nginx-entware-aarch64/` (or `$XDG_CACHE_HOME/nginx-entware-aarch64/`). Subsequent builds reuse the toolchain and downloaded sources.

## Build profile

- **Included:** HTTPS/TLS, HTTP/2, HTTP/3/QUIC, HTTP proxy, WebSockets/SSE, gRPC, FastCGI, uwsgi, SCGI, Memcached, TCP/UDP stream, real-IP handling, rewrites, caching, gzip, rate/connection limits, and authentication. Dynamic modules: **Brotli**, **stream**, **headers-more**.
- **Excluded:** native WebDAV, FLV, SSI, autoindex, tracking/legacy modules, njs, GeoIP2, NAXSI, Lua, RTMP, and MPEG-TS.
- **Hardening:** PIE, strong stack protection, `_FORTIFY_SOURCE=2`, full RELRO, and format-security checks. Targets Entware AArch64 / glibc 2.27. HTTP/3 and stream listeners remain inactive until configured.

Toolchain bootstrap is isolated from userland package hardening: Entware's stock configuration is preserved during toolchain compilation so glibc 2.27 bootstraps cleanly without stack-protector leaking into dynamic loader internals, while NGINX and its modules compile with full security hardening. Pinned default commits (`ENTWARE_REV` and `ENTWARE_PACKAGES_REV`) ensure builds remain reproducible across fresh checkouts.

## Manual GitHub Actions build

Once the [Build Nginx workflow](../../.github/workflows/build-nginx.yml) is
present on the repository default branch, select **Actions → Build Nginx →
Run workflow** and choose the latest stable version (default), mainline, or an
explicit version. Only manual runs are supported; there is no push, tag,
schedule, or PR trigger.

Each successful run uploads a separate downloadable Actions artifact with the
same output described below. By default it also publishes a GitHub Release
under a unique `nginx-v*` tag, marked **not Latest** so it cannot replace the
latest Maestro release. Uncheck `publish_release` for an artifact-only
trial.

### Pre-built Toolchain Container (GHCR)

Building the Entware toolchain from source takes ~1–1.5 hours. To speed this up, a pre-built container image (`ghcr.io/<repository>/entware-aarch64-builder:latest`) can be built once using the **Build Toolchain Image** workflow ([.github/workflows/build-toolchain-image.yml](../../.github/workflows/build-toolchain-image.yml)).

When present on GHCR, the NGINX builder automatically pulls the container in ~30 seconds and compiles NGINX and its modules in **~2–3 minutes**. If the image is not found, the workflow gracefully falls back to compiling the toolchain on the host using all available runner cores.

## Output and deployment

Default output: `~/.cache/nginx-entware-aarch64/out/nginx-<version>-aarch64-k3.10/`

Contains the `nginx` executable; matching `nginx-ssl`, `nginx-mod-brotli`, `nginx-mod-stream`, and `nginx-mod-headers-more` `.ipk` packages; their exported `.so` modules; `SHA256SUMS`; and `build-info.txt`.

**Deployment is manual.** Back up the router's current Nginx, modules, packages, and configuration. Install Nginx and its matching module packages together, verify runtime library compatibility, and test before reloading:

```sh
/opt/sbin/nginx -V
/opt/sbin/nginx -t
```

Retain previous packages for rollback. New upstream releases may require Entware recipe or third-party module fixes; a successful build alone does not guarantee runtime compatibility.

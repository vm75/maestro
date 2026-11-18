# Nginx AArch64 Cross-Builder

A standalone host-side tool for building **Nginx and matching dynamic modules** for the ASUS RT-AX88U (Asuswrt-Merlin) using Entware's `aarch64-k3.10` toolchain. It is kept alongside router-hub for version control, but **does not depend on router-hub, modify its configuration, or install anything on the router**.

## Repository layout

```text
router-hub/
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

From the router-hub repository root:

```bash
chmod +x tools/nginx-builder/build-nginx-entware-aarch64-reverse-proxy.sh

# Latest stable release (default; detected from nginx.org)
tools/nginx-builder/build-nginx-entware-aarch64-reverse-proxy.sh

# Latest mainline release
tools/nginx-builder/build-nginx-entware-aarch64-reverse-proxy.sh --mainline

# Specific upstream release
tools/nginx-builder/build-nginx-entware-aarch64-reverse-proxy.sh --version 1.28.0
```

Other options: `--channel stable|mainline`, `--jobs N`, `--workdir DIR`, `--output DIR`, `--no-update`, `--no-install-deps`, `--help`.

The persistent cache defaults to `~/.cache/nginx-entware-aarch64/` (or `$XDG_CACHE_HOME/nginx-entware-aarch64/`). Subsequent builds reuse the toolchain and downloaded sources.

## Build profile

- **Included:** HTTPS/TLS, HTTP/2, HTTP/3/QUIC, HTTP proxy, WebSockets/SSE, gRPC, FastCGI, uwsgi, SCGI, Memcached, TCP/UDP stream, real-IP handling, rewrites, caching, gzip, rate/connection limits, and authentication. Dynamic modules: **Brotli**, **stream**, **headers-more**.
- **Excluded:** native WebDAV, FLV, SSI, autoindex, tracking/legacy modules, njs, GeoIP2, NAXSI, Lua, RTMP, and MPEG-TS.
- **Hardening:** PIE, strong stack protection, `_FORTIFY_SOURCE=2`, full RELRO, and format-security checks. Targets Entware AArch64 / glibc 2.27. HTTP/3 and stream listeners remain inactive until configured.

## Output and deployment

Default output: `~/.cache/nginx-entware-aarch64/out/nginx-<version>-aarch64-k3.10/`

Contains the `nginx` executable; matching `nginx-ssl`, `nginx-mod-brotli`, `nginx-mod-stream`, and `nginx-mod-headers-more` `.ipk` packages; their exported `.so` modules; `SHA256SUMS`; and `build-info.txt`.

**Deployment is manual.** Back up the router's current Nginx, modules, packages, and configuration. Install Nginx and its matching module packages together, verify runtime library compatibility, and test before reloading:

```sh
/opt/sbin/nginx -V
/opt/sbin/nginx -t
```

Retain previous packages for rollback. New upstream releases may require Entware recipe or third-party module fixes; a successful build alone does not guarantee runtime compatibility.

#!/usr/bin/env bash
# Bootstrap and run the native Linux development environment as its owner.
set -euo pipefail

repo_root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
tools_dir="$repo_root/.dev-tools"

fail() { printf '%s\n' "$*" >&2; exit 1; }

install_system_dependencies() {
    local ID='' ID_LIKE=''
    # /etc/os-release is owned by the distribution, not project configuration.
    source "${1:-/etc/os-release}"
    local packages=(ca-certificates curl nginx openssl)
    case " $ID $ID_LIKE " in
        *' arch '*)
            packages+=(base-devel pkgconf iproute2 iputils xz)
            if ! command -v uv >/dev/null; then
                packages+=(python python-jinja python-yaml)
            fi
            if ! command -v mise >/dev/null; then packages+=(nodejs npm); fi
            # Use the existing database; never refresh it into a partial upgrade.
            sudo pacman -S --needed "${packages[@]}"
            ;;
        *' debian '*|*' ubuntu '*)
            packages+=(build-essential pkg-config libssl-dev iproute2 iputils-ping xz-utils)
            if ! command -v uv >/dev/null; then
                packages+=(python3 python3-jinja2 python3-yaml)
            fi
            if ! command -v mise >/dev/null; then packages+=(nodejs npm); fi
            sudo apt-get update
            sudo apt-get install --no-install-recommends "${packages[@]}"
            ;;
        *) fail "Automatic dependencies support Arch and Debian families; found $ID." ;;
    esac
}

rust_command() {
    if command -v mise >/dev/null; then
        mise exec rust@stable -- "$@"
    else
        "$@"
    fi
}

node_command() {
    if command -v mise >/dev/null; then
        mise exec node@22 -- "$@"
    else
        "$@"
    fi
}

check_rust() {
    local version major minor
    version=$(rust_command rustc --version) || return 1
    read -r _ version _ <<< "$version"
    IFS=. read -r major minor _ <<< "$version"
    (( major > 1 || (major == 1 && minor >= 85) )) && rust_command cargo --version >/dev/null
}

check_node() {
    node_command node -e '
        const [major, minor] = process.versions.node.split(".").map(Number);
        process.exit((major === 20 && minor >= 19) || (major === 22 && minor >= 12) || major > 22 ? 0 : 1);
    ' && node_command npm --version >/dev/null
}

install_node_fallback() (
    # Debian 12 ships Node 18. Keep a compatible official binary local to this checkout.
    local arch stage archive
    case "$(uname -m)" in
        x86_64) arch=x64 ;;
        aarch64) arch=arm64 ;;
        *) fail 'Install compatible Node.js yourself, or install mise and rerun make init-linux.' ;;
    esac
    stage=$(mktemp -d)
    trap 'rm -rf -- "$stage"' EXIT
    curl --proto '=https' --tlsv1.2 -fsSL https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt -o "$stage/SHASUMS256.txt"
    archive=$(awk -v suffix="-linux-$arch.tar.xz" '$2 ~ /^node-v22\./ && substr($2, length($2)-length(suffix)+1) == suffix {print $2}' "$stage/SHASUMS256.txt")
    [[ -n "$archive" && "$archive" != *$'\n'* ]] || fail 'Cannot identify the official Node 22 archive.'
    curl --proto '=https' --tlsv1.2 -fsSL "https://nodejs.org/dist/latest-v22.x/$archive" -o "$stage/$archive"
    (cd "$stage" && sha256sum --check --ignore-missing SHASUMS256.txt)
    mkdir -p "$tools_dir/node"
    tar -xJf "$stage/$archive" --strip-components=1 -C "$tools_dir/node"
)

install_toolchains() {
    if command -v mise >/dev/null; then
        mise install rust@stable node@22
    else
        if ! check_rust; then
            if ! command -v rustup >/dev/null; then
                local installer
                installer=$(mktemp)
                if ! curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o "$installer"; then
                    rm -f -- "$installer"
                    fail 'Could not download the official rustup installer.'
                fi
                if ! sh "$installer" -y --profile minimal --no-modify-path --default-toolchain none; then
                    rm -f -- "$installer"
                    fail 'rustup installation failed.'
                fi
                rm -f -- "$installer"
            fi
            rustup toolchain install stable --profile minimal
            # Select stable for this invocation only; preserve the user's global default.
            export RUSTUP_TOOLCHAIN=stable
        fi
        if ! check_node; then install_node_fallback; fi
    fi
    check_rust || fail 'Rust 1.85+ and Cargo are required.'
    check_node || fail 'Node.js 20.19+, 22.12+, or a newer major version and npm are required.'
    if command -v uv >/dev/null; then
        if [[ ! -x "$tools_dir/python/bin/python3" ]]; then
            uv venv --python 3.11 "$tools_dir/python"
        fi
        uv pip install --python "$tools_dir/python/bin/python3" -r "$repo_root/requirements.txt"
    fi
    hash -r
    python3 -c 'import sys; assert sys.version_info >= (3, 11), "Python 3.11+ required"; import jinja2, yaml'
}

init_helper() {
    if ! command -v mise >/dev/null && ! check_rust && command -v rustup >/dev/null; then
        export RUSTUP_TOOLCHAIN=stable
    fi
    check_rust || fail 'Run make init-linux to install Rust 1.85+.'
    # Keep the locally built artifact at a known path even with CARGO_TARGET_DIR.
    local target="$repo_root/target"
    rust_command cargo build --locked --no-default-features --features linux --bin maestro-admin --target-dir "$target"
    sudo -- /bin/bash "$repo_root/scripts/linux-helper.sh" install "$target/debug/maestro-admin"
    if ! sudo -n -l -u root -- /usr/local/libexec/maestro-admin; then
        fail 'Helper installed but exact sudo authorization is not available. Check sudoers includes.'
    fi
    printf 'For protected host operations, enable server.privileged_operations in data/linux/maestro.yaml and review the root-owned helper grants.\n'
    printf 'Restart with make run-linux and check Agent access > Capabilities.\n'
}

main() {
    [[ $# == 1 && ( $1 == init || $1 == run || $1 == init-helper || $1 == remove-helper ) ]] || fail 'Usage: scripts/linux-dev.sh init|run|init-helper|remove-helper'
    [[ $(id -u) != 0 ]] || fail 'Run Linux development targets as your non-root development account; privileged installation uses sudo.'
    # Shared by initialization and runtime, including the Python nginx renderer.
    export PATH="$tools_dir/python/bin:$tools_dir/node/bin:${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
    cd "$repo_root"
    case "$1" in
        init-helper)
            init_helper
            ;;
        remove-helper)
            sudo -- /bin/bash "$repo_root/scripts/linux-helper.sh" remove
            ;;
        init)
            install_system_dependencies
            install_toolchains
            rust_command cargo --version
            # setup-linux.sh only needs Cargo on PATH for its prerequisite check.
            if command -v mise >/dev/null; then
                mise exec rust@stable -- bash "$repo_root/scripts/setup-linux.sh"
            else
                bash "$repo_root/scripts/setup-linux.sh"
            fi
            node_command npm --prefix "$repo_root/linux-web" ci
            printf 'Ready. Start with make run-linux.\n'
            ;;
        run)
            # A pre-existing rustup installation may have an older global default.
            if ! command -v mise >/dev/null && ! check_rust && command -v rustup >/dev/null; then
                export RUSTUP_TOOLCHAIN=stable
            fi
            check_rust || fail 'Run make init-linux to install Rust 1.85+.'
            check_node || fail 'Run make init-linux to install a compatible Node.js and npm.'
            node_command npm --prefix "$repo_root/linux-web" run build
            rust_command cargo run --bin maestro --no-default-features --features linux-ui -- --config "$repo_root/data/linux/maestro.yaml" --linux-mode serve
            ;;
    esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then main "$@"; fi

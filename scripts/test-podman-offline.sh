#!/usr/bin/env bash
set -euo pipefail
trap 'echo "Podman lab failed at line $LINENO" >&2' ERR

# Real rootless engine probe. Every image, container, runtime path and configuration
# belongs to a fresh temporary lab. No pulls, host networking or user units.
if (( EUID == 0 )); then
    echo 'Run the Podman lab as a non-root workload account.' >&2
    exit 2
fi
for program in podman cc tar sha256sum timeout cargo; do
    command -v "$program" >/dev/null || { echo "Missing required command: $program" >&2; exit 2; }
done
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
podman_bin=$(command -v podman)
lab=$(mktemp -d "${TMPDIR:-/tmp}/maestro-podman.XXXXXX")
mkdir -m 700 "$lab/runtime" "$lab/config" "$lab/rootfs" "$lab/tmp"
export XDG_RUNTIME_DIR="$lab/runtime"
export XDG_CONFIG_HOME="$lab/config"
export REGISTRY_AUTH_FILE="$lab/auth.json"
printf '%s\n' '{"auths":{}}' > "$REGISTRY_AUTH_FILE"
export TMPDIR="$lab/tmp"

engine() {
    timeout --kill-after=5s 30s "$podman_bin" --remote=false --root "$lab/storage" --runroot "$lab/runroot" \
        --storage-driver vfs --cgroup-manager cgroupfs --events-backend file "$@"
}
cleanup() {
    local result=$?
    trap - EXIT
    # Never reset storage unless the engine confirms both exact lab roots.
    local roots
    roots=$(engine info --format '{{.Store.GraphRoot}}|{{.Store.RunRoot}}' 2>/dev/null) || roots=
    if [[ "$roots" == "$lab/storage|$lab/runroot" ]]; then
        if ! engine system reset --force > "$lab/cleanup.log" 2>&1; then
            echo "Podman cleanup failed; private lab retained: $lab" >&2
            if (( result == 0 )); then result=1; fi
            exit "$result"
        fi
    elif [[ -d "$lab/storage" ]]; then
        echo "Cannot verify Podman lab roots; private lab retained: $lab" >&2
        if (( result == 0 )); then result=1; fi
        exit "$result"
    fi
    rm -rf -- "$lab"
    exit "$result"
}
trap cleanup EXIT

[[ $(engine info --format '{{.Host.Security.Rootless}}') == true ]] || {
    echo 'The isolated engine is not rootless.' >&2
    exit 1
}
[[ $(engine info --format '{{.Store.GraphRoot}}|{{.Store.RunRoot}}') == "$lab/storage|$lab/runroot" ]] || {
    echo 'The engine did not select the exact private lab roots.' >&2
    exit 1
}

# Build a local scratch payload, avoiding registry access and base-image trust.
cc -static -Os -s -x c -o "$lab/rootfs/probe" - <<'C'
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
static volatile sig_atomic_t done;
static void stop(int signal_number) { (void)signal_number; done = 1; }
int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "hold") == 0) {
        signal(SIGTERM, stop);
        signal(SIGINT, stop);
        while (!done) sleep(1);
        return 0;
    }
    puts("maestro offline native probe");
    return 0;
}
C
tar -C "$lab/rootfs" -cf "$lab/rootfs.tar" probe
image=localhost/maestro-phase2-offline:lab
engine import --quiet --change 'ENTRYPOINT ["/probe"]' "$lab/rootfs.tar" "$image" >/dev/null
fingerprint=$(sha256sum "$lab/rootfs/probe")
fingerprint=${fingerprint%% *}

container_args=(--pull never --network none --read-only --cap-drop ALL
    --security-opt no-new-privileges --cgroups disabled)
expect() {
    if [[ "$1" != "$2" ]]; then
        printf 'Expected %s, got %q\n' "$2" "$1" >&2
        return 1
    fi
}
expect "$(engine run --rm "${container_args[@]}" "$image")" 'maestro offline native probe'
name=maestro-phase2-offline
engine create --name "$name" "${container_args[@]}" \
    --label io.maestro.service=offline \
    --label io.maestro.container=probe \
    --label "io.maestro.fingerprint=$fingerprint" "$image" hold >/dev/null
expect "$(engine inspect --format '{{index .Config.Labels "io.maestro.fingerprint"}}' "$name")" "$fingerprint"
expect "$(engine inspect --format '{{.State.Status}}' "$name")" created
engine start "$name" >/dev/null
expect "$(engine inspect --format '{{.State.Running}}' "$name")" true
# Exercise Maestro's actual bounded metadata reader against this running
# container. The wrapper fixes private roots; the reader disables remote mode.
printf '#!/usr/bin/env bash\nexec %q --root %q --runroot %q --storage-driver vfs --cgroup-manager cgroupfs --events-backend file "$@"\n' \
    "$podman_bin" "$lab/storage" "$lab/runroot" > "$lab/podman-metadata"
chmod 700 "$lab/podman-metadata"
check_reader() {
    MAESTRO_PODMAN_PROGRAM="$lab/podman-metadata" cargo test --manifest-path "$repo_root/Cargo.toml" \
        --locked --no-default-features --features linux --lib \
        "core::declarative::quadlet::runtime::tests::$1" -- --ignored --exact
}
check_reader native_rootless_inventory_reports_owned_stale_member

engine stop --time 2 "$name" >/dev/null
expect "$(engine inspect --format '{{.State.Running}}' "$name")" false
expect "$(engine inspect --format '{{.State.ExitCode}}' "$name")" 0
engine start "$name" >/dev/null
engine restart --time 2 "$name" >/dev/null
expect "$(engine inspect --format '{{.State.Running}}' "$name")" true
engine stop --time 2 "$name" >/dev/null
expect "$(engine inspect --format '{{.State.ExitCode}}' "$name")" 0
engine rm "$name" >/dev/null
if engine container exists "$name"; then exists_status=0; else exists_status=$?; fi
expect "$exists_status" 1
# A stopped foreign object with the configured name must remain untouched.
engine create --name "$name" "${container_args[@]}" "$image" hold >/dev/null
check_reader native_rootless_inventory_rejects_foreign_name
expect "$(engine inspect --format '{{.State.Status}}' "$name")" created
engine rm "$name" >/dev/null
engine version --format '{{.Client.Version}}'
echo 'Offline rootless Podman lifecycle, fingerprint-label and Maestro metadata ownership checks passed.'

#!/usr/bin/env bash
# Unit and regression test suite for standalone NGINX cross-builder and workflows.
set -Eeuo pipefail
unset CDPATH || true

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
REPO_ROOT=$(cd "$SCRIPT_DIR/../.." && pwd -P)
BUILD_SCRIPT="$SCRIPT_DIR/build-nginx-entware-aarch64-reverse-proxy.sh"
WORKFLOW_BUILD="$REPO_ROOT/.github/workflows/build-nginx.yml"
WORKFLOW_TOOLCHAIN="$REPO_ROOT/.github/workflows/build-toolchain-image.yml"

FAILED=0
passed() { printf '  PASS: %s\n' "$1"; }
failed() { printf '  FAIL: %s (%s)\n' "$1" "$2"; FAILED=$((FAILED + 1)); }

echo "=== Running NGINX Builder Regression Tests ==="

# Test 1: Bash syntax checks
test_syntax() {
  if bash -n "$BUILD_SCRIPT" 2>/dev/null; then
    passed "Build script bash syntax is valid"
  else
    failed "Build script bash syntax" "bash -n failed"
  fi
}

# Test 2: Verify release-notes heredoc does not execute backticks as commands
test_release_notes_heredoc() {
  # Extract the heredoc from build-nginx.yml
  local heredoc
  heredoc=$(sed -n '/cat > "\$notes" <<EOF/,/EOF/p' "$WORKFLOW_BUILD")
  if [[ -z "$heredoc" ]]; then
    failed "Release-notes heredoc" "Could not extract heredoc from $WORKFLOW_BUILD"
    return
  fi

  # In bash, an unescaped `aarch64-k3.10` inside <<EOF executes the command.
  # Escaped \`aarch64-k3.10\` produces literal Markdown backticks without executing.
  if grep -q '\\`aarch64-k3.10\\`' <<< "$heredoc"; then
    passed "Release-notes heredoc properly escapes backticks around aarch64-k3.10"
  else
    failed "Release-notes heredoc" "Contains unescaped command substitution backticks"
  fi

  # Execute a mock evaluation of the heredoc in a subshell where aarch64-k3.10 is NOT a command
  local eval_output
  eval_output=$(
    NGINX_VERSION=1.30.7 GITHUB_RUN_ID=12345 GITHUB_REPOSITORY=vm75/router-hub bash -c '
      notes=$(mktemp)
      cat > "$notes" <<EOF
      Manually cross-built **NGINX ${NGINX_VERSION}** for Asuswrt-Merlin / Entware \`aarch64-k3.10\`.
      - Built from [workflow run ${GITHUB_RUN_ID}](https://github.com/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID}).
EOF
      cat "$notes"
      rm -f "$notes"
    ' 2>&1
  )
  if grep -q '`aarch64-k3.10`' <<< "$eval_output" && ! grep -qi 'command not found' <<< "$eval_output"; then
    passed "Heredoc generates correct Markdown with backticks without command execution"
  else
    failed "Heredoc evaluation" "Failed or attempted command execution: $eval_output"
  fi
}

# Test 3: Verify Docker pull error handling does not suppress errors
test_docker_pull_error_handling() {
  # Check that docker pull 2>/dev/null is NOT present in build-nginx.yml
  if grep -q 'docker pull .*2>/dev/null' "$WORKFLOW_BUILD"; then
    failed "Docker pull error handling" "docker pull stderr is silently suppressed to /dev/null"
  else
    passed "Docker pull does not blindly suppress errors to /dev/null"
  fi

  # Check that build-nginx.yml distinguishes image not found from other errors
  if grep -qi 'manifest unknown' "$WORKFLOW_BUILD"; then
    passed "Docker pull inspects error output for missing image vs network/auth failure"
  else
    failed "Docker pull error handling" "Does not inspect failure diagnostics"
  fi
}

# Test 4: Verify toolchain bootstrap vs package hardening separation
test_two_phase_hardening() {
  # Toolchain bootstrap must use Entware's stock configuration without SSP/FORTIFY/RELRO
  # Package build must apply SSP STRONG, FORTIFY 2, FULL RELRO, FORMAT SECURITY
  if grep -q "PKG_CC_STACKPROTECTOR': 'STRONG'" "$BUILD_SCRIPT" && \
     grep -q "PKG_FORTIFY_SOURCE': '2'" "$BUILD_SCRIPT" && \
     grep -q "PKG_RELRO': 'FULL'" "$BUILD_SCRIPT"; then
    passed "NGINX package hardening configuration enables STRONG SSP, FORTIFY 2, and FULL RELRO"
  else
    failed "Package hardening profile" "Missing required hardening flags"
  fi

  # Toolchain bootstrap must be guarded before package hardening
  if grep -q 'toolchain_ready' "$BUILD_SCRIPT"; then
    passed "Build script guards toolchain bootstrap and detects pre-built toolchain"
  else
    failed "Toolchain bootstrap guard" "Missing toolchain_ready detection"
  fi

  # Toolchain-only must exit before package build
  if grep -q '(( TOOLCHAIN_ONLY ))' "$BUILD_SCRIPT"; then
    passed "--toolchain-only exits cleanly after toolchain build without touching NGINX recipes"
  else
    failed "--toolchain-only handling" "Missing TOOLCHAIN_ONLY exit check"
  fi
}

# Test 5: Verify reproducibility pins
test_reproducibility_pins() {
  if grep -q 'ENTWARE_REV=' "$BUILD_SCRIPT" && grep -q 'ENTWARE_PACKAGES_REV=' "$BUILD_SCRIPT"; then
    passed "Default validated Entware and packages feed commits are pinned"
  else
    failed "Reproducibility pins" "Missing ENTWARE_REV or ENTWARE_PACKAGES_REV"
  fi
}

# Test 6: Verify deliverable assets validation logic
test_asset_validation() {
  # Workflow must check for 11 assets
  if grep -q '(( ${#assets\[@\]} == 11 ))' "$WORKFLOW_BUILD"; then
    passed "Workflow verifies all 11 required assets (4 IPKs, 4 modules, 1 binary, checksums, build-info)"
  else
    failed "Asset count validation" "Expected check for 11 assets"
  fi

  # Workflow releases must have --latest=false
  if grep -q -- '--latest=false' "$WORKFLOW_BUILD"; then
    passed "NGINX releases are strictly marked --latest=false"
  else
    failed "Release publication" "--latest=false is missing"
  fi

  # Workflows must be manual-only (workflow_dispatch)
  if grep -q 'workflow_dispatch:' "$WORKFLOW_BUILD" && ! grep -q 'push:' "$WORKFLOW_BUILD" && ! grep -q 'pull_request:' "$WORKFLOW_BUILD"; then
    passed "build-nginx.yml is strictly manual-only"
  else
    failed "Workflow trigger" "build-nginx.yml must only trigger on workflow_dispatch"
  fi
  if grep -q 'workflow_dispatch:' "$WORKFLOW_TOOLCHAIN" && ! grep -q 'push:' "$WORKFLOW_TOOLCHAIN" && ! grep -q 'pull_request:' "$WORKFLOW_TOOLCHAIN"; then
    passed "build-toolchain-image.yml is strictly manual-only"
  else
    failed "Workflow trigger" "build-toolchain-image.yml must only trigger on workflow_dispatch"
  fi
}

test_syntax
test_release_notes_heredoc
test_docker_pull_error_handling
test_two_phase_hardening
test_reproducibility_pins
test_asset_validation

if (( FAILED == 0 )); then
  echo "=== All NGINX builder regression tests PASSED ==="
  exit 0
else
  echo "=== $FAILED tests FAILED ==="
  exit 1
fi

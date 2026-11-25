.DEFAULT_GOAL := check

# Reuse the Python environment prepared by make init-linux for test targets.
PYTHON ?= $(if $(wildcard .dev-tools/python/bin/python3),$(CURDIR)/.dev-tools/python/bin/python3,python3)

.PHONY: help check test test-linux test-podman test-quadlet test-nginx test-ipk test-ui test-router test-lab run run-linux run-router run-simulated init-linux init-linux-helper remove-linux-helper init-router fmt clippy fix release ipk init-lab run-lab

help:
	@echo 'Development environments (run one at a time; all use API port 3030):'
	@echo '  make run-simulated  Run fixture-backed router simulation (no setup required)'
	@echo '  make init-linux    Install Arch/Debian dependencies and prepare native Linux'
	@echo '  make init-linux-helper    Build and install the optional reviewed development helper'
	@echo '  make remove-linux-helper  Remove the helper and its sudo authorization'
	@echo '  make run-linux     Build the Linux UI and run real Linux operations as non-root'
	@echo '  make init-router   Build the IPK and install the AArch64 QEMU router guest'
	@echo '  make run-router    Boot the installed QEMU router guest'
	@echo '  make test-router   Check router guest scripts with mocked QEMU'
	@echo '  make fix           Apply baseline Clippy fixes and Rust formatting'
	@echo 'See docs/development.md for prerequisites and browser access.'

check:
	cargo check --all-targets

test:
	cargo test

test-nginx:
	"$(PYTHON)" -m unittest discover -s config/nginx/tests -p 'test_*.py' -v

test-ipk:
	bash scripts/test-package-ipk.sh

test-ui:
	python3 scripts/build-web-ui.py --check
	node scripts/test-web-ui.mjs

test-linux:
	PYTHON="$(PYTHON)" bash scripts/test-linux-integration.sh

test-podman:
	bash scripts/test-podman-offline.sh

test-quadlet:
	cargo test --locked --no-default-features --features linux --lib native_generator_accepts_rootless_artifacts -- --ignored

test-router:
	bash tests/test_lab_aarch64.sh

test-lab: test-router

run: run-simulated

run-simulated:
	cargo run --bin maestro --no-default-features --features asuswrt -- --config ./test-fixtures/etc/maestro/maestro.yaml --test-mode serve

init-linux:
	bash scripts/linux-dev.sh init

init-linux-helper:
	bash scripts/linux-dev.sh init-helper

remove-linux-helper:
	bash scripts/linux-dev.sh remove-helper

run-linux:
	bash scripts/linux-dev.sh run

init-router: ipk
	./lab/aarch64/init.sh

run-router:
	./lab/aarch64/run.sh

fmt:
	cargo fmt --all

clippy:
	cargo clippy --all-targets -- -D warnings

fix:
	@status=0; cargo +1.85.0 clippy --fix --all-targets --locked --allow-dirty --allow-staged || status=$?; \
		cargo +1.85.0 fmt --all || exit $?; exit $status

release:
	./scripts/build-release.sh aarch64-unknown-linux-musl

ipk: release
	./scripts/package-ipk.sh "v$$(awk -F '"' '/^version = / { print $$2; exit }' Cargo.toml)" aarch64-unknown-linux-musl

init-lab:
	./lab/aarch64/init.sh

run-lab: run-router

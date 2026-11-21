.PHONY: check test test-linux test-nginx test-ipk test-ui test-lab run fmt clippy release ipk init-lab run-lab
check:
	cargo check --all-targets

test:
	cargo test

test-nginx:
	python3 -m unittest discover -s config/nginx/tests -p 'test_*.py' -v

test-ipk:
	bash scripts/test-package-ipk.sh

test-ui:
	python3 scripts/build-web-ui.py --check
	node scripts/test-web-ui.mjs

test-linux:
	bash scripts/test-linux-integration.sh

test-lab:
	bash tests/test_lab_aarch64.sh

run:
	cargo run -- --config ./test-fixtures/etc/maestro/maestro.toml --test-mode serve

fmt:
	cargo fmt --all

clippy:
	cargo clippy --all-targets -- -D warnings

release:
	./scripts/build-release.sh aarch64-unknown-linux-musl

ipk: release
	./scripts/package-ipk.sh "v$$(awk -F '"' '/^version = / { print $$2; exit }' Cargo.toml)" aarch64-unknown-linux-musl

init-lab:
	./lab/aarch64/init.sh

run-lab:
	./lab/aarch64/run.sh

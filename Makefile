.PHONY: all fmt-check clippy test quality release-artifacts

all: quality

fmt-check:
	cargo fmt --check

clippy:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test -- --test-threads=1

quality: fmt-check clippy test

release-artifacts:
	./scripts/release-artifacts.sh

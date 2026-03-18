.PHONY: all fmt-check clippy test release-verify quality release-artifacts

all: quality

fmt-check:
	cargo fmt --check

clippy:
	cargo clippy --all-targets --all-features -- -D warnings

test:
	cargo test -- --test-threads=1

release-verify:
	cargo test --test release_artifacts

quality: fmt-check clippy test release-verify

release-artifacts:
	./scripts/release-artifacts.sh

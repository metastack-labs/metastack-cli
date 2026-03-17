# GitHub Release Runbook

`metastack-cli` ships production archives for four installer-facing platform slugs:

| Platform slug | Rust target triple | Builder | Release asset |
| --- | --- | --- | --- |
| `darwin-arm64` | `aarch64-apple-darwin` | `cargo` | `metastack-cli-<version>-darwin-arm64.tar.gz` |
| `darwin-x64` | `x86_64-apple-darwin` | `cargo` | `metastack-cli-<version>-darwin-x64.tar.gz` |
| `linux-x64` | `x86_64-unknown-linux-musl` | `cross` | `metastack-cli-<version>-linux-x64.tar.gz` |
| `linux-arm64` | `aarch64-unknown-linux-musl` | `cross` | `metastack-cli-<version>-linux-arm64.tar.gz` |

## Release Contract

- `Cargo.toml` is the source of truth for the CLI version.
- Git tags use the release form `v<version>` such as `v0.1.0`.
- The release workflow checks out the exact requested tag for both `push` and `workflow_dispatch` runs instead of building the default branch tip.
- Packaging fails fast when the resolved release tag does not match the version in `Cargo.toml`.
- Archive names use the plain Cargo package version without the leading `v`.
- Every archive contains a single `meta` binary at the archive root.
- `SHA256SUMS` uses the standard `<sha256><two spaces><filename>` layout so `scripts/install-meta.sh` can verify the downloaded archive before extraction.
- The installer accepts either `--version v0.1.0` or `--version 0.1.0`, normalizes both to the `v0.1.0` release tag, and downloads the asset matching the detected OS and architecture.

## Local Quality Gate

Run the canonical root validation flow before opening or approving release-related changes:

```bash
make quality
```

That target is the same gate used by `.github/workflows/quality.yml` on pull requests and by the `checks` job in `.github/workflows/release.yml`. It runs:

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo test --test release_artifacts`

The final focused test is the release-verification proof. It exercises `scripts/release-artifacts.sh` with stub builders and verifies the expected archive names, `SHA256SUMS` layout, and extracted `meta --version` output without requiring a full cross-platform packaging host.

## One-Time Prerequisites

Install the native macOS targets and the Docker-backed cross compiler:

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo install cross --locked
```

Full local packaging currently assumes a macOS maintainer host because the Darwin archives are built with native Apple tooling. Linux maintainers should use the GitHub Actions workflow for the full matrix and can still package Linux-only subsets locally with repeated `--target` flags.

## Build The Release Assets Locally

From the repository root, run:

```bash
make release-artifacts
```

`make release-artifacts` is the full packaging step. Use `make quality` for the faster CI-aligned validation flow when you only need to prove the root quality gate and release contract.

The release script writes the versioned assets under `target/release-artifacts/<version>/`:

```text
target/release-artifacts/<version>/
  metastack-cli-<version>-darwin-arm64.tar.gz
  metastack-cli-<version>-darwin-x64.tar.gz
  metastack-cli-<version>-linux-x64.tar.gz
  metastack-cli-<version>-linux-arm64.tar.gz
  SHA256SUMS
```

Use repeated `--target` flags when you need a subset for local debugging or workflow parity:

```bash
./scripts/release-artifacts.sh --output-dir /tmp/meta-release-artifacts --target x86_64-unknown-linux-musl --target aarch64-unknown-linux-musl
```

Validate a release/tag pairing before publishing:

```bash
./scripts/release-artifacts.sh --expect-version 0.1.0 --target x86_64-unknown-linux-musl
```

## GitHub Actions Release Flow

`.github/workflows/release.yml` is the production release workflow.

- `.github/workflows/quality.yml` runs `make quality` on every pull request so the local maintainer gate and PR automation stay aligned.
- `push` on `v*.*.*` tags runs `make quality`, builds the full 4-target matrix, assembles a combined `SHA256SUMS`, and publishes the GitHub Release with generated notes.
- `workflow_dispatch` accepts an existing semver tag plus a `draft` toggle so maintainers can rerun packaging or cut a draft release without creating a second workflow path, and the workflow explicitly checks out that tag before running checks or packaging.
- Workflow reruns update release assets in place with `gh release upload --clobber` instead of creating duplicate releases.

## Version Bump And Tagging

1. Update the package version in `Cargo.toml`.
2. Run the validation and packaging commands locally.
3. Commit the version bump and any related docs updates.
4. Create and push the semver tag:

```bash
git tag v0.1.0
git push origin HEAD
git push origin v0.1.0
```

5. Watch the `release` workflow, or use `workflow_dispatch` with `tag=v0.1.0` if a maintainer needs to rerun or publish as a draft.

## Release Validation And Smoke Tests

Validate the published assets and checksums:

```bash
gh release download v0.1.0 --pattern 'metastack-cli-*.tar.gz' --pattern SHA256SUMS --dir /tmp/meta-release
(cd /tmp/meta-release && shasum -a 256 -c SHA256SUMS)
```

Smoke test the installer on macOS:

```bash
curl -fsSL https://raw.githubusercontent.com/metastack-systems/metastack-cli/main/scripts/install-meta.sh | sh -s -- --version v0.1.0
~/.local/bin/meta --version
```

Smoke test the installer on Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/metastack-systems/metastack-cli/main/scripts/install-meta.sh | META_INSTALL_DIR="$HOME/.local/bin" sh -s -- --version v0.1.0
$HOME/.local/bin/meta --version
```

Smoke test the latest-path installer contract:

```bash
curl -fsSL https://raw.githubusercontent.com/metastack-systems/metastack-cli/main/scripts/install-meta.sh | sh
meta --version
```

## Rollback And Yanked Releases

- If a release is still a draft, rerun the workflow or delete the draft release and rerun from the same tag.
- If a published release is bad, do not reuse the same semver tag. Mark the broken release as yanked in the GitHub Release notes, remove or replace assets only when the repository policy allows it, and cut a new patch version.
- If the release needs to disappear entirely before users consume it, delete the GitHub Release and the remote tag, then cut a fresh tag after the fix is ready.

## Maintainer Notes

- `make release-artifacts` reads the version from `Cargo.toml`, so archive names stay aligned with `meta --version`.
- The script validates `meta --version` for binaries it can execute locally, and exits non-zero when a required dependency such as `cross` is missing.
- Override the output root with `./scripts/release-artifacts.sh --output-dir /tmp/meta-release-artifacts` when you do not want to write under `target/`.

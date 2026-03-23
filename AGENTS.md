# MetaStack CLI

This directory contains the MetaStack CLI agent service that polls Linear, creates per-issue workspaces, and runs agents in app-server mode.

## Environment

- Rust: `1.85+` (edition 2024) via `rustup` or `mise`.
- Install deps: `cargo build`.
- Main quality gate: `make quality` (format check, lint, tests, release-artifact verification).

## Codebase-Specific Conventions

- The shared injected agent workflow contract lives in `src/artifacts/injected-agent-workflow-contract.md`; it is compiled into the binary via `include_str!`. Repo-root `WORKFLOW.md` is only legacy compatibility content and documentation.
- Keep the implementation aligned with [`../SPEC.md`](../SPEC.md) where practical.
  - The implementation may be a superset of the spec.
  - The implementation must not conflict with the spec.
  - If implementation changes meaningfully alter the intended behavior, update the spec in the same
    change where practical so the spec stays current.
- Prefer adding config access through `crate::config::AppConfig` instead of ad-hoc env reads.
- Workspace safety is critical:
  - Never run Codex turn cwd in source repo.
  - Workspaces must stay under configured workspace root.
- Orchestrator behavior is stateful and concurrency-sensitive; preserve retry, reconciliation, and cleanup semantics.
- Follow `docs/logging.md` for logging conventions and required issue/session context fields.

## Tests and Validation

Run targeted tests while iterating, then run full gates before handoff.

```bash
make quality
```

## Required Rules

- Public functions must have doc comments (`///`) describing purpose and error conditions.
- All fallible operations must return `Result<T>` with `.context()` on I/O and parse errors.
- No `unwrap()` or `expect()` in production code; use `anyhow::Context` for descriptive error propagation.
- All code must pass `cargo clippy --all-targets --all-features -- -D warnings` cleanly.
- Keep changes narrowly scoped; avoid unrelated refactors.
- Follow existing module/style patterns in `src/`.

Validation command:

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

## PR Requirements

- PR body must follow `../.github/pull_request_template.md` exactly.

## Docs Update Policy

If behavior/config changes, update docs in the same PR:

- `../README.md` for project concept and goals.
- `README.md` for MetaStack CLI implementation and run instructions.
- `WORKFLOW.md` for legacy workflow-overlay compatibility notes and contributor-facing workflow-contract references.

# Validation

## Command Proofs

- `cargo test --test commands meta_backlog_spec_help_exposes_new_subcommand`
- `cargo test --test backlog_spec`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `make quality`

## Results

- `cargo test --test commands meta_backlog_spec_help_exposes_new_subcommand`
  - passed
  - confirmed `meta backlog --help` exposes the `spec` subcommand and includes the repo-local invocation example `meta backlog spec --root .`
- `cargo test --test backlog_spec`
  - passed
  - proved first-run creation writes only `.metastack/SPEC.md`
  - proved repeat-run improvement revises the existing SPEC in place and includes prior SPEC content in the generation prompt
  - proved render-once coverage for the request, follow-up, loading, and review states without writing `.metastack/SPEC.md`
  - proved malformed generated output missing required uppercase headings is rejected
- `cargo clippy --all-targets --all-features -- -D warnings`
  - passed
  - confirmed the new `meta backlog spec` flow, embedded instruction contract, and route-key wiring stay warning-free
- `make quality`
  - passed
  - confirmed the full repository quality gate remains green after the backlog spec implementation and follow-up snapshot expectation fixes

## Notes

- The command remains repo-local and only persists `.metastack/SPEC.md` under the resolved repository root.
- Validation used deterministic local agent stubs for SPEC generation and did not mutate Linear content or `.metastack/backlog/<ISSUE>/` packets.

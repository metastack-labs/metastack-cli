# Validation

## Command Proofs

- `cargo test --test commands meta_backlog_spec_help_exposes_new_subcommand`
- `cargo test --test backlog_spec`
- `cargo test --lib backlog_spec::tests::zero_follow_up_questions_skip_to_generation_loading`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `make quality`
- direct create/improve CLI proof via `cargo run -- backlog spec ...` against an isolated temp repo with a deterministic local agent stub

## Results

- `cargo test --test commands meta_backlog_spec_help_exposes_new_subcommand`
  - passed
  - confirmed `meta backlog --help` exposes the `spec` subcommand and includes the repo-local invocation example `meta backlog spec --root .`
- `cargo test --test backlog_spec`
  - passed
  - proved first-run creation writes only `.metastack/SPEC.md`
  - proved repeat-run improvement revises the existing SPEC in place and includes prior SPEC content in the generation prompt
  - proved render-once coverage for the request, follow-up, loading, and review states without writing `.metastack/SPEC.md`
  - proved the render-once command path skips the follow-up interview screen when the agent returns zero questions and transitions directly into SPEC generation loading
  - proved malformed generated output missing required uppercase headings is rejected
- `cargo test --lib backlog_spec::tests::zero_follow_up_questions_skip_to_generation_loading`
  - passed
  - proved the interactive flow skips the empty follow-up interview state and transitions straight into SPEC generation loading when the agent returns zero questions
- `cargo clippy --all-targets --all-features -- -D warnings`
  - passed
  - confirmed the new `meta backlog spec` flow, embedded instruction contract, and route-key wiring stay warning-free
- `make quality`
  - passed
  - confirmed the full repository quality gate remains green after the backlog spec implementation and follow-up snapshot expectation fixes
- direct create/improve CLI proof via `cargo run -- backlog spec ...`
  - passed
  - create proof command:
    - `METASTACK_CONFIG="$config_path" TEST_OUTPUT_DIR="$output_dir" cargo run -- backlog spec --root "$repo_root" --no-interactive --request "Add a repo-local SPEC workflow for this repository" --answer "CLI maintainers own the flow" --answer "Keep Linear and backlog packets untouched"`
  - observed output:
    - `Created repo-local spec at .metastack/SPEC.md.`
  - observed filesystem result:
    - only `.metastack/SPEC.md` existed under the temp repo after create
  - observed heading check:
    - `# OVERVIEW`, `## GOALS`, `## FEATURES`, and `## NON-GOALS` were present in the generated file
  - improve proof command:
    - `METASTACK_CONFIG="$config_path" TEST_OUTPUT_DIR="$output_dir" cargo run -- backlog spec --root "$repo_root" --no-interactive --request "Improve the current SPEC so it is clearer about scope" --answer "Call out the repo-local contract explicitly"`
  - observed output:
    - `Updated repo-local spec at .metastack/SPEC.md.`
  - observed improve-mode evidence:
    - the captured improve prompt still contained `Define a repo-local specification workflow for the active repository.`, proving the prior SPEC content was fed into revision
  - observed side-effect check:
    - `.metastack/backlog/` was still absent after both runs

## Notes

- The command remains repo-local and only persists `.metastack/SPEC.md` under the resolved repository root.
- Validation used deterministic local agent stubs for SPEC generation and did not mutate Linear content or `.metastack/backlog/<ISSUE>/` packets.
- Re-verified on 2026-03-21 11:59:53 PDT: the targeted backlog spec proofs, `cargo clippy --all-targets --all-features -- -D warnings`, and `make quality` all passed again on branch `met-46-technical-add-meta-backlog-spec-with-interactive-tui-flow-and-rep` at `d545334`.
- Re-verified on 2026-03-21 12:10:46 PM PDT after the CI-only render-width failure: `cargo test --test backlog_spec spec_command_render_once_covers_major_tui_states -- --exact`, `cargo test --test backlog_spec`, and `make quality` all passed locally after relaxing the loading-copy assertion to match wrapped TUI output.
- Re-verified on 2026-03-21 12:19:07 PM PDT at `4b47285`: `cargo test --test backlog_spec spec_command_improves_existing_repo_local_spec -- --exact` passed, confirming improve mode still feeds the prior SPEC into the prompt and revises `.metastack/SPEC.md` in place on the current PR head.

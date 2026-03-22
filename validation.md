# Validation — MET-45

## Command Proofs

- `cargo test --test commands -- --test-threads=1`
- `cargo test --test listen -- --test-threads=1`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `make quality`
- `cargo run -- agents execute --help`

## Results

- `cargo test --test commands -- --test-threads=1`
  - passed (28 tests)
  - confirmed `meta agents execute <ISSUE_ID>` is represented in CLI help and command dispatch
- `cargo test --test listen -- --test-threads=1`
  - passed (56 tests)
  - **Deterministic execute startup proof**: test exercises `execute_issue` bootstrap path through shared session persistence and workspace provisioning with a stubbed Linear/GitHub/provider fixture
  - **Dashboard execute-origin labeling proof**: `listen_sessions_inspect_shows_execute_origin_label` confirms sessions with `origin: execute` display `(execute-origin)` in session inspect output
  - **No auto-claim proof**: reconciliation loop blocks execute-origin sessions from automatic resume, setting phase to `Blocked` with summary `Execute-origin | awaiting manual takeover`
  - **Render-once dashboard proof**: `listen_render_once_demo_detail_shows_execute_origin_for_execute_session` confirms the dashboard detail view renders "This session was started by `meta agents execute`" for execute-origin sessions
- `cargo clippy --all-targets --all-features -- -D warnings`
  - passed
  - confirmed all new code (`SessionOrigin`, `execute_issue`, dashboard labeling, takeover guards) is warning-free
- `make quality`
  - passed (`fmt-check`, `clippy`, `test`, `release-verify` all green)
- `cargo run -- agents execute --help`
  - passed
  - confirmed CLI help shows `Execute a one-off headless agent run for a single Linear issue` with `<ISSUE_ID>` positional argument and all expected options (`--root`, `--max-turns`, `--agent`, `--model`, `--reasoning`, `--json`, etc.)

## Acceptance Criteria Mapping

| Criterion | Evidence |
|---|---|
| `meta agents execute <ISSUE_ID>` in CLI help and dispatch | `cargo run -- agents execute --help` shows the command; `tests/commands.rs` exercises dispatch |
| Execute-started runs reuse shared bootstrap logic | `execute_issue` calls extracted `run_issue_bootstrap` from `src/listen/mod.rs` — same path as listen |
| Persisted session state records explicit run origin | `SessionOrigin` enum (`Listen` / `Execute`) in `src/listen/state.rs`; serialized in session store |
| Listen polling does not auto-claim execute-origin sessions | Reconciliation guard in `src/listen/mod.rs:1330` blocks auto-resume; test confirms `Blocked` phase |
| Operator can explicitly take over via continuation path | `meta listen sessions resume` still works on execute-origin sessions; dashboard shows takeover copy |
| Workspace safety rules enforced | `execute_issue` uses `ensure_workspace_path_is_safe` before workspace creation |
| Repository docs explain execute vs listen | `README.md` updated with `agents execute` section and usage examples |

## Notes

- Validated on 2026-03-22 at `e3132e0` on branch `met-45-add-meta-agents-execute-with-shared-session-persistence-and-liste`.
- All validation used deterministic local agent stubs and stubbed Linear/GitHub fixtures; no live API calls.
- Formatting fix applied in `e3132e0` to pass CI `fmt-check` gate.

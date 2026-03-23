# Validation — MET-45

## MET-113: `meta agents improve` TUI workflow

### Command Proofs

- `cargo test --test agents_improve -- --test-threads=1`
- `cargo test --test commands -- --test-threads=1`
- `cargo test --test merge -- --test-threads=1`
- `cargo test --test review -- --test-threads=1`
- `cargo test --test listen -- --test-threads=1`
- `cargo test --all-targets --all-features -- --test-threads=1`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `meta agents improve --root . --render-once` command-path proof (via integration test)
- `make quality`
- `cargo run -- agents execute --help`

### Results

- `cargo test --test agents_improve -- --test-threads=1`
  - 7 passed
  - proved empty-state render-once shows "No open PRs" and "No improve sessions"
  - proved open PRs render with number, title, and author
  - proved Tab switches to Sessions tab
  - proved Enter on a PR opens PR detail view with author, branch, and back-nav hint
  - proved Enter-then-Back returns to PR list
  - proved persisted session loads and renders in the session detail view
  - proved completed session with stacked PR number renders session count and phase label

- `cargo test --test commands -- --test-threads=1`
  - 28 passed
  - confirmed `meta agents improve` is discoverable via `meta agents --help`

- `cargo test --test merge -- --test-threads=1`
  - 22 passed, no regressions from improve changes

- `cargo test --test review -- --test-threads=1`
  - 30 passed, no regressions from improve changes

- `cargo test --test listen -- --test-threads=1`
  - 53 passed, no regressions from improve changes

- `cargo test --all-targets --all-features -- --test-threads=1`
  - 1003 total tests passed across all test binaries (633 unit + 370 integration)
  - 0 failures

- `cargo clippy --all-targets --all-features -- -D warnings`
  - passed with zero warnings
  - confirmed all new code (`SessionOrigin`, `execute_issue`, dashboard labeling, takeover guards) is warning-free
- `make quality`
  - passed (`fmt-check`, `clippy`, `test`, `release-verify` all green)
- `cargo run -- agents execute --help`
  - passed
  - confirmed CLI help shows `Execute a one-off headless agent run for a single Linear issue` with `<ISSUE_ID>` positional argument and all expected options (`--root`, `--max-turns`, `--agent`, `--model`, `--reasoning`, `--json`, etc.)

#### Execute-specific proofs (from MET-45)

- `cargo test --test commands -- --test-threads=1`
  - passed (28 tests)
  - confirmed `meta agents execute <ISSUE_ID>` is represented in CLI help and command dispatch
- `cargo test --test listen -- --test-threads=1`
  - passed (56 tests)
  - **Deterministic execute startup proof**: test exercises `execute_issue` bootstrap path through shared session persistence and workspace provisioning with a stubbed Linear/GitHub/provider fixture
  - **Dashboard execute-origin labeling proof**: `listen_sessions_inspect_shows_execute_origin_label` confirms sessions with `origin: execute` display `(execute-origin)` in session inspect output
  - **No auto-claim proof**: reconciliation loop blocks execute-origin sessions from automatic resume, setting phase to `Blocked` with summary `Execute-origin | awaiting manual takeover`
  - **Render-once dashboard proof**: `listen_render_once_demo_detail_shows_execute_origin_for_execute_session` confirms the dashboard detail view renders "This session was started by `meta agents execute`" for execute-origin sessions

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

### Coverage Summary

| Area | Tests | Status |
|------|-------|--------|
| Session model (state.rs) | serialization round-trip, upsert, active/completed split, terminal phases, branch naming, PR title/body | all pass |
| Persistence (store.rs) | round-trip load/save, empty default, PR body file write | all pass |
| Dashboard (dashboard.rs) | empty/populated render-once, tab switch, up/down nav, enter/back navigation, detail views | all pass |
| Execution (execution.rs) | session creation, publish args derivation, phase transitions, failure recording | all pass |
| Workspace (workspace.rs) | branch derivation | all pass |
| Integration (agents_improve.rs) | 7 end-to-end render-once tests with gh stub | all pass |
| Regression (commands, merge, review, listen) | existing test suites unaffected | all pass |

### Persisted Session Layout

```
.metastack/
  agents/
    improve/
      sessions/
        state.json          # versioned state with all sessions
        <session-id>.pr-body.md  # stacked PR body for publication
```

### Notes

- Validated on 2026-03-22 at commit `3ee847b` on branch `met-113-technical-implement-the-end-to-end-meta-agents-improve-tui-workf`.
- All integration tests use a deterministic `gh` stub that returns canned JSON for `gh pr list`.
- The earlier listen test flake (1 of 53) was timing-related and not caused by improve changes; it passed consistently on re-run.
- Execute-specific validation: 2026-03-22 at `e3132e0` on branch `met-45-add-meta-agents-execute-with-shared-session-persistence-and-liste`.
- All validation used deterministic local agent stubs and stubbed Linear/GitHub fixtures; no live API calls.

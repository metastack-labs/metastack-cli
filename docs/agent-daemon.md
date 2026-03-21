# Agent Daemon

`meta listen` is the Symphony-inspired orchestration entrypoint for the Rust CLI. The current slice watches a Linear project, filters eligible tickets, claims newly eligible work, prepares an isolated clone-backed ticket workspace, downloads issue attachment context into that workspace, seeds the Linear workpad comment, launches a supervised worker that keeps running the configured local agent until the issue leaves the active workflow states, and surfaces progress in a dashboard without leaving the terminal.

## Design Goals

- Reuse the existing Linear client and `.metastack/` workspace instead of adding a one-off integration path.
- Keep daemon state install-scoped and inspectable so a small team can understand what the
  orchestrator has done across projects.
- Provide a deterministic dashboard render path that works in local development, tests, and CI.
- Keep the runtime modular so later tickets can swap the placeholder pickup flow for real agent execution.

## First Slice

The initial implementation delivered in `MET-13` focuses on the smallest end-to-end loop:

1. `meta listen` polls Linear for Todo issues scoped by `--team` and optional `--project`.
2. Repo-scoped listen config can further require a specific issue label and/or require the assignee to match the Linear viewer tied to the API key.
3. Newly discovered eligible issues are moved to `In Progress`.
4. The daemon creates or refreshes a sibling `<repo>-workspace/<TICKET>` standalone clone rooted at `origin/main`, then checks out a deterministic ticket branch inside that clone.
5. The daemon downloads the issue's Linear attachments into `.metastack/agents/issue-context/<TICKET>/`, plus a generated `README.md` manifest describing downloaded files and any failures.
6. The daemon bootstraps or updates a single `## Codex Workpad` comment on the Linear issue.
7. The daemon writes an agent brief inside the workspace at `.metastack/agents/briefs/<TICKET>.md`.
8. The configured local agent is launched in the workspace with the issue context, attachment-context path, workpad comment id, and optional repo instructions file injected into its prompt/instructions.
9. Session state is persisted to the install-scoped MetaStack data root under
   `listen/projects/<PROJECT_KEY>/session.json`, and agent stdout/stderr are appended to
   `listen/projects/<PROJECT_KEY>/logs/<TICKET>.log`.
10. Session cleanup is record-only: targeted session records are removed or rewritten inside
    `session.json` without deleting `project.json`, `active-listener.lock.json`, or unrelated
    per-issue logs, and live worker PIDs are never cleared automatically.
11. A full-screen ratatui dashboard renders runtime summary rows, a colorized agent table, the pending queue, daemon notes, and an active/completed session toggle.
12. The hidden listen worker keeps refreshing the Linear issue and re-running the agent with first-turn and continuation prompts while the issue remains active.
13. The hidden listen worker keeps looping while the issue remains active, but it treats repeated planning-only or no-op turns as a local stall instead of silently spinning.
14. Once the ticket branch is pushed, the worker creates or updates the matching branch PR as a draft, keeps the `metastack` label attached, and reuses the same PR on continuation instead of replacing it.
15. When the technical backlog is complete and meaningful non-`.metastack/` workspace progress was observed, the worker promotes that same branch PR to ready for review and then attempts to move both the parent issue and backlog child into a review-style state.
16. The worker records `completed` or `blocked` state locally, including stall summaries and recent agent log output for unattended failures.
17. During reconciliation, a stored `running` session with a dead worker PID is marked `blocked`
    with stale/worker-died context preserved in its summary and log references instead of being
    auto-resumed.
18. Completed sessions older than the default 24-hour TTL are pruned automatically during store
    loads and reconciliation, while blocked sessions are retained until explicit cleanup.
19. Live mode keeps the ratatui dashboard open in the terminal and uses the same shared listen snapshot for deterministic `--render-once` output.
20. Built-in `codex` and `claude` worker runs opportunistically capture structured input/output token usage when the provider surfaces it, accumulate those counts in the persisted session record across turns, and leave token fields blank instead of failing when providers omit exact usage data.

This mirrors the scheduler + status-surface split in Symphony while using one clear workspace
contract: each claimed ticket gets its own standalone clone and ticket branch under the configured
workspace root, while listener session state lives in a shared install-scoped store. The store key
is derived from the canonical source project root plus the effective project selector used for the
run, so the source repo checkout and any related worktrees still share one stored session per
project target while different project targets in the same checkout keep separate locks and logs.

## Command Surface

Primary options:

- `--team <KEY>`: Linear team scope.
- `--project <NAME|ID>`: optional project scope. Omitting it falls back to the repo default
  `linear.project_id` when configured.
- `--max-pickups <N>`: cap newly claimed issues per poll.
- `--poll-interval <SECONDS>`: refresh cadence for the live loop. Overrides the repo-scoped default when set.
- `--once`: run a single live cycle and print a textual summary.
- `--render-once`: run a single cycle and print a deterministic ratatui snapshot.
- `--demo`: skip Linear and render sample queue/session data.
- `listen sessions list|inspect|clear|resume`: inspect or reuse stored project sessions from the
  install-scoped listener store. Use `--project` with `inspect`, `clear`, or `resume` to target a
  non-default project from the same checkout, or `--project-key` when you already know the stored
  install-scoped key.
- `listen sessions list` and `inspect` now show the latest tracked provider-native manual resume
  metadata for built-in `codex` and `claude` workers. The dashboard keeps only the compact handle,
  while these commands print the full latest resume ID and provider so operators can copy the
  correct resume target directly.
- `listen sessions clear` accepts an issue identifier, `--blocked`, `--completed`, `--stale`, or
  `--all`; it refuses to remove any targeted record whose stored PID is still alive.
- Live dashboard keys: `Tab` toggles between active and completed sessions, `Left` selects active sessions, `Right` selects completed sessions, and `q` / `Ctrl-C` exits.

Examples:

```bash
meta agents listen --team MET
meta listen sessions list
meta agents listen --team MET --project "MetaStack CLI"
meta agents listen --team MET --project "MetaStack API"
meta listen sessions inspect --root . --project "MetaStack API"
meta listen sessions clear --root . --project "MetaStack API"
meta listen sessions resume --root . --project "MetaStack API" --once
```

Repo-scoped listen settings in `.metastack/meta.json`:

- `listen.required_labels`: optional string list of labels; issues are eligible when any listed label matches case-insensitively.
- `listen.required_label`: legacy single-label compatibility input. New saves persist `required_labels`.
- `listen.assignment_scope`: `any`, `viewer_only`, or `viewer_or_unassigned`.
  - Legacy compatibility: existing `viewer` values still load as `viewer_or_unassigned`.
- `listen.refresh_policy`: `reuse_and_refresh` (default) or `recreate_from_origin_main`.
- `listen.instructions_path`: optional markdown file merged into the shared injected workflow contract for launched-agent instructions.
- `listen.poll_interval_seconds`: default Linear refresh cadence for `meta listen` when `--poll-interval` is not passed.

Listen worker agent selection uses the shared built-in provider resolver:

1. explicit worker overrides such as `--agent`, `--model`, and `--reasoning`
2. the `agents.listen` command route override from `meta runtime config`
3. the `agents` route family override
4. repo defaults from `.metastack/meta.json`
5. install-scoped global defaults

When the selected provider is one of the built-in adapters, the listen worker also emits the
resolved provider/model/reasoning, route key, and config sources through the common launch
diagnostics and `METASTACK_AGENT_*` environment variables before the provider process starts.
Structured built-in output is also parsed for token telemetry so persisted listen sessions and the
dashboard can show cumulative `in`, `out`, and `total` counts when usage is available, while
unsupported or missing counts still render as `n/a`.
Listen-mode built-in launches also switch to machine-readable provider output so the worker can
capture the latest provider-native resume target for the current turn. Codex uses
`codex exec --json`, Claude uses `claude -p --verbose --output-format=stream-json`, and both
capture paths are silent best effort with no backfill of older stored session records.

## Runtime Modules

- `src/listen/mod.rs`: command entrypoint, polling loop, shared snapshot model, state persistence, filtering, attachment-context download, workpad bootstrap, hidden listen worker flow, and prompt/instruction injection.
- `src/listen/dashboard.rs`: ratatui rendering for the live full-screen view and deterministic snapshots.
- `src/listen/workspace.rs`: clone-backed ticket workspace path, refresh, and branch preparation helpers.
- `src/listen/workpad.rs`: deterministic bootstrap workpad rendering.
- `src/agents.rs`: reusable brief-generation and agent-launch helpers shared by `meta listen`, `meta scan`, and the planning flows.
- `src/agent_provider.rs`: built-in provider adapter catalog and launch behavior for `codex` and `claude`.
- `src/workflow_contract.rs`: shared injected workflow contract composition plus optional repo overlay loading.
- `src/listen/store.rs`: install-scoped project identity, metadata, lock, and session-store
  helpers.

## Current Limitations

- Live mode runs in an alternate terminal screen, exposes active/completed session toggles, and exits on `q` or `Ctrl-C` without binding a local TCP port.
- Session persistence is install-scoped and local-file based; there is no remote coordination
  beyond the per-project active-listener lock yet.
- The supervised worker can mark a ticket `blocked` if it exhausts the configured turn cap, or if repeated turns fail to produce meaningful implementation updates while the issue stays active.
- Agent rows already expose stage, age, local session handle, and PID, but real token/rate-limit telemetry is still limited until richer executor telemetry lands.

These are deliberate boundaries for the first slice. Future tickets can add agent executors, richer claim policies, and multi-agent coordination without replacing the command surface introduced here.

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
10. A full-screen ratatui dashboard renders runtime summary rows, a colorized agent table, the pending queue, daemon notes, and an active/completed session toggle.
11. The hidden listen worker keeps refreshing the Linear issue and re-running the agent with first-turn and continuation prompts while the issue remains active.
12. The hidden listen worker keeps looping while the issue remains active, but it treats repeated planning-only or no-op turns as a local stall instead of silently spinning.
13. When the technical backlog is complete and meaningful non-`.metastack/` workspace progress was observed, the worker attempts to move both the parent issue and backlog child into a review-style state.
14. The worker records `completed` or `blocked` state locally, including stall summaries and recent agent log output for unattended failures.
15. Live mode also serves an auto-refreshing local HTML dashboard from the same shared listen snapshot, including matching active/completed session tabs.

This mirrors the scheduler + status-surface split in Symphony while using one clear workspace
contract: each claimed ticket gets its own standalone clone and ticket branch under the configured
workspace root, while listener session state lives in a shared install-scoped store. The store key
is derived from the canonical source project `.metastack` root, so the source repo checkout and any
related worktrees resolve to the same stored project session and active-listener lock.

## Command Surface

Primary options:

- `--team <KEY>`: Linear team scope.
- `--project <NAME>`: optional project scope.
- `--max-pickups <N>`: cap newly claimed issues per poll.
- `--poll-interval <SECONDS>`: refresh cadence for the live loop. Overrides the repo-scoped default when set.
- `--dashboard-port <PORT>`: local browser dashboard port in steady-state mode (`4000` by default, `0` for an ephemeral port in tests).
- `--once`: run a single live cycle and print a textual summary.
- `--render-once`: run a single cycle and print a deterministic ratatui snapshot.
- `--demo`: skip Linear and render sample queue/session data.
- `listen sessions list|inspect|clear|resume`: inspect or reuse stored project sessions from the
  install-scoped listener store.
- Live dashboard keys: `Tab` toggles between active and completed sessions, `Left` selects active sessions, `Right` selects completed sessions, and `q` / `Ctrl-C` exits.

Repo-scoped listen settings in `.metastack/meta.json`:

- `listen.required_label`: only issues carrying this label are eligible.
- `listen.assignment_scope`: `any` or `viewer`.
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

## Runtime Modules

- `src/listen/mod.rs`: command entrypoint, polling loop, shared snapshot model, state persistence, filtering, attachment-context download, workpad bootstrap, hidden listen worker flow, and prompt/instruction injection.
- `src/listen/dashboard.rs`: ratatui rendering for the live full-screen view and deterministic snapshots.
- `src/listen/web.rs`: lightweight local HTTP server and HTML dashboard rendering.
- `src/listen/workspace.rs`: clone-backed ticket workspace path, refresh, and branch preparation helpers.
- `src/listen/workpad.rs`: deterministic bootstrap workpad rendering.
- `src/agents.rs`: reusable brief-generation and agent-launch helpers shared by `meta listen`, `meta scan`, and the planning flows.
- `src/agent_provider.rs`: built-in provider adapter catalog and launch behavior for `codex` and `claude`.
- `src/workflow_contract.rs`: shared injected workflow contract composition plus optional repo overlay loading.
- `src/listen/store.rs`: install-scoped project identity, metadata, lock, and session-store
  helpers.

## Current Limitations

- Live mode runs in an alternate terminal screen, exposes active/completed session toggles, and exits on `q` or `Ctrl-C`.
- Session persistence is install-scoped and local-file based; there is no remote coordination
  beyond the per-project active-listener lock yet.
- The supervised worker can mark a ticket `blocked` if it exhausts the configured turn cap, or if repeated turns fail to produce meaningful implementation updates while the issue stays active.
- Agent rows already expose stage, age, local session handle, and PID, but real token/rate-limit telemetry is still limited until richer executor telemetry lands.

These are deliberate boundaries for the first slice. Future tickets can add agent executors, richer claim policies, remote status surfaces, and multi-agent coordination without replacing the command surface introduced here.

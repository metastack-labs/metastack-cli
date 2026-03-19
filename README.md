
<div align="center">
  <h1>Intuition Org Harness</h1>
  <p><strong>Linear-native planning, repo context, and local agent automation from one CLI.</strong></p>
  <p>Create backlog items, sync planning files, run reusable workflows, and supervise unattended ticket execution without leaving the terminal.</p>
  <p>
    <a href="https://github.com/metastack-systems/metastack-cli/actions/workflows/quality.yml"><img src="https://img.shields.io/github/actions/workflow/status/metastack-systems/metastack-cli/quality.yml?label=quality" alt="Quality status" /></a>
    <a href="https://github.com/metastack-systems/metastack-cli/releases"><img src="https://img.shields.io/github/v/release/metastack-systems/metastack-cli?display_name=tag" alt="Latest release" /></a>
    <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-0f172a" alt="Supported platforms" />
    <img src="https://img.shields.io/badge/built%20with-Rust-f74c00" alt="Built with Rust" />
  </p>
  <p><a href="#install-meta">Install</a> · <a href="#quick-start">Quick Start</a> · <a href="#command-overview">Commands</a> · <a href="#command-reference">Reference</a></p>
</div>

The MetaStack CLI is a Rust terminal tool for engineers who want repository planning context, Linear workflows, and agent-backed automation to stay close to the code.

It is built for teams that want to:

- manage repo-scoped planning state under `.metastack/`
- move between Linear and local backlog files without context switching
- run local agents such as Codex or Claude with repository-aware prompts
- supervise unattended issue execution with `meta agents listen`

## Why MetaStack?

Most planning tools split work across issue trackers, docs, scripts, and ad hoc prompts. MetaStack pulls those workflows back into one place:

- `meta runtime config` saves install-scoped Linear and agent defaults.
- `meta runtime setup` bootstraps the repo and saves repo-scoped defaults under `.metastack/`.
- `meta context scan` turns the codebase into reusable planning context.
- `meta backlog plan`, `meta backlog tech`, `meta linear issues refine`, and `meta agents workflows` generate structured backlog work.
- `meta merge` batches open GitHub PRs into one isolated aggregate merge run and publish step.
- `meta linear ...` and `meta backlog sync` keep Linear and local files aligned.
- `meta agents listen` runs unattended ticket execution in dedicated workspace clones instead of your source checkout.

## Install `meta` During Development

From the root of the repository:

```bash
cargo install --path . --force
```

This will install the `meta` command to your Cargo bin directory, which is typically `~/.cargo/bin`.

## Install `meta` From Source

Install the latest GitHub Release into `~/.local/bin`:

```bash
curl -fsSL https://raw.githubusercontent.com/metastack-systems/metastack-cli/main/scripts/install-meta.sh | sh
```

Install a pinned release instead:

```bash
curl -fsSL https://raw.githubusercontent.com/metastack-systems/metastack-cli/main/scripts/install-meta.sh | sh -s -- --version v0.1.0
```

Install into a custom bin directory without `sudo`:

```bash
curl -fsSL https://raw.githubusercontent.com/metastack-systems/metastack-cli/main/scripts/install-meta.sh | META_INSTALL_DIR="$HOME/bin" sh
```

Download the installer first when you do not want `curl | sh`:

```bash
curl -fsSL https://raw.githubusercontent.com/metastack-systems/metastack-cli/main/scripts/install-meta.sh -o install-meta.sh
sh install-meta.sh --version v0.1.0
```

After installation:

```bash
meta --help
```

## Quick Start

Inside a repository you want metastack to manage:

```bash
meta runtime config
meta runtime setup
meta context scan
meta context show
meta backlog plan --request "Break the next release into Linear-ready tickets"
```

If you are ready to supervise issue execution:

```bash
meta agents listen --team MET --project "MetaStack CLI"
```

## Listen Prerequisites

Before running `meta agents listen` with the built-in providers:

- Built-in Codex workers require `~/.codex/config.toml` to include:

```toml
approval_policy = "never"
sandbox_mode = "danger-full-access"
```

- Remove `[mcp_servers.linear]` from the Codex config when possible. The preflight warns when Linear MCP is detected.
- Built-in Claude workers require `claude` on `PATH`.
- Built-in Claude listen runs should not inherit `ANTHROPIC_API_KEY`; headless listen is expected to use the local Claude subscription instead of an API-key override.
- Run `meta agents listen --check` to validate the active listen provider prerequisites plus Linear reachability/auth without starting the daemon.

`meta runtime setup` bootstraps the repo-local `.metastack/` workspace:

```text
.metastack/
  README.md
  meta.json
  agents/
    README.md
    briefs/
    sessions/
  backlog/
    README.md
    _TEMPLATE/
      README.md
      index.md
      checklist.md
      contacts.md
      decisions.md
      proposed-prs.md
      risks.md
      specification.md
      implementation.md
      validation.md
      context/
      tasks/
      artifacts/
  codebase/
    README.md
  workflows/
    README.md
  cron/
    README.md
```

## Command Overview

The preferred public surface is domain-first. Legacy top-level commands such as `meta plan`, `meta technical`, `meta listen`, and `meta sync` remain available during the migration window and print a hint toward the preferred path.

| Command family | Use it for |
| --- | --- |
| `meta backlog` | Plan, create technical backlog children, and sync backlog work for the current repository |
| `meta linear` | Browse, create, edit, refine, and dashboard Linear work |
| `meta agents` | Run the unattended listener and reusable workflow playbooks |
| `meta context` | Inspect, map, doctor, scan, or reload the effective agent context |
| `meta runtime` | Configure install-scoped and repo-scoped defaults and supervise cron jobs |
| `meta dashboard` | Open Linear, agents, team, or ops-oriented dashboard views |
| `meta merge` | Discover open GitHub PRs, batch them in a one-shot dashboard, and publish one aggregate PR |

## Build From Source

Build and install the CLI into your local Cargo bin directory:

```bash
cargo install --path .
```

Make sure Cargo's bin directory is on your `PATH`:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
```

## Common Workflow

A typical end-to-end loop looks like this:

1. Run `meta runtime config` once to save install-scoped Linear auth and agent defaults.
2. Run `meta runtime setup` once per repository to scaffold `.metastack/` and save repo defaults.
3. Run `meta context scan` to refresh the repo context under `.metastack/codebase/`.
4. Use `meta backlog plan` or `meta backlog tech` to create structured backlog work.
5. Use `meta linear ...`, `meta dashboard ...`, or `meta backlog sync` to coordinate with Linear.
6. Use `meta merge` when you want to batch open GitHub PRs in one isolated aggregate merge run.
7. Use `meta agents listen` when you want unattended ticket execution inside a dedicated workspace clone.

## Example Flows

Engineer:

```bash
meta runtime setup --team MET --project "MetaStack CLI"
meta context scan
meta backlog plan --request "Break the next release into Linear-ready tickets"
meta backlog tech MET-35
```

Team lead:

```bash
meta linear issues list --team MET --state "In Progress"
meta linear issues refine MET-35 --passes 2
meta dashboard team --team MET --project "MetaStack CLI"
```

Ops-style operator:

```bash
meta agents listen --team MET --project "MetaStack CLI" --once
meta dashboard agents --team MET --project "MetaStack CLI" --render-once
meta dashboard ops
meta runtime cron status
```

Aggregate merge operator:

```bash
meta merge --json
meta merge
meta merge --no-interactive --pull-request 101 --pull-request 102 --validate "make quality"
```

## Command Reference

### `runtime config`

Inspect or update the install-scoped MetaStack CLI config:

```bash
meta runtime config
meta runtime config --json
meta runtime config --api-key lin_api_work
meta runtime config --default-profile work
meta runtime config --default-agent codex --default-model gpt-5.4 --default-reasoning medium
meta runtime config --route backlog --route-agent claude --route-model opus
meta runtime config --route backlog.plan --route-agent codex --route-model gpt-5.3-codex
meta runtime config --clear-route backlog.plan
meta runtime config --advanced-routing
```

Legacy alias: `meta config`

`meta runtime config` writes a TOML config file to `$METASTACK_CONFIG` when set, otherwise:

- `$XDG_CONFIG_HOME/metastack/config.toml`
- `~/.config/metastack/config.toml`

The persisted config can store:

- install-scoped Linear API key/default team values
- named global Linear profiles under `[linear.profiles.<name>]`
- an optional global `linear.default_profile`
- global default provider/model/reasoning values for the built-in `codex` / `claude` catalog
- advanced family-level agent routing under `[agents.routing.families.<family>]`
- advanced command-level agent routing under `[agents.routing.commands."<route>"]`

Agent-backed routes resolve install-scoped settings in this order:

1. command route override
2. command family override
3. repo default from `.metastack/meta.json` when present
4. global default

For an individual run, explicit CLI flags still win over the routed defaults:
`--agent`/`--provider` first, then `--model`, then `--reasoning`.

For the built-in providers, `--reasoning`, `default_reasoning`, and `route_reasoning` are validated
against the selected provider/model catalog instead of being accepted as free text. The dashboards
now render reasoning as a select field tied to the current provider/model choice.

Built-in reasoning options shipped in-repo:

- `codex` `gpt-5.4`, `gpt-5.3-codex`, `gpt-5.2-codex`, `gpt-5.1-codex-max`, `gpt-5.1-codex`, `gpt-5.1-codex-mini`, `gpt-5-codex`, `gpt-5-codex-mini`: `low`, `medium`, `high`
- `claude` `sonnet`, `opus`, `haiku`, `sonnet[1m]`, `opusplan`: `low`, `medium`, `high`, `max`

Use `meta runtime config --advanced-routing` for the dedicated routing dashboard, or use
`--route`, `--route-agent`, `--route-model`, `--route-reasoning`, and `--clear-route` for
non-interactive edits.

Supported route families:

- `backlog`
- `context`
- `linear`
- `agents`
- `runtime.cron`
- `merge`

Supported command route keys:

- `backlog.plan`
- `backlog.split`
- `context.scan`
- `context.reload`
- `linear.issues.refine`
- `agents.listen`
- `agents.workflows.run`
- `runtime.cron.prompt`
- `merge.run`

Example global config:

```toml
[linear]
default_profile = "work"

[linear.profiles.work]
api_key = "lin_api_work"
api_url = "https://api.linear.app/graphql"
team = "MET"

[agents]
default_agent = "codex"
default_model = "gpt-5.4"
default_reasoning = "medium"

[agents.routing.families.backlog]
provider = "claude"
model = "opus"
reasoning = "high"

[agents.routing.commands."backlog.plan"]
provider = "codex"
model = "gpt-5.3-codex"
```

### `runtime setup`

Scaffold repo-local `.metastack/` state and inspect or update repo-scoped defaults:

```bash
meta runtime setup
meta runtime setup --json
meta runtime setup --team MET --project "MetaStack CLI"
meta runtime setup --api-key lin_api_repo --team MET --project "MetaStack CLI"
meta runtime setup --provider codex --model gpt-5.4 --reasoning medium
meta runtime setup --listen-label agent --assignment-scope viewer --refresh-policy reuse-and-refresh
```

Legacy alias: `meta setup`

`meta runtime setup` is safe to rerun in an existing checkout. It creates `.metastack/` when needed, seeds `.metastack/backlog/_TEMPLATE/` from the canonical Markdown tree shipped in `src/artifacts/BACKLOG_TEMPLATE`, lets the setup flow inherit shared Linear auth or save a project-specific Linear API key in install-scoped CLI config when a project needs its own token, validates any repo-selected profiles and built-in provider/model/reasoning combinations against the install-scoped catalog, resolves `--project <NAME>` to a canonical Linear project ID before saving, and writes repo defaults only to `.metastack/meta.json`.

For unattended `meta agents listen` runs, setup should be paired with a provider preflight:

- Codex requires `~/.codex/config.toml` with `approval_policy = "never"` and `sandbox_mode = "danger-full-access"`, and `[mcp_servers.linear]` should be removed or disabled.
- Claude requires `claude` on `PATH` and `ANTHROPIC_API_KEY` unset so the local subscription is used.
- Run `meta agents listen --check --root .` to verify the current machine before starting the daemon.

If setup finds canonical template files with local changes, interactive TTY runs prompt for `overwrite`, `skip`, or `cancel`. Non-interactive paths such as `--json` and direct flag updates stop with a clear error instead of silently overwriting those backlog template files.

Repo-dependent commands such as `meta backlog plan`, `meta backlog tech`, `meta backlog sync`, and `meta agents listen` now require repo setup and point back to `meta runtime setup` when `.metastack/meta.json` is missing.

Example repo-scoped config:

```json
{
  "linear": {
    "profile": "work",
    "team": "MET",
    "project_id": "project-42"
  },
  "agent": {
    "provider": "codex",
    "model": "gpt-5.4",
    "reasoning": "medium"
  },
  "listen": {
    "poll_interval_seconds": 30
  },
  "plan": {
    "interactive_follow_up_questions": 6
  }
}
```

Precedence is consistent across the CLI:

- Linear-backed commands use `CLI flag override -> install-scoped repo auth -> repo .metastack/meta.json/profile -> global config -> LINEAR_* environment fallback`
- Agent-backed launches use `CLI override -> repo .metastack/meta.json -> global config`

### `merge`

Inspect open GitHub pull requests for the current checkout, select a batch in a one-shot ratatui dashboard, run an aggregate merge in an isolated workspace outside the source checkout, rerun validation, and open or update one aggregate PR back into the repository default branch.

`meta merge` requires:

- `gh` on `PATH`
- a repo that has already been bootstrapped with `meta runtime setup`
- a configured local agent for merge planning and conflict help

Common invocations:

```bash
meta merge --json
meta merge
meta merge --render-once --events space,down,space,enter
meta merge --no-interactive --pull-request 101 --pull-request 102 --validate "make quality"
```

Behavior summary:

- `--json` emits the resolved GitHub repository metadata plus the open PR list used by the dashboard and planner.
- Plain `meta merge` opens a one-shot dashboard that lets you select multiple PRs, review the selected batch summary, launch immediately, then stay in a live progress screen until the merge run succeeds or fails.
- `--render-once` prints a deterministic dashboard snapshot for tests and proofs.
- `--no-interactive` skips the dashboard and runs the selected `--pull-request` values directly while printing textual phase updates to stdout.
- `--validate <COMMAND>` overrides the post-merge validation commands. When omitted, `meta merge` prefers `make quality` when the repo Makefile exposes that target, otherwise `make all`, otherwise `cargo test` for Rust repositories.
- Publication is gated on those validation commands succeeding. When validation fails, `meta merge` invokes the configured merge agent inside the isolated workspace, commits any repair edits onto the aggregate branch, and reruns validation. The run only stops without publication after the bounded repair loop is exhausted or validation execution itself cannot proceed.
- Both interactive and non-interactive runs publish the same major phases: workspace preparation, plan generation, merge application, validation, push, and PR publication. Merge application also records finer-grained per-PR substeps such as the active pull request and whether conflict assistance ran.

Each run writes local audit artifacts under `.metastack/merge-runs/<RUN_ID>/`, including:

- `context.json` with the repository, selected PR set, aggregate branch, and isolated workspace path
- `agent-plan-prompt.md` with the exact planner prompt sent to the configured local agent
- `plan.json` with the agent-selected merge order and conflict hotspots
- `progress.json` with the current phase, active substep detail, phase states, and the full structured event trail needed to reconstruct success and failure paths
- `merge-progress.json` with the structured run snapshot plus per-PR outcomes
- `validation.json` with each validation attempt, captured command output, and any repair commits recorded between attempts
- `aggregate-pr-body.md` with the Markdown body used when creating or updating the aggregate PR
- `publication.json` with the aggregate PR publication result
- `conflict-prompt-pr-<NUMBER>.md` and `conflict-resolution-pr-<NUMBER>.md` when agent-assisted conflict handling was required
- `validation-repair-prompt-attempt-<N>.md` and `validation-repair-output-attempt-<N>.md` when agent-assisted validation repair was required

### `context scan`

Inspect the current repository, write a deterministic scan fact base, then launch the configured local agent to refresh the higher-level planning docs:

```bash
meta context scan
```

Legacy alias: `meta scan`

Outputs:

- `.metastack/codebase/SCAN.md`
- `.metastack/codebase/ARCHITECTURE.md`
- `.metastack/codebase/CONCERNS.md`
- `.metastack/codebase/CONVENTIONS.md`
- `.metastack/codebase/INTEGRATIONS.md`
- `.metastack/codebase/STACK.md`
- `.metastack/codebase/STRUCTURE.md`
- `.metastack/codebase/TESTING.md`

When stdout is attached to a TTY, `meta context scan` renders a compact progress dashboard. The underlying agent output is captured in `.metastack/agents/sessions/scan.log`.

`meta context scan` treats the resolved repository root as the default target scope for the run. In monorepos, that means the top-level directory you invoked as `--root` (or the current working directory when `--root` is omitted). The scan prompt stays focused on that repository only and should narrow to a subproject only when the user explicitly asks for it.

### `agents workflows`

List, explain, and run reusable workflow playbooks. The CLI ships with built-in playbooks for backlog planning, ticket implementation, PR review, and incident triage, and it also loads repo-local playbooks from `.metastack/workflows/`.

```bash
meta agents workflows list
meta agents workflows explain backlog-planning
meta agents workflows run backlog-planning --param request="Plan a reusable workflow system"
meta agents workflows run ticket-implementation --param issue=MET-93
```

Legacy alias: `meta workflows`

Playbooks use Markdown with YAML front matter. The front matter defines the workflow name, summary, default provider, parameter contract, validation steps, optional instructions, and optional Linear issue lookup parameter. See [`src/artifacts/workflows/README.md`](src/artifacts/workflows/README.md) for the shipped format and `.metastack/workflows/README.md` for the repo-local scaffold.

### `context`

Inspect and refresh the effective context that agent-backed runs consume:

```bash
meta context show
meta context map
meta context doctor
meta context reload
```

- `show` prints the effective repo-scoped instructions, loaded project rules, and known codebase context sources
- `map` prints a repo-map style summary derived from the live repository tree
- `doctor` reports missing or stale inputs such as `.metastack/meta.json`, repo rules, instructions files, and generated codebase docs
- `reload` re-runs the context refresh path used by `meta scan`

### `runtime cron`

Create repository-local cron jobs as Markdown plus YAML front matter, then supervise them from the CLI:

```bash
meta runtime cron init
meta runtime cron init nightly --no-interactive --schedule "0 * * * *" --command "cargo test" --prompt "Review the latest test output and fix any failures"
meta runtime cron status
meta runtime cron start
meta runtime cron stop
meta runtime cron run nightly
```

Legacy alias: `meta cron`

Side effects:

- ensures `.metastack/cron/` exists
- creates `.metastack/cron/<NAME>.md` job definitions
- runs the shell command first when configured, then the optional agent in the same working directory
- creates `.metastack/cron/.runtime/` on demand for scheduler state and logs

In the interactive cron editor, the prompt field submits on `Enter` and inserts a newline on `Shift+Enter`. Image attachments are intentionally rejected there in v1 so saved cron jobs never persist dangling temp-file references.

Cron job files use this shape:

```md
---
schedule: "0 * * * *"
command: "cargo test"
agent: "codex"
shell: "/bin/sh"
working_directory: "."
timeout_seconds: 900
enabled: true
---

Review the command output and update the repository when needed.
```

### `backlog plan`

Turn a planning request into one or more Linear backlog issues:

```bash
meta backlog plan
meta backlog plan --no-interactive --request "Plan a dashboard for feature intake" --answer "Use the existing TUI patterns" --answer "Split the work into multiple tickets"
```

Legacy alias: `meta plan`

In a TTY, `meta backlog plan` opens one persistent ratatui planning session to capture the request, collect follow-up answers, and review the generated ticket breakdown before creating Backlog issues in Linear.

Multiline request and follow-up editors submit on `Enter`; use `Shift+Enter` when you need to insert a newline without advancing the workflow. In the request editor, `Up` and `Down` move the cursor between lines and preserve the visual column across wrapped text when possible.

The request editor and follow-up answer editors support up to 5 pasted images per editor in v1. `Ctrl+V` checks the clipboard for an image first and falls back to normal text paste when no image is present. Pasted local image paths and `file://` URLs are normalized into session-local temp PNG attachments outside the repository, and the editor renders them as non-editable `[Image #N]` placeholders.

Current prompt-image support matrix:

- `meta backlog plan` request editor: supported
- `meta backlog plan` follow-up answer editors: supported
- review-only planning screens: text only
- macOS clipboard image paste: supported
- Linux clipboard image paste: supported through `wl-paste -t image/png` on Wayland and `xclip -selection clipboard -t image/png -o` on X11
- Windows / WSL clipboard image paste: not supported yet; the UI reports a clear error and path / `file://` paste still works

For deterministic automation, pass `--no-interactive` with `--request` and repeated `--answer` values.

The planning prompt is repo-scoped by default: it derives the active project identity from the resolved repository root, plans for the full repository directory, and asks the agent to create backlog issues only for that repository unless the user explicitly narrows the request to a subproject.

Side effects:

- ensures `.metastack/backlog/_TEMPLATE/` exists
- creates one or more Linear backlog issues
- copies the full canonical template tree into `.metastack/backlog/<NEW_ISSUE_ID>/`
- writes each generated backlog item to `.metastack/backlog/<NEW_ISSUE_ID>/`
- uses `.metastack/backlog/<NEW_ISSUE_ID>/index.md` as the initial Linear issue description
- writes `.metastack/backlog/<NEW_ISSUE_ID>/.linear.json` to persist issue metadata

### `backlog tech`

Create a technical sub-issue from an existing Linear parent issue and have the configured local agent turn the repo template into a concrete backlog item:

```bash
meta backlog tech --api-key "$LINEAR_API_KEY" MET-35
meta backlog split --api-key "$LINEAR_API_KEY" MET-35
meta backlog derive --api-key "$LINEAR_API_KEY" MET-35
```

Legacy alias: `meta technical`

The command requires a configured local agent, or one of the built-in supported agents (`codex` / `claude`) available on `PATH`.

`meta backlog tech` uses the same repo-root scope contract as `meta backlog plan`: the agent sees the active repository identity derived from the resolved root, defaults work to the top-level repository directory, and should only produce a narrower technical backlog item when the user explicitly requested a subproject.

In a TTY, the parent-issue picker now uses the shared Linear issue browser:

- type to search by identifier, title, state, project, or description
- matching is case-insensitive and ranks exact identifiers first, then identifier prefixes and exact token matches, then broader substring matches
- shared semantic styling highlights identifiers, titles, state, priority, project, and preview metadata while you review the selected parent issue

Before the agent prompt is rendered, `meta backlog tech` now localizes markdown image references found in the parent issue description, parent-of-parent description, and Linear comments. The generated backlog item always includes `artifacts/ticket-images.md` as a traceability manifest plus `context/ticket-discussion.md` with author-attributed comment context, and the agent sees those rewritten `artifacts/...` paths in its prompt context. Downloads from `uploads.linear.app` send the raw Linear API key in the `Authorization` header; other hosts are fetched with a plain GET.
Side effects:

- ensures `.metastack/backlog/_TEMPLATE/` exists
- asks the configured local agent to inspect the parent Linear issue and author the backlog files from `.metastack/backlog/_TEMPLATE/`
- creates a new Linear child issue under the referenced parent
- copies the full canonical template tree into `.metastack/backlog/<NEW_ISSUE_ID>/`
- writes the generated backlog item to `.metastack/backlog/<NEW_ISSUE_ID>/`
- downloads localized ticket images into `.metastack/backlog/<NEW_ISSUE_ID>/artifacts/`
- writes `.metastack/backlog/<NEW_ISSUE_ID>/artifacts/ticket-images.md` with file name, alt text, source label, and original URL for every discovered markdown image
- writes `.metastack/backlog/<NEW_ISSUE_ID>/context/ticket-discussion.md` with chronological `### **Author** (YYYY-MM-DD)` comment context
- uses `.metastack/backlog/<NEW_ISSUE_ID>/index.md` as the Linear issue description
- uploads the remaining managed backlog files as Linear attachments

### `issues refine`

Critique and rewrite one or more existing Linear issues that already belong to the active repository scope:

```bash
meta issues refine MET-35
meta issues refine MET-35 MET-36 --passes 2
meta issues refine MET-35 --apply
```

`meta issues refine` is the quality-improvement step after `meta plan` or `meta backlog tech`. It reuses the configured local agent to critique the current Linear description, persist each refinement pass under `.metastack/backlog/<ISSUE>/artifacts/refinement/<RUN_ID>/`, and generate a proposed rewrite. By default the command is critique-only.

Pass `--apply` only when you want to promote the final rewrite into `.metastack/backlog/<ISSUE>/index.md` and then push that rewritten description back to Linear. The command always writes the local before/after snapshots first so the refinement run stays auditable even if the remote mutation fails.

Side effects:

- validates that every requested issue matches the configured repo team/project scope
- writes `original.md`, per-pass findings JSON/Markdown, `final-proposed.md`, and `summary.json` under `.metastack/backlog/<ISSUE>/artifacts/refinement/<RUN_ID>/`
- keeps the default flow critique-only, without mutating `.metastack/backlog/<ISSUE>/index.md` or the Linear issue description
- with `--apply`, updates `.metastack/backlog/<ISSUE>/index.md` before attempting the Linear description update
- during `meta listen`, blocks `--apply` for the active ticket so the primary issue description is not overwritten in unattended execution

### `backlog sync`

Browse issues from the repo default Linear project, then pull or push the selected backlog item without leaving the terminal:

```bash
meta backlog sync --api-key "$LINEAR_API_KEY"
meta backlog sync --api-key "$LINEAR_API_KEY" status
meta backlog sync --api-key "$LINEAR_API_KEY" status --fetch
meta backlog sync --api-key "$LINEAR_API_KEY" link MET-35 --entry manual-notes
meta backlog sync --api-key "$LINEAR_API_KEY" link MET-35 --entry manual-notes --pull
meta backlog sync --api-key "$LINEAR_API_KEY" pull MET-35
meta backlog sync --api-key "$LINEAR_API_KEY" pull --all
meta backlog sync --api-key "$LINEAR_API_KEY" push MET-35
meta backlog sync --api-key "$LINEAR_API_KEY" push MET-35 --update-description
meta backlog sync --api-key "$LINEAR_API_KEY" push --all
```

Legacy alias: `meta sync`

Side effects:

- bare `meta backlog sync` opens a ratatui issue browser scoped by `.metastack/meta.json` `linear.project_id`
- `link` associates an existing `.metastack/backlog/<ENTRY>/` directory with a Linear issue by writing `.linear.json`
- `link` prompts for an unlinked backlog entry in a TTY when `--entry <SLUG>` is omitted
- `link --pull` immediately hydrates the linked entry from Linear after writing metadata
- `status` scans `.metastack/backlog/` and prints `identifier | title | status | last sync`
- `status` resolves only local change state by default; pass `--fetch` to check the current Linear issue and surface `remote-ahead` or `diverged`
- `pull` refreshes `.metastack/backlog/<ISSUE_ID>/index.md` from the Linear description and rewrites markdown image references to local `artifacts/...` paths
- `pull` restores CLI-managed attachment files into the same directory when present
- `pull` re-downloads every markdown image referenced by the issue description, parent description, and Linear comments into `.metastack/backlog/<ISSUE_ID>/artifacts/`
- `pull` writes `.metastack/backlog/<ISSUE_ID>/artifacts/ticket-images.md` as a localized-image manifest
- `pull` writes `.metastack/backlog/<ISSUE_ID>/context/ticket-discussion.md` with chronological author-attributed comment context
- `pull` logs per-image download failures without failing the overall sync
- `pull` uses raw `Authorization: <LINEAR_API_KEY>` only for `uploads.linear.app` image downloads; other hosts are fetched without that special auth header
- `pull` persists `.metastack/backlog/<ISSUE_ID>/.linear.json`, including `local_hash`, `remote_hash`, and `last_sync_at` alongside the existing issue metadata
- when `pull` sees a `remote-ahead` or `diverged` packet, it shows a diff between the local `index.md` and the incoming Linear description before any files are overwritten
- in a TTY, `pull` asks for confirmation before overwriting local backlog content; in non-interactive runs it exits non-zero instead of silently replacing changed files
- `pull --all` walks every linked backlog entry sequentially and prints a synced/skipped/error summary
- `push` replaces only CLI-managed attachments by default, leaving unrelated Linear attachments untouched
- `push` leaves the Linear issue description unchanged unless you pass `--update-description`
- `push --update-description` refuses to overwrite the Linear description when the stored baselines resolve to `remote-ahead` or `diverged`
- `push --all` walks every linked backlog entry sequentially, respects `--update-description`, and exits non-zero when any entry fails
- during `meta listen`, `push --update-description` is blocked for the active ticket so the primary issue description stays untouched
- pass `--no-interactive` with `link`, `pull`, or `push` when scripting; in that mode every required selector must be explicit

The sync dashboard and render-once snapshot now include a shared issue search bar plus each issue's local sync state:

- type while the issue list is focused to search by identifier, title, state, project, or description
- matching is case-insensitive and ranks exact identifiers first, then identifier prefixes and exact token matches, then broader substring matches
- the shared browser highlights matches in issue rows and previews and keeps sync-specific actions on the right-hand side

The sync dashboard and render-once snapshot also show each issue's local sync state:

- `synced`: current local and remote hashes still match the stored baselines
- `local-ahead`: local tracked backlog files changed since the last stored baseline, but the Linear issue did not
- `remote-ahead`: the Linear issue changed since the last stored baseline, but the local backlog packet did not
- `diverged`: both local backlog files and the Linear issue changed since the last stored baseline
- `unlinked`: the local packet is missing or the existing `.linear.json` predates hash baselines

Local hashes are derived deterministically from tracked files under `.metastack/backlog/<ISSUE>/`. Dotfiles, including `.linear.json`, are excluded so repeat no-op syncs remain `synced`.

### `linear issues`, `linear projects`, and `dashboard`

Use Linear from the command line:

```bash
meta linear projects list --team MET
meta linear issues list --team MET --project "MetaStack CLI"
meta linear issues list --team MET --json
meta linear issues create --team MET
meta linear issues create --no-interactive --team MET --title "Add docs" --description "Cover command usage"
meta linear issues edit --issue MET-11
meta linear issues edit --no-interactive --issue MET-11 --state "In Progress"
meta linear issues refine MET-11
meta linear issues refine MET-11 --passes 2 --apply
meta dashboard linear --demo
meta dashboard team --team MET
```

Legacy aliases: `meta issues`, `meta projects`, `meta dashboard`

Notes:

- `meta linear issues list` opens an interactive issue browser unless you pass `--json`
- `meta linear issues list`, `meta dashboard linear`, and `meta dashboard team` share the same free-text search behavior when the issue list is focused: type to search by identifier, title, state, project, or description, with exact identifiers ranked ahead of broader matches
- the shared Linear dashboards keep their existing filters, and the search query narrows the visible issue set after those filters are applied
- `meta linear issues create` and `meta linear issues edit` open ratatui workflows when stdin/stdout are attached to a TTY
- In the interactive create/edit forms, multiline descriptions advance on `Enter`, insert a newline on `Shift+Enter`, and support `Up`/`Down` cursor movement within multi-line text
- `meta linear issues refine` is non-interactive, uses the configured local agent, and defaults to critique-only unless you pass `--apply`
- `meta dashboard linear` is the preferred Linear dashboard path; bare `meta dashboard` remains a compatibility alias during migration

Required auth:

- `LINEAR_API_KEY`
- optional: `LINEAR_API_URL`
- optional: `LINEAR_TEAM`

### `agents listen`

Run the unattended agent daemon. The listener watches Todo issues, applies repo-scoped label and assignee filters, moves newly claimed work to `In Progress`, prepares a per-ticket standalone clone under a sibling `-workspace` directory, bootstraps a `## Codex Workpad` comment on the Linear issue, downloads issue attachments into a local attachment-context manifest under `.metastack/agents/issue-context/<TICKET>/`, and launches a supervised listen worker inside that workspace. The worker re-runs the configured local agent with Symphony-inspired first-turn and continuation prompts while the ticket stays active, but it now stops once a turn leaves meaningful local workspace progress and attempts to move the issue into a review-style state instead of burning all 20 turns on the same in-progress work.

Legacy alias: `meta listen`

`meta agents listen` keeps the same repository identity as the source checkout, but the worker prompt is anchored to the provided workspace checkout as the only local write scope. Implementation, validation, and local backlog updates must stay inside that workspace for the active repository unless the issue explicitly asks for a narrower subproject.

The live terminal dashboard refreshes locally every second so session-state changes stay visible, while the configured listen poll interval continues to control how often Linear is queried. Steady-state listen runs stay entirely in the terminal TUI, and `--once` / `--render-once` emit terminal-only summary output.

Examples:

```bash
meta agents listen --demo --render-once
meta agents listen --check --root .
meta agents listen --team MET --project "MetaStack CLI" --once
meta agents listen --team MET --project "MetaStack CLI"
meta runtime setup --listen-label agent --assignment-scope viewer --refresh-policy reuse-and-refresh
```

Listen prerequisites:

- Codex: `~/.codex/config.toml` must include:

```toml
approval_policy = "never"
sandbox_mode = "danger-full-access"
```

- Codex: remove `[mcp_servers.linear]` from the Codex config or disable it; the preflight warns when Linear MCP is detected.
- Claude: `claude` must be on `PATH`, and `ANTHROPIC_API_KEY` should be unset for unattended subscription-backed runs.
- `meta agents listen --check --root .` runs the same startup preflight, including Linear reachability/auth validation, without starting the daemon.

Outputs:

- `<parent>/<repo>-workspace/<TICKET>/`
- repo-scoped listen refresh policy in `.metastack/meta.json`
- `<parent>/<repo>-workspace/<TICKET>/.metastack/agents/briefs/<TICKET>.md`
- `<parent>/<repo>-workspace/<TICKET>/.metastack/agents/issue-context/<TICKET>/README.md`
- install-scoped MetaListen state under the global MetaStack data root, keyed by the canonical
  source project `.metastack` root
- install-scoped MetaListen logs under the same project store
- a live terminal dashboard in steady-state mode, or a render-once terminal snapshot when requested

When `$METASTACK_CONFIG` points to a custom config file, the listener store lives under that
config file's parent `data/` directory. Otherwise the default install-scoped root is derived from
the existing config path rules, for example `~/.config/metastack/data/`. Each project is stored in
`listen/projects/<PROJECT_KEY>/` with `project.json`, `session.json`, an active-listener lock, and
per-issue logs.

Stored-session management commands:

```bash
meta listen sessions list
meta listen sessions inspect
meta listen sessions clear
meta listen sessions resume --project-key <PROJECT_KEY> --once
```

Reference:

- [`docs/agent-daemon.md`](docs/agent-daemon.md)

Linear commands also read repo-scoped defaults from `.metastack/meta.json`, plus optional project-specific Linear auth stored in install-scoped CLI config for the current repo root. Repo defaults should store the canonical Linear project ID; `meta setup --project <NAME>` resolves names to IDs before saving, while older name-based values are still resolved at read time for compatibility. `meta listen` also reads the optional required label, assignee filter, instructions file, and default poll interval from `.metastack/meta.json`, while interactive `meta plan` reads the optional `plan.interactive_follow_up_questions` override there and `meta plan` / `meta backlog tech` resolve the repo-scoped issue-label defaults to real Linear label IDs before issue creation, falling back to `plan` / `technical` when unset. The optional `linear.ticket_context.discussion_prompt_chars` and `linear.ticket_context.discussion_persisted_chars` settings control the comment-character budgets used for agent-facing and persisted `context/ticket-discussion.md` output. During `meta setup` saves, metastack checks that the effective listen, plan, and technical labels exist on the selected team and creates any missing team labels so later issue creation stays deterministic. When `meta linear issues list` returns no rows, it prints the applied filters so hidden repo defaults are visible.
## Agent Configuration

Agent-backed commands use stable route keys so different workflows can resolve different defaults from the same install-scoped config. `meta backlog plan`, `meta backlog split`, `meta context scan`, `meta context reload`, `meta linear issues refine`, `meta agents workflows run`, `meta runtime cron run`, `meta agents listen`, and `meta merge run` all resolve provider/model/reasoning in this order:

1. explicit CLI overrides such as `--agent`, `--provider`, `--model`, and `--reasoning`
2. command route override
3. command family override
4. repo default from `.metastack/meta.json` when present
5. global default

Workflow playbooks can still declare a built-in provider, but that value is now only used as the final fallback when the explicit, route, repo, and global config layers do not select one.

The built-in provider adapters are the single source of truth for metadata and launch behavior. They run `codex exec` and `claude -p`, pass `--model=<value>` automatically when a model is configured, validate reasoning against the selected provider/model, and expose resolution diagnostics before launch. Before spawning a built-in provider, the CLI now checks the installed shell help surface for the emitted flags and fails fast with the resolved provider/model/reasoning plus the exact attempted command if the local binary has drifted. Codex reasoning is passed as `-c reasoning.effort="<value>"`; Claude reasoning is passed as `--effort=<value>`.

Built-in providers are also the only prompt-image launch path in v1. When a supported prompt-bearing editor submits attachments, the CLI preserves attachment order, resizes oversized images to fit within `2048x768`, base64-encodes the resulting PNG payloads, and appends an explicit attachment block to the built-in provider prompt. Custom configured agents fail fast with a clear unsupported-provider error instead of dropping the images silently.

Sandbox and permission handling depends on the command path:

- `meta agents listen` uses unrestricted execution for built-in providers so unattended workers can run validation, git/GitHub flows, and Linear updates. Codex uses `--dangerously-bypass-approvals-and-sandbox`; Claude uses `--permission-mode=bypassPermissions`.
- `meta context scan`, `meta backlog plan`, `meta backlog split`, `meta linear issues refine`, workflow runs, merge flows, and cron prompts keep the built-in Codex adapter on `--sandbox workspace-write --ask-for-approval never`.

Listen startup now runs a provider preflight before polling Linear, and worker pickup reruns it inside the workspace before the first agent turn. Codex checks require a readable `~/.codex/config.toml` with `approval_policy = "never"` and `sandbox_mode = "danger-full-access"` and warn when `[mcp_servers.linear]` is configured. Claude checks require `claude` on `PATH` and fail fast when `ANTHROPIC_API_KEY` is set. Both providers also validate that the resolved built-in launch command exposes the required unrestricted mode for unattended listen runs.

This is intentionally stricter than Codex `--full-auto`: in `codex-cli 0.115.0`, `codex exec --help` documents `--full-auto` as `--sandbox workspace-write`, which is still too restrictive for unattended listen workers that need network, git, GitHub, and Linear mutations.

Agent launches receive:

For `meta plan`, `meta backlog tech`, `meta issues refine`, `meta scan`, and `meta listen`, the rendered agent prompt also includes a shared repo-target contract derived from the resolved command root:

- the built-in workflow contract shipped in `src/artifacts/injected-agent-workflow-contract.md`
- the resolved `RepoTarget` scope block, including repo identity and root path
- optional repo overlays from root `AGENTS.md` and legacy `WORKFLOW.md`
- optional repo-scoped instructions configured in `.metastack/meta.json`
- for `meta listen`, an additional unattended workspace/workpad layer on top of that shared contract

- a combined payload via the configured transport (`arg` or `stdin`)
- `METASTACK_AGENT_NAME`
- `METASTACK_AGENT_PROMPT`
- `METASTACK_AGENT_INSTRUCTIONS`
- `METASTACK_AGENT_MODEL`
- `METASTACK_AGENT_REASONING`
- `METASTACK_AGENT_ROUTE_KEY`
- `METASTACK_AGENT_FAMILY_KEY`
- `METASTACK_AGENT_PROVIDER_SOURCE`
- `METASTACK_AGENT_MODEL_SOURCE`
- `METASTACK_AGENT_REASONING_SOURCE`
- `METASTACK_LINEAR_ATTACHMENT_CONTEXT_PATH` when the issue has downloaded attachment context

`meta agents workflows run --dry-run` now prints the resolved provider/model/reasoning plus their
resolution sources. `meta context scan` also writes the same diagnostics into the scan agent log so
misrouting can be proved from the persisted runtime evidence.

If you need to override the built-in launch command, you can still customize the persisted agent command in the config file:

```toml
[agents]
default_agent = "codex"
default_model = "gpt-5.3-codex"

[agents.commands.codex]
command = "codex"
args = ["exec", "{{model_arg}}"]
transport = "arg"
```

## Quality Gate

Run the canonical root validation flow with:

```bash
make quality
```

`make quality` is the local maintainer and pull-request gate. It runs:

- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- `cargo test --test release_artifacts`

The focused `release_artifacts` proof keeps the GitHub Release packaging contract explicit in the root gate by verifying the release-script archive names, `SHA256SUMS`, and extracted `meta --version` output.

## Testing

Run the full Rust test suite from the repository root with:

```bash
cargo test
```

The integration suite is split by command domain, so local iteration can stay focused:

- `cargo test --test config`
- `cargo test --test scan`
- `cargo test --test plan`
- `cargo test --test refine`
- `cargo test --test sync`
- `cargo test --test linear`
- `cargo test --test listen`
- `cargo test --test cron`

## Release Artifacts

Maintainers can package the supported GitHub Release assets with:

```bash
make release-artifacts
```

Use `make release-artifacts` when you need the full versioned archives under `target/release-artifacts/<version>/`.

Reference:

- [`docs/manual-releases.md`](docs/manual-releases.md)

## Additional Docs

- [`docs/agent-daemon.md`](docs/agent-daemon.md)
- [`docs/manual-releases.md`](docs/manual-releases.md)
- [`src/artifacts/workflows/README.md`](src/artifacts/workflows/README.md)

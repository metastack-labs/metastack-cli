# Cron Workflows

`meta runtime cron` now supports Markdown-defined workflows with durable per-run state, approvals, retries, and resumable execution.

## Discovery

Definitions are loaded in this precedence order:

1. install-scoped `$METASTACK_CONFIG` data root under `data/cron/`
2. repo-scoped `.metastack/cron/`

When both roots define the same filename stem, the repo-scoped definition wins. `meta runtime cron list` reports the resolved source for each workflow, and `meta runtime cron validate` checks every discovered definition without starting the scheduler.

## Contract

Workflow files keep the existing Markdown plus YAML-front-matter shape.

Supported top-level front matter keys:

- `schedule`: required cron expression
- `enabled`: optional boolean, defaults to `true`
- `mode`: optional `legacy` or `workflow`
- `retry.max_attempts`: optional integer, defaults to `1`
- `retry.backoff_seconds`: optional integer, defaults to `0`
- `steps`: optional explicit workflow step list

Legacy `command`, `agent`, `prompt`, `shell`, `working_directory`, and `timeout_seconds` fields still load as a two-step synthesized workflow when `steps` is omitted.

Supported step types:

- `shell`: run a shell command in a repo-local working directory
- `agent`: run a routed agent prompt with prior step outputs in scope
- `cli`: run the current `meta` binary with subcommand args
- `approval`: pause and persist a pending approval checkpoint

Optional step fields:

- `when`: branch on a prior step output
- `guardrails.allow`: explicit allowed mutation targets
- `guardrails.mutates`: declared mutation targets that must be covered by `allow`

## Runtime Artifacts

The runtime persists under `.metastack/cron/.runtime/`:

- `scheduler.json`: scheduler heartbeat and per-job summary
- `logs/<RUN_ID>.log`: append-only run log
- `runs/<RUN_ID>.json`: durable workflow run state, steps, retries, and approvals
- `jobs/<NAME>.json`: latest per-job runtime summary for status output

`meta runtime cron run <NAME>` creates a fresh run artifact. `meta runtime cron resume <RUN_ID>` reuses the persisted run state and skips completed steps. Interrupted runs are reconciled automatically when the daemon restarts.

## Approval Flow

Approval checkpoints set the run status to `waiting_for_approval`.

- `meta runtime cron approvals`
- `meta runtime cron approve <RUN_ID> --note "..."`
- `meta runtime cron reject <RUN_ID> --reason "..."`

Approving a run resumes execution after the approval step. Rejecting a run records terminal rejected state in the run artifact.

## Validation Walkthrough

Use this proof flow when validating the workflow runtime locally without starting the scheduler:

1. Create a proof root and onboarded config, then copy the shipped samples into `.metastack/cron/`.
2. Add one approval-gated workflow and one resumable workflow.
3. Run `meta runtime cron list` and `meta runtime cron validate` against that proof root.
4. Run the approval workflow, inspect `meta runtime cron approvals`, then approve and reject separate runs.
5. Run the resumable workflow once so it fails, fix the blocking condition, then call `meta runtime cron resume <RUN_ID>`.

Example command sequence:

```bash
meta runtime cron --root target/cron-proof list
meta runtime cron --root target/cron-proof validate
meta runtime cron --root target/cron-proof run review
meta runtime cron --root target/cron-proof approvals --json
meta runtime cron --root target/cron-proof approve <RUN_ID> --note "ship it"
meta runtime cron --root target/cron-proof reject <RUN_ID> --reason "not ready"
meta runtime cron --root target/cron-proof run resume-strict
meta runtime cron --root target/cron-proof resume <RUN_ID>
```

Expected proof points:

- `list` shows disabled shipped samples plus repo-local proof workflows with source, mode, and step count.
- `validate` succeeds without creating scheduler state or starting the daemon.
- approval runs persist `.metastack/cron/.runtime/runs/<RUN_ID>.json` with `status: waiting_for_approval`, then transition to `succeeded` or `rejected` after operator action.
- resume proofs keep already-completed steps at the same `attempt_count` while rerunning only the interrupted or failed step.
- runtime artifacts appear under `.metastack/cron/.runtime/jobs/`, `.metastack/cron/.runtime/logs/`, and `.metastack/cron/.runtime/runs/`.

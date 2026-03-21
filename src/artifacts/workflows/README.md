# Workflow Playbooks

`meta agents workflows` supports reusable Markdown playbooks with YAML front matter.

## Format

```md
---
name: release-triage
summary: Investigate a release blocker and propose next actions.
provider: codex
parameters:
  - name: incident
    description: Human-readable incident summary.
    required: true
validation:
  - Confirm impact, owner, and current workaround.
instructions: |
  Keep the report concise and action-oriented.
linear_issue_parameter: issue
---
Incident summary:
{{incident}}
```

Supported front matter keys:

- `name`: unique workflow identifier used by `meta agents workflows explain|run`
- `summary`: one-line description shown by `meta agents workflows list`
- `provider`: default local agent/provider name used for `run`
- `parameters`: input contract with `name`, `description`, optional `required`, and optional `default`
- `validation`: checklist items shown by `explain` and `run`
- `instructions`: optional agent instructions rendered separately from the main prompt
- `linear_issue_parameter`: optional parameter name whose value should be resolved from Linear before prompt rendering

Workflow execution resolves provider/model/reasoning with the same precedence used by the other
agent-backed commands:

1. explicit `meta agents workflows run --provider/--model/--reasoning` overrides
2. the `agents.workflows.run` command route override from `meta runtime config`
3. the `agents` route family override
4. repo defaults from `.metastack/meta.json`
5. install-scoped global defaults
6. the workflow front matter `provider` as the final fallback

For the built-in `codex` and `claude` providers, the in-repo adapter catalog is the source of truth
for supported models and reasoning options. `meta agents workflows run --dry-run` prints the
resolved provider/model/reasoning plus their resolution sources so misrouting can be verified
before launch.

Prompt templates can reference workflow parameters plus shared variables such as:

- `{{repo_root}}`
- `{{workflow_contract}}` for the full injected workflow contract, repo scope block, repo overlays, and repo-scoped instructions
- `{{effective_instructions}}` for only the optional repo-scoped instructions file from `.metastack/meta.json`
- `{{project_rules}}` for only repo overlay content from `AGENTS.md` and legacy `WORKFLOW.md`
- `{{context_bundle}}`
- `{{repo_map}}`
- `{{validation_steps}}`
- `{{issue_identifier}}`, `{{issue_title}}`, `{{issue_url}}`, `{{issue_state}}`, `{{issue_description}}` when `linear_issue_parameter` is set

Repo-local playbooks live under `.metastack/workflows/`. The built-in playbooks shipped by this repository live alongside this README in `src/artifacts/workflows/`.

Legacy alias: `meta workflows`

## Run Experience

`meta agents workflows run <NAME>` is TUI-first when stdin/stdout are attached to a TTY:

- the wizard walks through one workflow parameter per step
- required values are validated inline before generation
- Linear issue parameters must look like identifiers such as `MET-50`
- generation lands on a review/export dashboard instead of exiting immediately
- `e` opens multiline edit mode for the generated Markdown
- `s` opens a one-off save-path prompt with a `.metastack/workflows/generated/` default

For scripts and tests, use the deterministic fallback:

```bash
meta agents workflows run <NAME> --no-interactive --param key=value
meta agents workflows run <NAME> --no-interactive --param key=value --output path/to/file.md
```

The fallback keeps the existing promptless execution contract and only overwrites an existing
output file when `--overwrite` is supplied.

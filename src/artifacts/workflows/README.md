# Workflow Playbooks

`meta workflows` supports reusable Markdown playbooks with YAML front matter.

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

- `name`: unique workflow identifier used by `meta workflows explain|run`
- `summary`: one-line description shown by `meta workflows list`
- `provider`: default local agent/provider name used for `run`
- `parameters`: input contract with `name`, `description`, optional `required`, and optional `default`
- `validation`: checklist items shown by `explain` and `run`
- `instructions`: optional agent instructions rendered separately from the main prompt
- `linear_issue_parameter`: optional parameter name whose value should be resolved from Linear before prompt rendering

Workflow execution resolves provider/model/reasoning with the same precedence used by the other
agent-backed commands:

1. explicit `meta workflows run --provider/--model/--reasoning` overrides
2. the `agents.workflows.run` command route override from `meta runtime config`
3. the `agents` route family override
4. repo defaults from `.metastack/meta.json`
5. install-scoped global defaults
6. the workflow front matter `provider` as the final fallback

For the built-in `codex` and `claude` providers, the in-repo adapter catalog is the source of truth
for supported models and reasoning options. `meta workflows run --dry-run` prints the resolved
provider/model/reasoning plus their resolution sources so misrouting can be verified before launch.

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

---
name: pr-review
summary: Review a patch or pull request against the repo's current constraints and conventions.
provider: codex
parameters:
  - name: change_summary
    description: Patch summary, diff excerpt, or PR description to review.
    required: true
  - name: focus
    description: Optional review focus such as performance, UX, or regression risk.
    required: false
validation:
  - Prioritize bugs, regressions, and missing tests over style-only commentary.
  - Tie findings back to concrete repo rules, codebase context, or runtime behavior.
  - Call out open questions when the supplied patch summary is incomplete.
instructions: |
  You are acting as a strict code reviewer. Findings come first.
---
You are running the `pr-review` workflow for `{{repo_root}}`.

Review focus:
{{focus}}

Patch summary:
{{change_summary}}

Injected workflow contract:
{{workflow_contract}}

Codebase context:
{{context_bundle}}

Repo map:
{{repo_map}}

Validation steps:
{{validation_steps}}

Return findings first, then open questions, then a short change summary only if useful.

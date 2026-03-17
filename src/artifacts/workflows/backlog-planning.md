---
name: backlog-planning
summary: Break a product request into actionable backlog work for the current repository.
provider: codex
parameters:
  - name: request
    description: Product or feature request to plan.
    required: true
  - name: constraints
    description: Optional constraints, scope notes, or sequencing guidance.
    required: false
validation:
  - Confirm the request is broken into independently actionable backlog slices.
  - Preserve repo-specific constraints from project rules and codebase context.
  - Call out open questions or risks that would block implementation.
instructions: |
  You are a staff engineer writing concrete, execution-ready backlog plans.
---
You are running the `backlog-planning` workflow for the repository at `{{repo_root}}`.

Injected workflow contract:
{{workflow_contract}}

Codebase context:
{{context_bundle}}

Repo map:
{{repo_map}}

Product request:
{{request}}

Constraints:
{{constraints}}

Validation steps:
{{validation_steps}}

Return a concrete implementation and backlog plan with scope, risks, and recommended ticket slices.

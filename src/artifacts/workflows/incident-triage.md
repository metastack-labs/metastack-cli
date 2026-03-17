---
name: incident-triage
summary: Triage an incident or blocker using current repo context and operator notes.
provider: codex
parameters:
  - name: incident
    description: Incident summary or blocker statement.
    required: true
  - name: impact
    description: User or system impact observed so far.
    required: false
  - name: current_signals
    description: Logs, symptoms, or known repro signals.
    required: false
validation:
  - Separate confirmed facts from hypotheses.
  - Identify immediate mitigation, next diagnostic step, and owner handoff needs.
  - Call out missing data that blocks a confident triage conclusion.
instructions: |
  You are triaging a live blocker. Stay concrete, ordered, and explicit about uncertainty.
---
You are running the `incident-triage` workflow for `{{repo_root}}`.

Incident:
{{incident}}

Impact:
{{impact}}

Current signals:
{{current_signals}}

Injected workflow contract:
{{workflow_contract}}

Codebase context:
{{context_bundle}}

Repo map:
{{repo_map}}

Validation steps:
{{validation_steps}}

Return a triage note with confirmed facts, hypotheses, immediate mitigation, next diagnostic actions, and follow-up owners.

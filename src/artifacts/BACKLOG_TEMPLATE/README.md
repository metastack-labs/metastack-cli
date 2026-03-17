# Backlog Item Template

This directory is the canonical backlog template embedded into `meta setup`, `meta plan`, and `meta backlog tech`.

## Required Files

- `index.md`: summary, scope, milestones, and next actions
- `specification.md`: technical contract and acceptance criteria
- `checklist.md`: execution checklist by area
- `contacts.md`: owners, reviewers, and escalation points
- `proposed-prs.md`: PR slices and merge sequencing
- `decisions.md`: decision log
- `risks.md`: risk register and open questions
- `implementation.md`: concrete implementation touchpoints
- `validation.md`: command proofs and evidence notes

## Supporting Folders

- `context/`: research and background notes
- `tasks/`: workstream-level implementation plans
- `artifacts/`: outputs and evidence produced during execution

## Editing Notes

1. Replace placeholders:
   - `{{BACKLOG_TITLE}}`
   - `{{BACKLOG_SLUG}}`
   - `{{TODAY}}`
2. Keep this template lightweight. Add large docs only when needed.
3. Use relative links that resolve from each file's own directory.
4. `index.md` is the required root document for both planning and technical generation.

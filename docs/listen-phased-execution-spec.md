# Listen Phased Execution Spec

This document defines the phased execution model for `meta agents listen`.

## Goals

- Execute Linear tickets as quickly as possible with minimal orchestration overhead.
- Keep the Linear ticket as the primary work contract.
- Allow as many execution turns as needed until the ticket is complete.
- Preserve quality through explicit review phases instead of relying on backlog heuristics.
- Keep local backlog files and the Linear workpad synchronized as reporting artifacts, not as the completion gate.

## Source Of Truth

The Linear ticket is the execution source of truth:

- title
- description
- acceptance criteria
- validation / test-plan sections
- attachments and ticket discussion

Listener-specific CLI instructions remain additive and minimal. Local backlog files are supportive tracking artifacts only.

## Phase Model

Each listener cycle for an active ticket follows these phases:

1. `execute`
2. `review`
3. `continue` or `final_review`
4. `publish`

### Execute

The execution phase receives:

- ticket context from Linear
- workspace path
- minimal repo/listener contract
- attachment context paths when present
- current workpad reference
- compact delta context from the previous review on continuation turns

The execution phase must attempt to complete as much of the remaining ticket work as possible before stopping.

### Review

The review phase is a separate agent pass that compares the current workspace against:

- Linear acceptance criteria
- explicit validation requirements
- the requested ticket deliverables

The review phase must return structured JSON with:

- `summary`
- `complete`
- `completed_items`
- `remaining_items`
- `validation_completed`
- `validation_remaining`
- `risks`
- `notes`

The listener uses this review output to:

- update the Linear workpad comment
- update the local backlog progress checklist section
- decide whether another execution turn is required

### Continue

If the review phase reports incomplete work, the next execution turn receives compact delta context only:

- what was completed
- what remains
- validation still required
- risks that still need attention

This continuation path avoids re-injecting the full ticket context when it is not necessary.

### Final Review

When the review phase reports `complete = true`, the listener runs one more fast safety review. The final review must return structured JSON with:

- `approved`
- `summary`
- `missing_items`
- `validation_gaps`
- `risks`
- `notes`

If final review fails, the missing items become the next continuation delta and execution resumes.

### Publish

When final review approves the work:

- the branch PR is refreshed and promoted to ready
- the `metastack` label is preserved
- the PR is attached to Linear
- the Linear ticket is moved from `In Progress` to the review-style state

## Tracking Artifacts

### Linear Workpad

The active `## Codex Workpad` comment is rewritten after each review phase to show:

- summary
- completed checklist
- remaining checklist
- validation checklist
- risks / notes

### Local Backlog

If a local backlog entry exists, the listener updates a managed section in `index.md`:

- `## Listener Progress Checklist`

This section is reporting output only. It must not decide ticket completion by itself.

## Completion Rules

The listener no longer treats backlog completeness as the completion gate.

A ticket is complete only when:

- review says the ticket deliverables are complete
- final review approves the result
- publish / Linear review handoff succeeds

## Turn Budget

`max_turns` limits execution turns, not the lightweight review/final-review passes around each turn.

This preserves quality while keeping the main execution loop bounded.

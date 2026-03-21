# Decision: Linear Workflow State Creation

**Status:** Rejected (read-only)
**Date:** 2026-03-21

## Context

The CLI allows users to configure a default Linear workflow status for new standalone
issues. During onboarding and manual config flows, the picker queries Linear for the
team's available workflow states and presents them as choices.

The question arose whether the CLI should also support *creating* new workflow states
in Linear when a desired state does not exist.

## Decision

The CLI is explicitly **read-only** for workflow state selection. It queries existing
workflow states from Linear but does not create, modify, or delete them.

### Rationale

1. **Permissions:** Creating workflow states requires workspace-admin-level access in
   Linear. Most CLI users authenticate with personal API keys that do not carry admin
   permissions. Attempting to create states would fail for the majority of users and
   produce confusing errors.
2. **Shared resource:** Workflow states are workspace-wide settings shared across all
   team members. Mutating them from a local CLI tool risks unintended side effects for
   the entire team.
3. **Scope:** The CLI's goal is to streamline issue creation and backlog management,
   not to administer Linear workspace configuration. State management belongs in the
   Linear UI where team leads can review and coordinate changes.

## Consequences

- The onboarding and config pickers only display states that already exist on the
  selected Linear team.
- If a user configures a `default_state` value that does not match any state on the
  target team, the `create_issue` call will fail with a clear error message indicating
  the state was not found.
- Users who need a new workflow state must create it in the Linear UI first, then
  re-run onboarding or update their config to select it.

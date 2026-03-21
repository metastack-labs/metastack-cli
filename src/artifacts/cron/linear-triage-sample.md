---
schedule: "0 9 * * 1-5"
mode: workflow
enabled: false
retry:
  max_attempts: 2
  backoff_seconds: 5
steps:
  - id: intake
    type: agent
    agent: "codex"
    prompt: "Review open backlog items, cluster them by theme, and draft a triage summary for the current repository."
  - id: approval
    type: approval
    approval_message: "Approve creating or refining Linear ticket notes from the triage summary."
  - id: linear_snapshot
    type: cli
    command: "linear"
    args: ["issues", "list", "--state", "Backlog"]
    guardrails:
      allow: ["linear"]
      mutates: []
---
Sample only. Copy into `.metastack/cron/` and enable it explicitly before use.

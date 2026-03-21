---
schedule: "0 14 * * 1-5"
mode: workflow
enabled: false
retry:
  max_attempts: 2
steps:
  - id: collect
    type: shell
    command: "git status --short && git branch --show-current"
  - id: review
    type: agent
    agent: "codex"
    prompt: "Review the current branch for PR readiness, summarize risks, and highlight missing validation."
  - id: approval
    type: approval
    approval_message: "Approve running GitHub review-prep inspection commands."
  - id: github_snapshot
    type: cli
    command: "status"
    args: []
    when:
      step: review
      path: status
      exists: true
    guardrails:
      allow: ["github"]
      mutates: []
---
Sample only. Copy into `.metastack/cron/` and enable it explicitly before use.

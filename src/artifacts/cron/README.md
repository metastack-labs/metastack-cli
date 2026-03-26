# Cron Workflow Samples

These sample workflow definitions are shipped with the repository as disabled-by-default references for `{{brand.command_name}} runtime cron`.

- `linear-triage-sample.md`: triage and scope Linear backlog work
- `github-pr-review-sample.md`: assess PR readiness before review handoff

They are not auto-installed. Copy them into `{{brand.project_dir}}/cron/` or the install-scoped `data/cron/` root, then run `{{brand.command_name}} runtime cron validate` or `{{brand.command_name}} runtime cron list` to inspect the resolved definitions before enabling them.

# Proposed PRs: {{BACKLOG_TITLE}}

Last updated: {{TODAY}}

## PR Strategy

- Keep each PR independently reviewable.
- Land contract changes before consumer migration PRs.
- Avoid mixing behavior changes with broad refactors.

## Planned PRs

| PR ID | Goal | Files/Areas | Depends On | Risk | Owner | Status |
|---|---|---|---|---|---|---|
| {{BACKLOG_SLUG}}-01 | Lock contract surface | `TBD` | None | Medium | `@tbd` | planned |
| {{BACKLOG_SLUG}}-02 | Implement core behavior | `TBD` | {{BACKLOG_SLUG}}-01 | Medium | `@tbd` | planned |
| {{BACKLOG_SLUG}}-03 | Consumer alignment + tests | `TBD` | {{BACKLOG_SLUG}}-02 | Low | `@tbd` | planned |

## Merge Order

1. `{{BACKLOG_SLUG}}-01`
2. `{{BACKLOG_SLUG}}-02`
3. `{{BACKLOG_SLUG}}-03`

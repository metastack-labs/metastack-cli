# Checklist: {{BACKLOG_TITLE}}

Last updated: {{TODAY}}

## 1. Baseline and Decisions

- [ ] Confirm scope and non-goals in `index.md`.
- [ ] Confirm contract boundaries in `specification.md`.
- [ ] Confirm owners/reviewers in `contacts.md`.

## 2. Implementation Tasks by Area

### Area: Core Package or Service

- [ ] Implement core contract changes.
- [ ] Add validation for inputs/config.
- [ ] Add tests for happy-path and failure-path behavior.

### Area: Consumer Integrations

- [ ] Update consuming apps/services.
- [ ] Remove consumer-side ad hoc transforms.
- [ ] Add integration compatibility tests.

### Area: Tooling and Docs

- [ ] Update developer docs.
- [ ] Add migration notes if contracts changed.
- [ ] Ensure all links and references resolve.

## 3. Cross-Cutting Quality Gates

- [ ] Deterministic behavior verified for identical inputs/config.
- [ ] No forbidden dependencies or unsafe imports introduced.
- [ ] Observability and logs cover key failure cases.
- [ ] Performance budget validated.

## 4. Exit Criteria

- [ ] `Definition of Done` in `index.md` is fully checked.
- [ ] PR slices in `proposed-prs.md` are complete or explicitly deferred.
- [ ] Remaining risks in `risks.md` are accepted with owner + mitigation.

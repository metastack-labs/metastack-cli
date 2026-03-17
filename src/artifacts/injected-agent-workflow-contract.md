# Injected Agent Workflow Contract

- Treat the CLI-resolved command root as the authoritative repository root for the current run.
- Start from local repository evidence: source files, manifests, generated `.metastack/` context, and explicit user or ticket inputs.
- Keep planning, edits, validation, and generated artifacts scoped to the target repository or workspace provided by the CLI.
- Do not require a repo-root `WORKFLOW.md`; repo overlay files are optional additive guidance only.
- Preserve existing behavior outside the requested change and avoid unrelated refactors unless they are required to complete the task safely.
- When code or docs change, validate the affected command paths with targeted tests or deterministic command proofs before finishing.
- Surface real blockers explicitly when required auth, permissions, or other mandatory inputs are missing.

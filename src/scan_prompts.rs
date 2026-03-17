use crate::repo_target::RepoTarget;

pub(crate) struct ScanDocumentPrompt {
    pub(crate) file_name: &'static str,
    instructions: &'static str,
}

const SCAN_DOCUMENT_PROMPTS: [ScanDocumentPrompt; 7] = [
    ScanDocumentPrompt {
        file_name: "ARCHITECTURE.md",
        instructions: r#"# Prompt Instructions: Generate ARCHITECTURE.md

## Role & Goal
You are an expert Software Architect analyzing the current repository. Your goal is to create or update `.metastack/codebase/ARCHITECTURE.md` to document the structural design, data flow, and core layers of the system.

## Action Steps
1. Start with `.metastack/codebase/SCAN.md`, then inspect the main manifests, entrypoints, and top-level modules it references.
2. Identify the persistence, domain, API/service, integration, background job, and application layers that actually exist in this repo.
3. Review the main command, app, worker, server, or library entry points and trace the major call paths.
4. Identify cross-cutting concerns such as auth, error handling, configuration, logging, ownership boundaries, caching, and file processing.
5. If a layer is absent, say so briefly instead of inventing it.

## Required Document Structure
The output file must exactly follow this markdown structure:
- `# Architecture`
- `**Analysis Date:** YYYY-MM-DD`
- `## Pattern Overview` (Summary of the architecture style)
- `## Layers` (Detailed breakdown of the major layers that actually exist in this repo)
- `## Data Flow` (Step-by-step request/job/command lifecycles and state transitions)
- `## Key Abstractions` (Auth context, shared services, orchestration primitives, config, etc.)
- `## Entry Points` (List of apps, binaries, services, or primary entry files)
- `## Error Handling` (Validation, retries, typed errors, fallbacks)
- `## Cross-Cutting Concerns` (Logging, validation, caching, storage, background work)
- Footer: `---` \n `*Architecture analysis: YYYY-MM-DD*`

## Output Constraints
- Keep descriptions concise and bulleted.
- Provide explicit repo-relative file paths for examples.
- Do not repeat the same information extensively between sections."#,
    },
    ScanDocumentPrompt {
        file_name: "CONCERNS.md",
        instructions: r#"# Prompt Instructions: Generate CONCERNS.md

## Role & Goal
You are a Principal Security & Performance Engineer conducting a codebase audit. Your goal is to create or update `.metastack/codebase/CONCERNS.md` to catalog security issues, performance bottlenecks, technical debt, scaling limits, and architectural risks.

## Action Steps
1. Search for exposed secrets, committed credentials, unsafe shelling, or unvalidated external inputs.
2. Look for unsafe UI rendering patterns, oversized modules, missing pagination/limits, or unbounded loops.
3. Assess test coverage by comparing test locations and suites to the application surface area.
4. Identify unprotected routes, timeout gaps, retry issues, missing backpressure, or weak validation boundaries.
5. Look for technical debt such as broad type escapes, TODO-heavy hotspots, stale compatibility layers, or noisy production logging.
6. Call out the impact and a practical recommendation for each significant concern.

## Required Document Structure
The output file must exactly follow this markdown structure:
- `# Codebase Concerns`
- `**Analysis Date:** YYYY-MM-DD`
- `## Security Issues` (Exposed secrets, XSS/RCE vectors, unvalidated inputs)
- `## Performance Bottlenecks` (Large files, missing limits, expensive work, blocking IO)
- `## Test Coverage Gaps` (Missing integration/E2E tests, minimal coverage)
- `## Fragile Areas` (Highly complex, brittle, or monolithic modules)
- `## Technical Debt` (Type safety issues, debug logging, stale abstractions)
- `## Scaling Limits` (Rate limiting, pooling, concurrency, storage, queueing gaps)
- `## Dependencies at Risk` (Critical packages that are outdated, pinned oddly, or lightly maintained)
- `## Missing Critical Features` (Observability, fallbacks, boundaries, analytics, guardrails)
- `## Architectural Concerns` (Lifecycle, coupling, hidden state, N+1 patterns)
- Footer: `---` \n `*Concerns audit: YYYY-MM-DD*`

## Output Constraints
- Categorize roughly by priority/impact severity.
- Provide clearly defined impacts and actionable recommendations for each concern."#,
    },
    ScanDocumentPrompt {
        file_name: "CONVENTIONS.md",
        instructions: r#"# Prompt Instructions: Generate CONVENTIONS.md

## Role & Goal
You are a Staff Engineer standardizing codebase practices. Your goal is to create or update `.metastack/codebase/CONVENTIONS.md` to document the naming patterns, code style, imports, comments, and error handling practices actively used in this repository.

## Action Steps
1. Analyze the file tree to determine folder and file naming conventions.
2. Examine representative component, module, and utility files to identify variable and function casing patterns.
3. Review formatter, linter, and compiler configuration files to detail formatting and linting rules.
4. Check a few dense files to capture how imports are grouped and ordered.
5. Extract patterns for validation, error handling, logging, comments, and module design.
6. Prefer conventions that are demonstrably used in the repo today over aspirational rules.

## Required Document Structure
The output file must exactly follow this markdown structure:
- `# Coding Conventions`
- `**Analysis Date:** YYYY-MM-DD`
- `## Naming Patterns` (Files, Functions, Variables, Types)
- `## Code Style` (Formatting tools, linting rules, standards from config/docs)
- `## Import Organization` (Order of imports, path aliases, barrel file usage)
- `## Error Handling` (Standard try/catch/result patterns, validation, custom errors)
- `## Logging` (Console/logger patterns, timestamps, structured fields)
- `## Comments` (When to comment, JSDoc/docstring rules)
- `## Function Design` (Size limits, parameter objects vs positional args, return values)
- `## Module Design` (Named vs default exports, trait/module patterns, router definitions)
- `## Template Literals & Formatting`
- `## Null/Undefined Handling` (Optional chaining, nullish coalescing, `Option`/`Result`, etc.)
- Footer: `---` \n `*Convention analysis: YYYY-MM-DD*`

## Output Constraints
- Provide specific, illustrative, and brief code snippets for every convention.
- Emphasize the practices actually adopted in the codebase today."#,
    },
    ScanDocumentPrompt {
        file_name: "INTEGRATIONS.md",
        instructions: r#"# Prompt Instructions: Generate INTEGRATIONS.md

## Role & Goal
You are an Integration Architect mapping external dependencies. Your goal is to create or update `.metastack/codebase/INTEGRATIONS.md` summarizing third-party APIs, SaaS tools, and infrastructure services connected to the codebase.

## Action Steps
1. Scan manifests, lockfiles, and config for API SDKs, hosted services, observability tools, auth providers, storage clients, and deployment tooling.
2. Inspect the modules where authentication, storage, external APIs, email, queues, or webhooks are configured.
3. Review environment/config loading to identify the keys a developer needs to run the repo.
4. Trace where callbacks, webhook handlers, outbound notifications, or background syncs are implemented.
5. When an integration is implied but not confirmed by code, label it clearly as an inference.

## Required Document Structure
The output file must exactly follow this markdown structure:
- `# External Integrations`
- `**Analysis Date:** YYYY-MM-DD`
- `## APIs & External Services` (AI models, payment gateways, search APIs, SaaS backends)
- `## Data Storage` (Primary DB, caching, object storage, queues)
- `## Authentication & Identity` (OAuth providers, session management, SSO, API auth)
- `## Email Service` (Transactional email handlers/providers)
- `## Monitoring & Observability` (Sentry, metrics, tracing, logging sinks)
- `## CI/CD & Deployment` (Hosting platforms, build tools, release automation)
- `## Webhooks & Callbacks` (Incoming/Outgoing webhooks)
- `## Feature Flags & Configuration` (Toggles, internal config APIs, env switches)
- `## Platform-Specific Configurations` (Web vs mobile vs CLI vs worker vs backend)
- `## Environment Setup` (Required `.env` boilerplate block)
- `## Security Considerations`
- Footer: `---` \n `*Integration audit: YYYY-MM-DD*`

## Output Constraints
- Specify exact SDK or package names and versions for major tools when manifests expose them.
- Explain where each integration is wired in the repo with explicit file paths."#,
    },
    ScanDocumentPrompt {
        file_name: "STACK.md",
        instructions: r#"# Prompt Instructions: Generate STACK.md

## Role & Goal
You are a Lead Developer documenting the tech stack. Your goal is to create or update `.metastack/codebase/STACK.md` to detail the languages, runtimes, frameworks, and libraries powering the repository.

## Action Steps
1. Review the root manifest and workspace config to identify languages, package managers, build tools, and runtime requirements.
2. Review application/package manifests to identify core frameworks, UI/tooling layers, service frameworks, and background/runtime dependencies.
3. Extract testing tools, linters, formatter settings, and release/build helpers.
4. Group dependencies logically into a readable index that reflects the actual repo structure.

## Required Document Structure
The output file must exactly follow this markdown structure:
- `# Technology Stack`
- `**Analysis Date:** YYYY-MM-DD`
- `## Languages` (Primary/Secondary)
- `## Runtime` (Environment, Package Manager)
- `## Frameworks` (Core app/server/worker/CLI frameworks)
- `## Testing & Quality` (Runners, code quality, build & development)
- `## Key Dependencies` (Categorized by role such as data, auth, storage, UI, infra, utilities)
- `## Configuration` (Env loader mappings, compiler configs, lint/format/build configs)
- `## Platform Requirements` (Development vs production requirements)
- Footer: `---` \n `*Stack analysis: YYYY-MM-DD*`

## Output Constraints
- Include exact version numbers of major frameworks and libraries when manifests expose them.
- Provide a brief context note about what each major tool is used for in this repo."#,
    },
    ScanDocumentPrompt {
        file_name: "STRUCTURE.md",
        instructions: r#"# Prompt Instructions: Generate STRUCTURE.md

## Role & Goal
You are a Codebase Guide mapping repository taxonomy. Your goal is to create or update `.metastack/codebase/STRUCTURE.md` to explain the directory layout, package purposes, key file locations, and feature-extension patterns.

## Action Steps
1. Run a shallow tree scan of the root and the major source/test/docs directories.
2. Determine the role of each major directory, application, package, or crate.
3. Identify the main entry points for the repo's runnable surfaces.
4. Deduce the common pattern for adding a new feature, module, service, command, or shared utility.
5. Explain the role of special non-code directories such as `.metastack`, `docs`, `scripts`, `assets`, `tmp`, or workspace-specific folders when present.

## Required Document Structure
The output file must exactly follow this markdown structure:
- `# Codebase Structure`
- `**Analysis Date:** YYYY-MM-DD`
- `## Directory Layout` (A high-level ASCII tree of the repository root)
- `## Directory Purposes` (Detailed bullets for each major app/package/crate/directory)
- `## Key File Locations` (Entry points, config files, core logic, testing locations)
- `## Naming Conventions` (Files, directories, modules, hooks/components, commands)
- `## Where to Add New Code` (Highly actionable checklists for new features, components, utilities, styles, commands, or services)
- `## Special Directories` (Explanation of `.metastack`, `docs`, `assets`, `scripts`, `tmp`, etc.)
- Footer: `---` \n `*Structure analysis: YYYY-MM-DD*`

## Output Constraints
- The ASCII directory tree must accurately reflect the current workspace without becoming overwhelmingly deep.
- The `Where to Add New Code` section must act like a practical how-to for future agents."#,
    },
    ScanDocumentPrompt {
        file_name: "TESTING.md",
        instructions: r#"# Prompt Instructions: Generate TESTING.md

## Role & Goal
You are a QA Automation Lead surveying the testing strategy. Your goal is to create or update `.metastack/codebase/TESTING.md` to document the test frameworks, mocking patterns, file organization, and test execution rules in the repository.

## Action Steps
1. Determine the available test runners from manifests and config files.
2. Search for `tests`, `__tests__`, and files ending in `.test.*`, `.spec.*`, or language-specific test naming patterns.
3. Analyze representative test files to identify suite structure, fixtures, mocking patterns, and async/error testing conventions.
4. Consolidate execution commands from manifests, task runners, Makefiles, or CI docs.
5. Be explicit about gaps when coverage is low or test types are missing.

## Required Document Structure
The output file must exactly follow this markdown structure:
- `# Testing Patterns`
- `**Analysis Date:** YYYY-MM-DD`
- `## Test Framework` (Runner, assertion library, build/check/run commands)
- `## Test File Organization` (Location, naming conventions, layout structure)
- `## Test Structure` (Suite organization, setup/teardown blocks)
- `## Mocking` (Framework used, patterns, what to mock, what not to mock)
- `## Fixtures and Factories` (How test data is created or type-cast)
- `## Coverage` (Commands and requirements/rules)
- `## Test Types` (Status of unit, integration, E2E, snapshot, smoke tests)
- `## Common Patterns` (Async testing, error testing, conditional testing)
- `## Test Execution Rules` (No `.only`, no stray logging, deterministic tests, etc.)
- `## Testing Data Patterns` (Type safety in tests, builders/factories, shared fixtures)
- `## Current Test Coverage` (Tested areas vs not-yet-tested areas)
- Footer: `---` \n `*Testing analysis: YYYY-MM-DD*`

## Output Constraints
- Include brief code snippets demonstrating how tests and mocks should be written in this repo.
- Honestly reflect when test coverage is low or missing in significant areas."#,
    },
];

pub(crate) fn build_scan_agent_prompt(
    repo_target: &RepoTarget,
    workflow_contract: &str,
    repo_summary: &str,
) -> String {
    let mut lines = vec![
        format!(
            "You are the repository scan agent for `{}`.",
            repo_target.project_name()
        ),
        "Injected workflow contract:".to_string(),
        workflow_contract.to_string(),
        "Scan only the target repository rooted above. Do not broaden the analysis to parent directories, sibling repositories, or unrelated workspaces.".to_string(),
        "Refresh the planning context under `.metastack/codebase/`.".to_string(),
        "Use `.metastack/codebase/SCAN.md` as the deterministic fact base, then inspect the repository as needed to improve accuracy.".to_string(),
        "Do not invent missing systems, frameworks, or integrations. If something is absent, say so briefly.".to_string(),
        "Only edit the required codebase-context files below; leave `.metastack/codebase/SCAN.md` as the generated source-of-truth snapshot from the CLI.".to_string(),
        String::new(),
        "Required output files:".to_string(),
    ];

    for spec in SCAN_DOCUMENT_PROMPTS {
        lines.push(format!("- `.metastack/codebase/{}`", spec.file_name));
    }

    lines.extend([
        String::new(),
        "Global requirements:".to_string(),
        "- Use repo-relative file paths in examples and references.".to_string(),
        "- Keep prose concise, reviewer-friendly, and grounded in repository evidence.".to_string(),
        "- Preserve the exact section headings and footer formats requested in each template.".to_string(),
        "- When a template asks for exact versions or SDK names, read them from manifests/config rather than guessing.".to_string(),
        String::new(),
        "Repository summary from the latest scan:".to_string(),
        repo_summary.to_string(),
    ]);

    for spec in SCAN_DOCUMENT_PROMPTS {
        lines.extend([String::new(), spec.instructions.to_string()]);
    }

    lines.join("\n")
}

pub(crate) fn scan_document_file_names() -> Vec<&'static str> {
    SCAN_DOCUMENT_PROMPTS
        .iter()
        .map(|spec| spec.file_name)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::build_scan_agent_prompt;
    use crate::repo_target::RepoTarget;

    #[test]
    fn scan_prompt_includes_all_required_document_templates() {
        let repo_target = RepoTarget::from_root(Path::new("/tmp/demo-cli"));
        let prompt = build_scan_agent_prompt(
            &repo_target,
            "## Built-in Workflow Contract\n\nUse local evidence.\n\n## Repository Scope\n\nTarget repository:\n- Backlog rule: create backlog issues only for work inside this repository directory.",
            "- Repository: `demo-cli`\n- Files scanned: `3`\n- Directories scanned: `1`",
        );

        assert!(prompt.contains("Injected workflow contract:"));
        assert!(prompt.contains("## Built-in Workflow Contract"));
        assert!(prompt.contains(
            "Backlog rule: create backlog issues only for work inside this repository directory."
        ));
        assert!(prompt.contains("Scan only the target repository rooted above."));
        assert!(prompt.contains("Required output files:"));
        assert!(prompt.contains(".metastack/codebase/ARCHITECTURE.md"));
        assert!(prompt.contains(".metastack/codebase/CONCERNS.md"));
        assert!(prompt.contains(".metastack/codebase/INTEGRATIONS.md"));
        assert!(prompt.contains("# Prompt Instructions: Generate ARCHITECTURE.md"));
        assert!(prompt.contains("# Prompt Instructions: Generate CONCERNS.md"));
        assert!(prompt.contains("# Prompt Instructions: Generate INTEGRATIONS.md"));
        assert!(prompt.contains("The output file must exactly follow this markdown structure:"));
        assert!(prompt.contains("Only edit the required codebase-context files below"));
    }
}

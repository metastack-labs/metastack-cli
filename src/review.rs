//! Shared review engine used by `meta backlog review` and `meta linear issues refine`.
//!
//! Provides the core multi-pass critique/rewrite loop, artifact recording,
//! multi-agent orchestration with fallback, and optional apply-back to local
//! backlog and Linear.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::macros::format_description;

use crate::agents::run_agent_capture;
use crate::backlog::{
    BacklogIssueMetadata, INDEX_FILE_NAME, ManagedFileRecord, save_issue_metadata,
    write_issue_description,
};
use crate::cli::{BacklogReviewArgs, RunAgentArgs};
use crate::config::{AGENT_ROUTE_BACKLOG_REVIEW, AppConfig};
use crate::fs::{
    PlanningPaths, canonicalize_existing_dir, display_path, ensure_dir, write_text_file,
};
use crate::linear::{IssueEditSpec, IssueSummary, LinearService, ReqwestLinearClient};
use crate::load_linear_command_context;
use crate::repo_target::RepoTarget;
use crate::scaffold::ensure_planning_layout;

// ---------------------------------------------------------------------------
// Public constants
// ---------------------------------------------------------------------------

const ORIGINAL_SNAPSHOT_FILE: &str = "original.md";
const ISSUE_SNAPSHOT_FILE: &str = "issue.json";
const LOCAL_INDEX_SNAPSHOT_FILE: &str = "local-index.md";
const FINAL_PROPOSED_FILE: &str = "final-proposed.md";
const SUMMARY_FILE: &str = "summary.json";

// ---------------------------------------------------------------------------
// CLI command entry point
// ---------------------------------------------------------------------------

/// Run `meta backlog review`, the Linear-first multi-agent review command.
///
/// 1. Resolves config defaults for agent chain, passes, and mode.
/// 2. For each issue, pulls the latest description from Linear.
/// 3. Runs the shared review engine with multi-agent support.
/// 4. On success (non-critique-only), updates local backlog and pushes to Linear.
pub(crate) async fn run_backlog_review_command(args: &BacklogReviewArgs) -> Result<String> {
    let root = canonicalize_existing_dir(&args.client.root)?;
    ensure_planning_layout(&root, false)?;
    let app_config = AppConfig::load()?;

    // Resolve defaults from config, allowing CLI overrides.
    let config_defaults = &app_config.backlog_review;
    let passes = if args.passes > 0 { args.passes } else { 1 };
    let passes = config_defaults.passes.unwrap_or(passes).max(1);
    let passes = if args.passes > 1 { args.passes } else { passes };

    let critique_only = args.critique_only
        || config_defaults
            .default_mode
            .as_deref()
            .is_some_and(|mode| mode.eq_ignore_ascii_case("critique"));

    let fallback_enabled = config_defaults
        .fallback_behavior
        .as_deref()
        .map(|behavior| !behavior.eq_ignore_ascii_case("fail"))
        .unwrap_or(true);

    let agent_chain = if !args.agents.is_empty() {
        args.agents.clone()
    } else if let Some(chain) = config_defaults.agent_chain.as_deref() {
        chain
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        Vec::new()
    };

    if passes == 0 {
        bail!("`meta backlog review` requires at least 1 pass");
    }

    let command_context = load_linear_command_context(&args.client, None)?;
    let mut reports = Vec::with_capacity(args.issues.len());

    for identifier in &args.issues {
        // Linear-first: pull latest issue state before review.
        let issue = command_context.service.load_issue(identifier).await?;

        // Write the latest Linear description to local backlog before reviewing.
        let issue_dir = PlanningPaths::new(&root).backlog_issue_dir(&issue.identifier);
        ensure_dir(&issue_dir)?;
        write_issue_description(
            &root,
            &issue.identifier,
            issue.description.as_deref().unwrap_or_default(),
        )?;

        let config = ReviewEngineConfig {
            root: root.clone(),
            artifact_kind: "review".to_string(),
            route_key: AGENT_ROUTE_BACKLOG_REVIEW.to_string(),
            critique_only,
            passes,
            agent_chain: agent_chain.clone(),
            fallback_enabled,
            agent: args.agent.clone(),
            model: args.model.clone(),
            reasoning: args.reasoning.clone(),
            prompt_pass_label: "Review".to_string(),
        };

        let report = review_issue(&config, Some(&command_context.service), &issue).await?;
        reports.push(report);
    }

    Ok(render_review_reports(&root, &reports))
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Configuration for a single review run.
#[derive(Debug, Clone)]
pub struct ReviewEngineConfig {
    /// Root path of the repository.
    pub root: PathBuf,
    /// Artifact subdirectory name under `artifacts/`, e.g. `"refinement"` or `"review"`.
    pub artifact_kind: String,
    /// Agent route key used for resolution, e.g. `"linear.issues.refine"` or `"backlog.review"`.
    pub route_key: String,
    /// When `true`, write artifacts only and do not mutate local backlog or Linear.
    pub critique_only: bool,
    /// Total number of review passes to run.
    pub passes: usize,
    /// Ordered agent chain for multi-agent orchestration, e.g. `["codex", "claude", "codex"]`.
    /// When empty, the default agent resolution path is used for every pass.
    pub agent_chain: Vec<String>,
    /// When `true`, a failed agent pass falls back to the next available agent.
    pub fallback_enabled: bool,
    /// Explicit single-agent override (takes precedence over chain for every pass).
    pub agent: Option<String>,
    /// Model override for every pass.
    pub model: Option<String>,
    /// Reasoning override for every pass.
    pub reasoning: Option<String>,
    /// Label used in the agent prompt to identify the pass type, e.g. `"Refinement"` or `"Review"`.
    pub prompt_pass_label: String,
}

/// Structured output returned by the review agent for each pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPassOutput {
    pub summary: String,
    #[serde(default)]
    pub findings: StructuredFindings,
    #[serde(alias = "proposed_description", alias = "proposed_rewrite")]
    pub rewrite: String,
}

/// Categorized findings from a single review pass.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructuredFindings {
    #[serde(default)]
    pub missing_requirements: Vec<String>,
    #[serde(default)]
    pub unclear_scope: Vec<String>,
    #[serde(default)]
    pub validation_gaps: Vec<String>,
    #[serde(default)]
    pub dependency_risks: Vec<String>,
    #[serde(default)]
    pub follow_up_ideas: Vec<String>,
}

/// Metadata for a single review pass stored in the run summary.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewPassRecord {
    pub pass_number: usize,
    pub summary: String,
    pub findings_json_path: String,
    pub findings_markdown_path: String,
    pub rewrite_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_record: Option<ReviewAgentRecord>,
}

/// Records which agent was requested vs. actually used for a pass.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewAgentRecord {
    pub requested_agent: Option<String>,
    pub actual_agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

/// Records the outcome of the apply phase.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewApplyRecord {
    pub requested: bool,
    pub local_updated: bool,
    pub remote_updated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_before_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_after_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_before_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_after_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Full summary of a review run, written to `summary.json`.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewRunSummary {
    pub run_id: String,
    pub issue_identifier: String,
    pub issue_title: String,
    pub critique_only: bool,
    pub passes_requested: usize,
    pub started_at: String,
    pub completed_at: String,
    pub original_snapshot_path: String,
    pub issue_snapshot_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_index_snapshot_path: Option<String>,
    pub final_proposed_path: String,
    pub passes: Vec<ReviewPassRecord>,
    pub apply: ReviewApplyRecord,
}

/// Result of reviewing a single issue, returned to the caller.
#[derive(Debug, Clone)]
pub struct ReviewReport {
    pub issue_identifier: String,
    pub run_dir: PathBuf,
    pub apply_requested: bool,
}

// ---------------------------------------------------------------------------
// Core engine
// ---------------------------------------------------------------------------

/// Run the full review pipeline for a single issue.
///
/// This is the shared entry point used by both `meta linear issues refine`
/// and `meta backlog review`. The `config` controls artifact naming,
/// agent resolution, multi-agent chain, and apply behavior.
pub async fn review_issue(
    config: &ReviewEngineConfig,
    service: Option<&LinearService<ReqwestLinearClient>>,
    issue: &IssueSummary,
) -> Result<ReviewReport> {
    let started_at = now_rfc3339()?;
    let run_id = review_run_id()?;
    let root = &config.root;
    let paths = PlanningPaths::new(root);
    let issue_dir = paths.backlog_issue_dir(&issue.identifier);
    ensure_dir(&issue_dir)?;
    save_issue_metadata(&issue_dir, &build_issue_metadata(issue))?;

    let run_dir = issue_dir
        .join("artifacts")
        .join(&config.artifact_kind)
        .join(&run_id);
    ensure_dir(&run_dir)?;

    // Save original snapshots.
    let original_description = issue.description.clone().unwrap_or_default();
    let original_snapshot_path = run_dir.join(ORIGINAL_SNAPSHOT_FILE);
    write_text_file(&original_snapshot_path, &original_description, true)?;

    let issue_snapshot_path = run_dir.join(ISSUE_SNAPSHOT_FILE);
    write_text_file(
        &issue_snapshot_path,
        &serde_json::to_string_pretty(issue).context("failed to encode issue snapshot")?,
        true,
    )?;

    let local_index_path = issue_dir.join(INDEX_FILE_NAME);
    let local_index_before = match fs::read_to_string(&local_index_path) {
        Ok(contents) => Some(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read `{}`", local_index_path.display()));
        }
    };
    let local_index_snapshot_path = if let Some(contents) = local_index_before.as_deref() {
        let path = run_dir.join(LOCAL_INDEX_SNAPSHOT_FILE);
        write_text_file(&path, contents, true)?;
        Some(path)
    } else {
        None
    };

    // Run passes.
    let mut pass_records = Vec::with_capacity(config.passes);
    let mut previous_pass = None;

    for pass_number in 1..=config.passes {
        let (parsed, agent_record) = run_review_pass(
            config,
            root,
            issue,
            local_index_before.as_deref(),
            pass_number,
            previous_pass.as_ref(),
        )?;

        let normalized = normalize_pass_output(parsed)?;
        let record = save_pass_artifacts(root, &run_dir, pass_number, &normalized, agent_record)?;

        pass_records.push(record);
        previous_pass = Some(normalized);
    }

    let final_pass = previous_pass.ok_or_else(|| anyhow!("no review pass was produced"))?;
    let final_proposed_path = run_dir.join(FINAL_PROPOSED_FILE);
    write_text_file(&final_proposed_path, &final_pass.rewrite, true)?;

    // Apply phase.
    let apply = apply_review_result(
        config,
        root,
        &run_dir,
        service,
        issue,
        &final_pass,
        &original_description,
        local_index_before.as_deref(),
    )
    .await?;

    // Write run summary.
    let completed_at = now_rfc3339()?;
    let summary = ReviewRunSummary {
        run_id: run_id.clone(),
        issue_identifier: issue.identifier.clone(),
        issue_title: issue.title.clone(),
        critique_only: config.critique_only,
        passes_requested: config.passes,
        started_at,
        completed_at,
        original_snapshot_path: display_path(&original_snapshot_path, root),
        issue_snapshot_path: display_path(&issue_snapshot_path, root),
        local_index_snapshot_path: local_index_snapshot_path
            .as_ref()
            .map(|path| display_path(path, root)),
        final_proposed_path: display_path(&final_proposed_path, root),
        passes: pass_records,
        apply,
    };

    let summary_path = run_dir.join(SUMMARY_FILE);
    write_text_file(
        &summary_path,
        &serde_json::to_string_pretty(&summary).context("failed to encode review summary")?,
        true,
    )?;

    if let Some(error) = summary.apply.error.as_deref() {
        bail!(
            "reviewed `{}` but failed to finish the apply-back flow: {}",
            issue.identifier,
            error
        );
    }

    Ok(ReviewReport {
        issue_identifier: issue.identifier.clone(),
        run_dir,
        apply_requested: !config.critique_only,
    })
}

/// Render a human-readable summary of review reports.
pub fn render_review_reports(root: &Path, reports: &[ReviewReport]) -> String {
    let mut lines = vec![format!("Reviewed {} issue(s):", reports.len())];

    for report in reports {
        let mode = if report.apply_requested {
            "applied"
        } else {
            "critique-only"
        };
        lines.push(format!(
            "- {}: {} ({})",
            report.issue_identifier,
            mode,
            display_path(&report.run_dir, root)
        ));
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Multi-agent orchestration
// ---------------------------------------------------------------------------

/// Run a single review pass, with optional multi-agent fallback.
///
/// Returns the parsed output and an optional agent record. When `config.agent`
/// is set it takes precedence. When `config.agent_chain` is non-empty, the
/// chain index for the pass determines the requested agent. If that agent
/// fails and `config.fallback_enabled` is `true`, the remaining chain entries
/// and built-in providers are tried in order.
fn run_review_pass(
    config: &ReviewEngineConfig,
    root: &Path,
    issue: &IssueSummary,
    local_index_snapshot: Option<&str>,
    pass_number: usize,
    previous_pass: Option<&ReviewPassOutput>,
) -> Result<(ReviewPassOutput, Option<ReviewAgentRecord>)> {
    let prompt = render_review_prompt(
        root,
        issue,
        local_index_snapshot,
        pass_number,
        config.passes,
        previous_pass,
        &config.prompt_pass_label,
    )?;

    // Determine the requested agent for this pass.
    let requested_agent = config.agent.clone().or_else(|| {
        if config.agent_chain.is_empty() {
            None
        } else {
            let index = (pass_number - 1) % config.agent_chain.len();
            Some(config.agent_chain[index].clone())
        }
    });

    // Try the requested agent first.
    let base_args = RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(config.route_key.clone()),
        agent: requested_agent.clone(),
        prompt: prompt.clone(),
        instructions: None,
        model: config.model.clone(),
        reasoning: config.reasoning.clone(),
        transport: None,
    };

    match run_agent_capture(&base_args) {
        Ok(output) => {
            let parsed: ReviewPassOutput =
                parse_agent_json(&output.stdout, "review critique/rewrite")?;
            let agent_record = requested_agent.as_ref().map(|requested| ReviewAgentRecord {
                requested_agent: Some(requested.clone()),
                actual_agent: requested.clone(),
                fallback_reason: None,
            });
            Ok((parsed, agent_record))
        }
        Err(primary_error) if config.fallback_enabled && !config.agent_chain.is_empty() => {
            // Try fallback agents.
            let primary_error_msg = primary_error.to_string();
            let fallback_candidates = build_fallback_candidates(config, requested_agent.as_deref());

            for candidate in &fallback_candidates {
                let fallback_args = RunAgentArgs {
                    agent: Some(candidate.clone()),
                    prompt: prompt.clone(),
                    ..base_args.clone()
                };
                match run_agent_capture(&fallback_args) {
                    Ok(output) => {
                        let parsed: ReviewPassOutput =
                            parse_agent_json(&output.stdout, "review critique/rewrite (fallback)")?;
                        let agent_record = Some(ReviewAgentRecord {
                            requested_agent: requested_agent.clone(),
                            actual_agent: candidate.clone(),
                            fallback_reason: Some(primary_error_msg),
                        });
                        return Ok((parsed, agent_record));
                    }
                    Err(_) => continue,
                }
            }

            bail!(
                "all configured agents failed for pass {pass_number}: primary agent error: {primary_error_msg}"
            )
        }
        Err(error) => Err(error).with_context(
            || "review requires a configured local agent to critique and rewrite issues",
        ),
    }
}

/// Build the list of fallback candidates, excluding the already-tried requested agent.
fn build_fallback_candidates(config: &ReviewEngineConfig, requested: Option<&str>) -> Vec<String> {
    let mut candidates = Vec::new();

    // Add remaining chain entries first.
    for agent in &config.agent_chain {
        let normalized = agent.trim().to_lowercase();
        if Some(normalized.as_str()) != requested.map(|s| s.trim().to_lowercase()).as_deref()
            && !candidates.contains(&normalized)
        {
            candidates.push(normalized);
        }
    }

    // Add built-in providers as last resort.
    for builtin in &["codex", "claude"] {
        let name = (*builtin).to_string();
        if Some(name.as_str()) != requested && !candidates.contains(&name) {
            candidates.push(name);
        }
    }

    candidates
}

// ---------------------------------------------------------------------------
// Apply logic
// ---------------------------------------------------------------------------

/// Apply the final review result to local backlog and optionally to Linear.
#[allow(clippy::too_many_arguments)]
async fn apply_review_result(
    config: &ReviewEngineConfig,
    root: &Path,
    run_dir: &Path,
    service: Option<&LinearService<ReqwestLinearClient>>,
    issue: &IssueSummary,
    final_pass: &ReviewPassOutput,
    original_description: &str,
    local_index_before: Option<&str>,
) -> Result<ReviewApplyRecord> {
    let mut apply = ReviewApplyRecord {
        requested: !config.critique_only,
        local_updated: false,
        remote_updated: false,
        local_before_path: None,
        local_after_path: None,
        remote_before_path: None,
        remote_after_path: None,
        error: None,
    };

    if config.critique_only {
        return Ok(apply);
    }

    // Check listen guard.
    if let Err(error) = guard_listen_apply(&issue.identifier) {
        apply.error = Some(error.to_string());
        return Ok(apply);
    }

    let local_before_path = run_dir.join("applied-local-before.md");
    let local_after_path = run_dir.join("applied-local-after.md");
    let remote_before_path = run_dir.join("applied-remote-before.md");
    let remote_after_path = run_dir.join("applied-remote-after.md");

    write_text_file(
        &local_before_path,
        local_index_before.unwrap_or_default(),
        true,
    )?;
    write_text_file(&local_after_path, &final_pass.rewrite, true)?;
    write_text_file(&remote_before_path, original_description, true)?;
    write_text_file(&remote_after_path, &final_pass.rewrite, true)?;

    apply.local_before_path = Some(display_path(&local_before_path, root));
    apply.local_after_path = Some(display_path(&local_after_path, root));
    apply.remote_before_path = Some(display_path(&remote_before_path, root));
    apply.remote_after_path = Some(display_path(&remote_after_path, root));

    // Update local backlog.
    write_issue_description(root, &issue.identifier, &final_pass.rewrite)?;
    apply.local_updated = true;

    // Push to Linear when a service is available.
    if let Some(service) = service {
        if let Err(error) = service
            .edit_issue(IssueEditSpec {
                identifier: issue.identifier.clone(),
                title: None,
                description: Some(final_pass.rewrite.clone()),
                project: None,
                state: None,
                priority: None,
            })
            .await
        {
            apply.error = Some(error.to_string());
        } else {
            apply.remote_updated = true;
        }
    }

    Ok(apply)
}

/// Prevent apply from overwriting the active issue description during `meta listen`.
fn guard_listen_apply(identifier: &str) -> Result<()> {
    let unattended = std::env::var("METASTACK_LISTEN_UNATTENDED")
        .ok()
        .is_some_and(|value| value == "1");
    let active_issue = std::env::var("METASTACK_LINEAR_ISSUE_IDENTIFIER").ok();

    if unattended
        && active_issue
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(identifier))
    {
        bail!(
            "apply is disabled during `meta listen` because it would overwrite the primary Linear issue description; update the workpad comment instead"
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Artifact helpers
// ---------------------------------------------------------------------------

/// Save per-pass artifacts and return the pass record for the run summary.
fn save_pass_artifacts(
    root: &Path,
    run_dir: &Path,
    pass_number: usize,
    output: &ReviewPassOutput,
    agent_record: Option<ReviewAgentRecord>,
) -> Result<ReviewPassRecord> {
    let pass_json_path = run_dir.join(format!("pass-{pass_number:02}.json"));
    write_text_file(
        &pass_json_path,
        &serde_json::to_string_pretty(output).context("failed to encode review pass artifact")?,
        true,
    )?;

    let pass_findings_path = run_dir.join(format!("pass-{pass_number:02}-findings.md"));
    write_text_file(
        &pass_findings_path,
        &render_findings_markdown(pass_number, output),
        true,
    )?;

    let pass_rewrite_path = run_dir.join(format!("pass-{pass_number:02}-rewrite.md"));
    write_text_file(&pass_rewrite_path, &output.rewrite, true)?;

    if let Some(record) = &agent_record {
        let agent_path = run_dir.join(format!("pass-{pass_number:02}-agent.json"));
        write_text_file(
            &agent_path,
            &serde_json::to_string_pretty(record)
                .context("failed to encode agent record artifact")?,
            true,
        )?;
    }

    Ok(ReviewPassRecord {
        pass_number,
        summary: output.summary.clone(),
        findings_json_path: display_path(&pass_json_path, root),
        findings_markdown_path: display_path(&pass_findings_path, root),
        rewrite_path: display_path(&pass_rewrite_path, root),
        agent_record,
    })
}

// ---------------------------------------------------------------------------
// Prompt rendering
// ---------------------------------------------------------------------------

/// Render the review prompt for a single pass.
pub fn render_review_prompt(
    root: &Path,
    issue: &IssueSummary,
    local_index_snapshot: Option<&str>,
    pass_number: usize,
    total_passes: usize,
    previous_pass: Option<&ReviewPassOutput>,
    pass_label: &str,
) -> Result<String> {
    let repo_target = RepoTarget::from_root(root);
    let planning_context = load_context_bundle(root)?;
    let current_description = issue
        .description
        .as_deref()
        .unwrap_or("_No Linear description was provided._");
    let local_backlog_block = local_index_snapshot
        .map(str::trim)
        .filter(|contents| !contents.is_empty())
        .map(|contents| render_fenced_block("md", contents))
        .unwrap_or_else(|| "_No local backlog packet exists yet for this issue._".to_string());
    let previous_pass_block = previous_pass
        .map(|pass| {
            serde_json::to_string_pretty(pass)
                .unwrap_or_else(|_| "{\"summary\":\"Previous pass could not be rendered\"}".into())
        })
        .map(|json| render_fenced_block("json", &json))
        .unwrap_or_else(|| "_No previous review pass exists yet._".to_string());

    Ok(format!(
        "You are reviewing an existing Linear issue for the active repository.\n\n\
Repository scope:\n{repo_scope}\n\n\
Issue metadata:\n\
- Identifier: `{identifier}`\n\
- Title: {title}\n\
- Team: {team}\n\
- Project: {project}\n\
- State: {state}\n\
- URL: {url}\n\
- {pass_label} pass: {pass_number} of {total_passes}\n\n\
Current Linear description:\n{current_description_block}\n\n\
Current local backlog index snapshot:\n{local_backlog_block}\n\n\
Previous review pass output:\n{previous_pass_block}\n\n\
Repository planning context:\n{planning_context}\n\n\
Instructions:\n\
1. Critique the current issue quality for this repository only.\n\
2. Produce structured findings in these exact categories: `missing_requirements`, `unclear_scope`, `validation_gaps`, `dependency_risks`, and `follow_up_ideas`.\n\
3. Rewrite the full issue description as polished Markdown ready to save into `.metastack/backlog/<ISSUE>/index.md` and, when explicitly approved, back into Linear.\n\
4. Keep the rewrite consistent with the configured repository scope. Do not invent a second storage model or work outside this repository.\n\
5. When the issue changes CLI behavior, include concrete validation commands or command-path checks in the rewrite.\n\
6. Use the previous pass as critique input when present, but improve it if you find gaps or ambiguity.\n\
7. Return JSON only using this exact shape:\n\
{{\n  \"summary\": \"One paragraph explaining the main quality changes\",\n  \"findings\": {{\n    \"missing_requirements\": [\"...\"],\n    \"unclear_scope\": [\"...\"],\n    \"validation_gaps\": [\"...\"],\n    \"dependency_risks\": [\"...\"],\n    \"follow_up_ideas\": [\"...\"]\n  }},\n  \"rewrite\": \"# Improved issue markdown...\"\n}}",
        repo_scope = repo_target.prompt_scope_block(),
        identifier = issue.identifier,
        title = issue.title,
        team = issue.team.key,
        current_description_block = render_fenced_block("md", current_description),
        project = issue
            .project
            .as_ref()
            .map(|project| project.name.as_str())
            .unwrap_or("No project"),
        state = issue
            .state
            .as_ref()
            .map(|state| state.name.as_str())
            .unwrap_or("Unknown"),
        url = issue.url,
    ))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Parse agent JSON output, tolerating code fences and leading/trailing text.
pub fn parse_agent_json<T>(raw: &str, phase: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let trimmed = raw.trim();
    let mut candidates = vec![trimmed.to_string()];

    if let Some(stripped) = strip_code_fence(trimmed) {
        candidates.push(stripped);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && start <= end
    {
        candidates.push(trimmed[start..=end].to_string());
    }

    for candidate in candidates {
        if let Ok(parsed) = serde_json::from_str::<T>(&candidate) {
            return Ok(parsed);
        }
    }

    bail!(
        "review agent returned invalid JSON during {phase}: {}",
        preview_text(trimmed)
    )
}

/// Render a fenced markdown block with dynamic fence length.
pub fn render_fenced_block(language: &str, contents: &str) -> String {
    let fence_len = max_backtick_run(contents).saturating_add(1).max(3);
    let fence = "`".repeat(fence_len);
    if language.is_empty() {
        format!("{fence}\n{contents}\n{fence}")
    } else {
        format!("{fence}{language}\n{contents}\n{fence}")
    }
}

/// Format the current UTC time as an RFC 3339 string.
pub fn now_rfc3339() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
        ))
        .context("failed to format the review timestamp")
}

/// Generate a unique run ID based on the current UTC time.
pub fn review_run_id() -> Result<String> {
    let now = OffsetDateTime::now_utc();
    let base = now
        .format(&format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .context("failed to format the review run id")?;
    Ok(format!("{}-{:09}", base, now.nanosecond()))
}

/// Normalize and validate the output of a single review pass.
pub fn normalize_pass_output(output: ReviewPassOutput) -> Result<ReviewPassOutput> {
    let rewrite = output.rewrite.trim().replace("\r\n", "\n");
    if rewrite.is_empty() {
        bail!("review agent returned an empty proposed rewrite");
    }

    Ok(ReviewPassOutput {
        summary: trimmed_or_default(output.summary, "No summary provided."),
        findings: StructuredFindings {
            missing_requirements: normalize_findings(output.findings.missing_requirements),
            unclear_scope: normalize_findings(output.findings.unclear_scope),
            validation_gaps: normalize_findings(output.findings.validation_gaps),
            dependency_risks: normalize_findings(output.findings.dependency_risks),
            follow_up_ideas: normalize_findings(output.findings.follow_up_ideas),
        },
        rewrite,
    })
}

/// Render findings for a single pass as a markdown document.
pub fn render_findings_markdown(pass_number: usize, output: &ReviewPassOutput) -> String {
    let mut lines = vec![
        format!("# Review Pass {pass_number}"),
        String::new(),
        "## Summary".to_string(),
        String::new(),
        output.summary.clone(),
        String::new(),
    ];

    for (title, values) in [
        (
            "Missing Requirements",
            &output.findings.missing_requirements,
        ),
        ("Unclear Scope", &output.findings.unclear_scope),
        ("Validation Gaps", &output.findings.validation_gaps),
        ("Dependency Risks", &output.findings.dependency_risks),
        ("Follow-up Ideas", &output.findings.follow_up_ideas),
    ] {
        lines.push(format!("## {title}"));
        lines.push(String::new());
        if values.is_empty() {
            lines.push("- None identified.".to_string());
        } else {
            lines.extend(values.iter().map(|value| format!("- {value}")));
        }
        lines.push(String::new());
    }

    lines.push("## Proposed Rewrite".to_string());
    lines.push(String::new());
    lines.push(output.rewrite.clone());
    lines.join("\n")
}

/// Build issue metadata for the local backlog directory.
pub fn build_issue_metadata(issue: &IssueSummary) -> BacklogIssueMetadata {
    BacklogIssueMetadata {
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        url: issue.url.clone(),
        team_key: issue.team.key.clone(),
        project_id: issue.project.as_ref().map(|project| project.id.clone()),
        project_name: issue.project.as_ref().map(|project| project.name.clone()),
        parent_id: issue.parent.as_ref().map(|parent| parent.id.clone()),
        parent_identifier: issue
            .parent
            .as_ref()
            .map(|parent| parent.identifier.clone()),
        managed_files: Vec::<ManagedFileRecord>::new(),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn normalize_findings(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn trimmed_or_default(value: String, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

fn max_backtick_run(value: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;

    for ch in value.chars() {
        if ch == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }

    longest
}

fn strip_code_fence(raw: &str) -> Option<String> {
    let stripped = raw.strip_prefix("```")?;
    let stripped = stripped
        .strip_prefix("json\n")
        .or_else(|| stripped.strip_prefix("JSON\n"))
        .or_else(|| stripped.strip_prefix('\n'))
        .unwrap_or(stripped);
    let stripped = stripped.strip_suffix("```")?;
    Some(stripped.trim().to_string())
}

fn preview_text(value: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 240;
    let Some((truncate_at, _)) = value.char_indices().nth(MAX_PREVIEW_CHARS) else {
        return value.to_string();
    };

    format!("{}...", &value[..truncate_at])
}

fn load_context_bundle(root: &Path) -> Result<String> {
    let paths = PlanningPaths::new(root);
    let sections = [
        ("SCAN.md", paths.scan_path()),
        ("ARCHITECTURE.md", paths.architecture_path()),
        ("CONVENTIONS.md", paths.conventions_path()),
        ("STACK.md", paths.stack_path()),
        ("TESTING.md", paths.testing_path()),
    ];
    let mut lines = Vec::new();

    for (title, path) in sections {
        lines.push(format!("## {title}"));
        lines.push(String::new());
        lines.push(read_context(&path)?);
        lines.push(String::new());
    }

    Ok(lines.join("\n"))
}

fn read_context(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(format!(
            "_Missing `{}`. Run `meta scan` to generate it._",
            path.file_name()
                .map(|value| value.to_string_lossy())
                .unwrap_or_default()
        )),
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::{IssueLink, ProjectRef, TeamRef, WorkflowState};

    #[test]
    fn render_fenced_block_expands_when_contents_include_triple_backticks() {
        let block = render_fenced_block("md", "```\ncargo test\n```");

        assert!(block.starts_with("````md\n"));
        assert!(block.ends_with("\n````"));
    }

    #[test]
    fn render_review_prompt_uses_safe_fences_for_existing_markdown_code_blocks() {
        let temp = tempfile::tempdir().expect("temp dir");
        let root = temp.path();
        let paths = PlanningPaths::new(root);
        write_text_file(&paths.scan_path(), "scan", true).expect("scan context");
        write_text_file(&paths.architecture_path(), "architecture", true).expect("architecture");
        write_text_file(&paths.conventions_path(), "conventions", true).expect("conventions");
        write_text_file(&paths.stack_path(), "stack", true).expect("stack");
        write_text_file(&paths.testing_path(), "testing", true).expect("testing");

        let issue = IssueSummary {
            id: "issue-1".to_string(),
            identifier: "MET-148".to_string(),
            title: "Review prompt fences".to_string(),
            description: Some("```bash\ncargo test\n```".to_string()),
            url: "https://linear.example/MET-148".to_string(),
            priority: None,
            estimate: None,
            updated_at: "2026-03-15T00:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "MET".to_string(),
                name: "Meta".to_string(),
            },
            project: Some(ProjectRef {
                id: "project-1".to_string(),
                name: "MetaStack CLI".to_string(),
            }),
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: Some(WorkflowState {
                id: "state-1".to_string(),
                name: "Todo".to_string(),
                kind: Some("unstarted".to_string()),
            }),
            attachments: Vec::new(),
            parent: Some(IssueLink {
                id: "parent-1".to_string(),
                identifier: "MET-143".to_string(),
                title: "Parent".to_string(),
                url: "https://linear.example/MET-143".to_string(),
            }),
            children: Vec::new(),
        };
        let previous_pass = ReviewPassOutput {
            summary: "summary".to_string(),
            findings: StructuredFindings::default(),
            rewrite: "```elixir\nmix test\n```".to_string(),
        };

        let prompt = render_review_prompt(
            root,
            &issue,
            Some("```md\n## Local\n```"),
            2,
            2,
            Some(&previous_pass),
            "Review",
        )
        .expect("prompt");

        assert!(prompt.contains("Current Linear description:\n````md\n```bash"));
        assert!(prompt.contains("Current local backlog index snapshot:\n````md\n```md"));
        assert!(prompt.contains("Previous review pass output:\n````json\n{"));
        assert!(prompt.contains("\"rewrite\": \"```elixir\\nmix test\\n```\""));
    }

    #[test]
    fn preview_text_truncates_on_char_boundary() {
        let preview = preview_text(&"é".repeat(241));

        assert_eq!(preview, format!("{}...", "é".repeat(240)));
    }

    #[test]
    fn build_fallback_candidates_excludes_requested_agent() {
        let config = ReviewEngineConfig {
            root: PathBuf::new(),
            artifact_kind: "review".to_string(),
            route_key: "backlog.review".to_string(),
            critique_only: false,
            passes: 3,
            agent_chain: vec!["codex".to_string(), "claude".to_string()],
            fallback_enabled: true,
            agent: None,
            model: None,
            reasoning: None,
            prompt_pass_label: "Review".to_string(),
        };

        let candidates = build_fallback_candidates(&config, Some("codex"));
        assert_eq!(candidates, vec!["claude"]);
    }

    #[test]
    fn build_fallback_candidates_adds_builtins_when_chain_is_exhausted() {
        let config = ReviewEngineConfig {
            root: PathBuf::new(),
            artifact_kind: "review".to_string(),
            route_key: "backlog.review".to_string(),
            critique_only: false,
            passes: 1,
            agent_chain: vec!["custom-agent".to_string()],
            fallback_enabled: true,
            agent: None,
            model: None,
            reasoning: None,
            prompt_pass_label: "Review".to_string(),
        };

        let candidates = build_fallback_candidates(&config, Some("custom-agent"));
        assert!(candidates.contains(&"codex".to_string()));
        assert!(candidates.contains(&"claude".to_string()));
    }
}

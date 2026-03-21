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
use crate::cli::{IssueRefineArgs, LinearClientArgs, RunAgentArgs};
use crate::config::AGENT_ROUTE_LINEAR_ISSUES_REFINE;
use crate::fs::{
    PlanningPaths, canonicalize_existing_dir, display_path, ensure_dir, write_text_file,
};
use crate::repo_target::RepoTarget;
use crate::scaffold::ensure_planning_layout;

use super::command::load_linear_command_context;
use super::{IssueEditSpec, IssueSummary, LinearService, ReqwestLinearClient};

const ORIGINAL_SNAPSHOT_FILE: &str = "original.md";
const ISSUE_SNAPSHOT_FILE: &str = "issue.json";
const LOCAL_INDEX_SNAPSHOT_FILE: &str = "local-index.md";
const FINAL_PROPOSED_FILE: &str = "final-proposed.md";
const SUMMARY_FILE: &str = "summary.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RefinementPassOutput {
    summary: String,
    #[serde(default)]
    findings: StructuredFindings,
    #[serde(alias = "proposed_description", alias = "proposed_rewrite")]
    rewrite: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StructuredFindings {
    #[serde(default)]
    missing_requirements: Vec<String>,
    #[serde(default)]
    unclear_scope: Vec<String>,
    #[serde(default)]
    validation_gaps: Vec<String>,
    #[serde(default)]
    dependency_risks: Vec<String>,
    #[serde(default)]
    follow_up_ideas: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RefinementPassRecord {
    pass_number: usize,
    summary: String,
    findings_json_path: String,
    findings_markdown_path: String,
    rewrite_path: String,
}

#[derive(Debug, Clone, Serialize)]
struct RefinementApplyRecord {
    requested: bool,
    local_updated: bool,
    remote_updated: bool,
    local_before_path: Option<String>,
    local_after_path: Option<String>,
    remote_before_path: Option<String>,
    remote_after_path: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RefinementRunSummary {
    run_id: String,
    issue_identifier: String,
    issue_title: String,
    critique_only: bool,
    passes_requested: usize,
    started_at: String,
    completed_at: String,
    original_snapshot_path: String,
    issue_snapshot_path: String,
    local_index_snapshot_path: Option<String>,
    final_proposed_path: String,
    passes: Vec<RefinementPassRecord>,
    apply: RefinementApplyRecord,
}

#[derive(Debug, Clone)]
struct RefinementReport {
    issue_identifier: String,
    run_dir: PathBuf,
    apply_requested: bool,
}

pub(crate) async fn run_issue_refine_command(
    client_args: &LinearClientArgs,
    cli_default_team: Option<String>,
    args: IssueRefineArgs,
) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    ensure_planning_layout(&root, false)?;
    if args.passes == 0 {
        bail!("`meta issues refine` requires `--passes` to be at least 1");
    }
    let command_context = load_linear_command_context(client_args, cli_default_team)?;
    let mut reports = Vec::with_capacity(args.issues.len());

    for identifier in &args.issues {
        let issue = command_context.service.load_issue(identifier).await?;
        validate_issue_scope(
            &issue,
            command_context.default_team.as_deref(),
            command_context.default_project_id.as_deref(),
        )?;
        let report = refine_issue(&root, &command_context.service, &issue, &args).await?;
        reports.push(report);
    }

    println!("{}", render_refinement_reports(&root, &reports));
    Ok(())
}

async fn refine_issue(
    root: &Path,
    service: &LinearService<ReqwestLinearClient>,
    issue: &IssueSummary,
    args: &IssueRefineArgs,
) -> Result<RefinementReport> {
    let started_at = now_rfc3339()?;
    let run_id = refinement_run_id()?;
    let paths = PlanningPaths::new(root);
    let issue_dir = paths.backlog_issue_dir(&issue.identifier);
    ensure_dir(&issue_dir)?;
    save_issue_metadata(&issue_dir, &build_issue_metadata(issue))?;

    let run_dir = issue_dir.join("artifacts").join("refinement").join(&run_id);
    ensure_dir(&run_dir)?;

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

    let mut pass_records = Vec::with_capacity(args.passes);
    let mut previous_pass = None;

    for pass_number in 1..=args.passes {
        let prompt = render_refinement_prompt(
            root,
            issue,
            local_index_before.as_deref(),
            pass_number,
            args.passes,
            previous_pass.as_ref(),
        )?;
        let output = run_agent_capture(&RunAgentArgs {
            root: Some(root.to_path_buf()),
            route_key: Some(AGENT_ROUTE_LINEAR_ISSUES_REFINE.to_string()),
            agent: args.agent.clone(),
            prompt,
            instructions: None,
            model: args.model.clone(),
            reasoning: args.reasoning.clone(),
            transport: None,
            attachments: Vec::new(),
        })
        .with_context(|| {
            "meta issues refine requires a configured local agent to critique and rewrite existing issues"
        })?;
        let parsed: RefinementPassOutput =
            parse_agent_json(&output.stdout, "issue refinement critique/rewrite")?;
        let normalized = normalize_pass_output(parsed)?;

        let pass_json_path = run_dir.join(format!("pass-{pass_number:02}.json"));
        write_text_file(
            &pass_json_path,
            &serde_json::to_string_pretty(&normalized)
                .context("failed to encode refinement pass artifact")?,
            true,
        )?;

        let pass_findings_path = run_dir.join(format!("pass-{pass_number:02}-findings.md"));
        write_text_file(
            &pass_findings_path,
            &render_findings_markdown(pass_number, &normalized),
            true,
        )?;

        let pass_rewrite_path = run_dir.join(format!("pass-{pass_number:02}-rewrite.md"));
        write_text_file(&pass_rewrite_path, &normalized.rewrite, true)?;

        pass_records.push(RefinementPassRecord {
            pass_number,
            summary: normalized.summary.clone(),
            findings_json_path: display_path(&pass_json_path, root),
            findings_markdown_path: display_path(&pass_findings_path, root),
            rewrite_path: display_path(&pass_rewrite_path, root),
        });
        previous_pass = Some(normalized);
    }

    let final_pass = previous_pass.ok_or_else(|| anyhow!("no refinement pass was produced"))?;
    let final_proposed_path = run_dir.join(FINAL_PROPOSED_FILE);
    write_text_file(&final_proposed_path, &final_pass.rewrite, true)?;

    let mut apply = RefinementApplyRecord {
        requested: args.apply,
        local_updated: false,
        remote_updated: false,
        local_before_path: None,
        local_after_path: None,
        remote_before_path: None,
        remote_after_path: None,
        error: None,
    };

    if args.apply {
        if let Err(error) = guard_listen_issue_refine_apply(&issue.identifier) {
            apply.error = Some(error.to_string());
        } else {
            let local_before_path = run_dir.join("applied-local-before.md");
            let local_after_path = run_dir.join("applied-local-after.md");
            let remote_before_path = run_dir.join("applied-remote-before.md");
            let remote_after_path = run_dir.join("applied-remote-after.md");

            write_text_file(
                &local_before_path,
                local_index_before.as_deref().unwrap_or_default(),
                true,
            )?;
            write_text_file(&local_after_path, &final_pass.rewrite, true)?;
            write_text_file(&remote_before_path, &original_description, true)?;
            write_text_file(&remote_after_path, &final_pass.rewrite, true)?;

            apply.local_before_path = Some(display_path(&local_before_path, root));
            apply.local_after_path = Some(display_path(&local_after_path, root));
            apply.remote_before_path = Some(display_path(&remote_before_path, root));
            apply.remote_after_path = Some(display_path(&remote_after_path, root));

            write_issue_description(root, &issue.identifier, &final_pass.rewrite)?;
            apply.local_updated = true;

            if let Err(error) = service
                .edit_issue(IssueEditSpec {
                    identifier: issue.identifier.clone(),
                    title: None,
                    description: Some(final_pass.rewrite.clone()),
                    project: None,
                    state: None,
                    priority: None,
                    estimate: None,
                    labels: None,
                    parent_identifier: None,
                })
                .await
            {
                apply.error = Some(error.to_string());
            } else {
                apply.remote_updated = true;
            }
        }
    }

    let completed_at = now_rfc3339()?;
    let summary = RefinementRunSummary {
        run_id: run_id.clone(),
        issue_identifier: issue.identifier.clone(),
        issue_title: issue.title.clone(),
        critique_only: !args.apply,
        passes_requested: args.passes,
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
        &serde_json::to_string_pretty(&summary).context("failed to encode refinement summary")?,
        true,
    )?;

    if let Some(error) = summary.apply.error.as_deref() {
        bail!(
            "refined `{}` but failed to finish the apply-back flow: {}",
            issue.identifier,
            error
        );
    }

    Ok(RefinementReport {
        issue_identifier: issue.identifier.clone(),
        run_dir,
        apply_requested: args.apply,
    })
}

fn render_refinement_reports(root: &Path, reports: &[RefinementReport]) -> String {
    let mut lines = vec![format!("Refined {} issue(s):", reports.len())];

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

fn validate_issue_scope(
    issue: &IssueSummary,
    default_team: Option<&str>,
    default_project_id: Option<&str>,
) -> Result<()> {
    if let Some(team) = default_team
        && !issue.team.key.eq_ignore_ascii_case(team)
    {
        bail!(
            "issue `{}` belongs to team `{}`, outside the configured repo team scope `{}`",
            issue.identifier,
            issue.team.key,
            team
        );
    }

    if let Some(project_selector) = default_project_id {
        let Some(project) = issue.project.as_ref() else {
            bail!(
                "issue `{}` has no project, outside the configured repo project scope `{}`",
                issue.identifier,
                project_selector
            );
        };
        let matches =
            project.id == project_selector || project.name.eq_ignore_ascii_case(project_selector);
        if !matches {
            bail!(
                "issue `{}` belongs to project `{}` (`{}`), outside the configured repo project scope `{}`",
                issue.identifier,
                project.name,
                project.id,
                project_selector
            );
        }
    }

    Ok(())
}

fn build_issue_metadata(issue: &IssueSummary) -> BacklogIssueMetadata {
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
        local_hash: None,
        remote_hash: None,
        last_sync_at: None,
        last_pulled_comment_ids: Vec::new(),
        managed_files: Vec::<ManagedFileRecord>::new(),
    }
}

fn guard_listen_issue_refine_apply(identifier: &str) -> Result<()> {
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
            "`meta issues refine {identifier} --apply` is disabled during `meta listen` because it would overwrite the primary Linear issue description; update the workpad comment instead"
        );
    }

    Ok(())
}

fn normalize_pass_output(output: RefinementPassOutput) -> Result<RefinementPassOutput> {
    let rewrite = output.rewrite.trim().replace("\r\n", "\n");
    if rewrite.is_empty() {
        bail!("refinement agent returned an empty proposed rewrite");
    }

    Ok(RefinementPassOutput {
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

fn render_refinement_prompt(
    root: &Path,
    issue: &IssueSummary,
    local_index_snapshot: Option<&str>,
    pass_number: usize,
    total_passes: usize,
    previous_pass: Option<&RefinementPassOutput>,
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
        .unwrap_or_else(|| "_No previous refinement pass exists yet._".to_string());

    Ok(format!(
        "You are refining an existing Linear issue for the active repository.\n\n\
Repository scope:\n{repo_scope}\n\n\
Issue metadata:\n\
- Identifier: `{identifier}`\n\
- Title: {title}\n\
- Team: {team}\n\
- Project: {project}\n\
- State: {state}\n\
- URL: {url}\n\
- Refinement pass: {pass_number} of {total_passes}\n\n\
Current Linear description:\n{current_description_block}\n\n\
Current local backlog index snapshot:\n{local_backlog_block}\n\n\
Previous refinement pass output:\n{previous_pass_block}\n\n\
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

fn render_fenced_block(language: &str, contents: &str) -> String {
    let fence_len = max_backtick_run(contents).saturating_add(1).max(3);
    let fence = "`".repeat(fence_len);
    if language.is_empty() {
        format!("{fence}\n{contents}\n{fence}")
    } else {
        format!("{fence}{language}\n{contents}\n{fence}")
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

fn render_findings_markdown(pass_number: usize, output: &RefinementPassOutput) -> String {
    let mut lines = vec![
        format!("# Refinement Pass {pass_number}"),
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

fn parse_agent_json<T>(raw: &str, phase: &str) -> Result<T>
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
        "refinement agent returned invalid JSON during {phase}: {}",
        preview_text(trimmed)
    )
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

fn now_rfc3339() -> Result<String> {
    OffsetDateTime::now_utc()
        .format(&format_description!(
            "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
        ))
        .context("failed to format the refinement timestamp")
}

fn refinement_run_id() -> Result<String> {
    let now = OffsetDateTime::now_utc();
    let base = now
        .format(&format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .context("failed to format the refinement run id")?;
    Ok(format!("{}-{:09}", base, now.nanosecond()))
}

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
    fn render_refinement_prompt_uses_safe_fences_for_existing_markdown_code_blocks() {
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
            title: "Refinement prompt fences".to_string(),
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
                description: None,
            }),
            children: Vec::new(),
        };
        let previous_pass = RefinementPassOutput {
            summary: "summary".to_string(),
            findings: StructuredFindings::default(),
            rewrite: "```elixir\nmix test\n```".to_string(),
        };

        let prompt = render_refinement_prompt(
            root,
            &issue,
            Some("```md\n## Local\n```"),
            2,
            2,
            Some(&previous_pass),
        )
        .expect("prompt");

        assert!(prompt.contains("Current Linear description:\n````md\n```bash"));
        assert!(prompt.contains("Current local backlog index snapshot:\n````md\n```md"));
        assert!(prompt.contains("Previous refinement pass output:\n````json\n{"));
        assert!(prompt.contains("\"rewrite\": \"```elixir\\nmix test\\n```\""));
    }

    #[test]
    fn preview_text_truncates_on_char_boundary() {
        let preview = preview_text(&"é".repeat(241));

        assert_eq!(preview, format!("{}...", "é".repeat(240)));
    }
}

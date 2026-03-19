use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::agents::{
    apply_invocation_environment, apply_noninteractive_agent_environment,
    command_args_for_invocation, format_agent_config_source, resolve_agent_invocation_for_planning,
    validate_invocation_command_surface,
};
use crate::cli::MergeArgs;
use crate::config::{
    AGENT_ROUTE_MERGE, AgentConfigOverrides, AppConfig, PlanningMeta, load_required_planning_meta,
};
use crate::fs::{
    PlanningPaths, canonicalize_existing_dir, ensure_dir, ensure_workspace_path_is_safe,
    sibling_workspace_root, write_text_file,
};
use crate::merge_dashboard::{
    MergeDashboardAction, MergeDashboardData, MergeDashboardExit, MergeDashboardOptions,
    MergeDashboardPullRequest, run_merge_dashboard,
};
use crate::progress::{
    ProgressArtifact, ProgressOutputMode, ProgressStepDefinition, ProgressTracker,
};
use crate::scaffold::ensure_planning_layout;

const STEP_PREPARE_WORKSPACE: &str = "prepare_workspace";
const STEP_PLAN: &str = "plan_generation";
const STEP_APPLY: &str = "merge_application";
const STEP_VALIDATE: &str = "validation";
const STEP_PUSH: &str = "push";
const STEP_PUBLISH: &str = "publish_pr";
const MAX_VALIDATION_REPAIR_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Serialize)]
struct MergeDiscovery {
    repository: GithubRepository,
    pull_requests: Vec<GithubPullRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GithubRepository {
    name_with_owner: String,
    url: String,
    default_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GithubPullRequest {
    number: u64,
    title: String,
    body: String,
    url: String,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    author: GithubActor,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GithubActor {
    login: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RepoViewResponse {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
    url: String,
    #[serde(rename = "defaultBranchRef")]
    default_branch_ref: GithubBranchRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GithubBranchRef {
    name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MergePlan {
    merge_order: Vec<u64>,
    conflict_hotspots: Vec<String>,
    summary: String,
}

#[derive(Debug, Clone, Serialize)]
struct MergeContextArtifact {
    run_id: String,
    repository: GithubRepository,
    selected_pull_requests: Vec<GithubPullRequest>,
    source_root: String,
    workspace_path: String,
    aggregate_branch: String,
    agent_resolution: AgentResolutionArtifact,
}

#[derive(Debug, Clone, Serialize)]
struct AgentResolutionArtifact {
    provider: String,
    model: Option<String>,
    reasoning: Option<String>,
    route_key: Option<String>,
    family_key: Option<String>,
    provider_source: String,
    model_source: Option<String>,
    reasoning_source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct MergeProgressArtifact {
    run: ProgressArtifact,
    steps: Vec<MergeStepRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct MergeStepRecord {
    pull_request: u64,
    status: String,
    detail: String,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationArtifact {
    attempts: Vec<ValidationAttemptRecord>,
    success: bool,
    repair_attempts: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationAttemptRecord {
    attempt: usize,
    commands: Vec<ValidationCommandRecord>,
    repair: Option<ValidationRepairRecord>,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationCommandRecord {
    command: String,
    exit_code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationRepairRecord {
    attempt: usize,
    commit: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PublicationArtifact {
    aggregate_branch: String,
    title: String,
    url: String,
    action: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ExistingAggregatePullRequest {
    number: u64,
    url: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreatedPullRequest {
    url: String,
}

#[derive(Debug, Clone)]
struct AggregatePublication {
    url: String,
    action: &'static str,
}

#[derive(Debug, Clone)]
struct GhCli;

pub async fn run_merge(args: &MergeArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "merge")?;
    ensure_planning_layout(&root, false)?;

    let gh = GhCli;
    let repository = gh.resolve_repository(&root)?;
    let pull_requests = gh.list_open_pull_requests(&root)?;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&MergeDiscovery {
                repository,
                pull_requests,
            })?
        );
        return Ok(());
    }

    let selected_numbers = if args.render_once {
        let exit = run_merge_dashboard(
            build_dashboard_data(&repository, &pull_requests),
            MergeDashboardOptions {
                render_once: true,
                width: args.width,
                height: args.height,
                actions: args
                    .events
                    .iter()
                    .copied()
                    .map(MergeDashboardAction::from)
                    .collect(),
            },
        )?;
        let MergeDashboardExit::Snapshot(snapshot) = exit else {
            bail!("`meta merge --render-once` should only emit a snapshot");
        };
        println!("{snapshot}");
        return Ok(());
    } else if args.no_interactive {
        if args.pull_requests.is_empty() {
            bail!("`meta merge --no-interactive` requires at least one `--pull-request <NUMBER>`");
        }
        args.pull_requests.clone()
    } else {
        match run_merge_dashboard(
            build_dashboard_data(&repository, &pull_requests),
            MergeDashboardOptions {
                render_once: false,
                width: args.width,
                height: args.height,
                actions: Vec::new(),
            },
        )? {
            MergeDashboardExit::Selected(numbers) if numbers.is_empty() => {
                println!("Merge canceled before execution. No aggregate branch or PR was created.");
                return Ok(());
            }
            MergeDashboardExit::Selected(numbers) => numbers,
            MergeDashboardExit::Cancelled => {
                println!("Merge canceled before execution. No aggregate branch or PR was created.");
                return Ok(());
            }
            MergeDashboardExit::Snapshot(_) => unreachable!(),
        }
    };

    let selected_pull_requests = resolve_selected_pull_requests(&pull_requests, &selected_numbers)
        .with_context(|| {
            format!(
                "selected pull request set `{}` did not match the discovered open PR list",
                selected_numbers
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    let run = execute_merge_run(&root, args, &gh, &repository, selected_pull_requests)?;

    let publication_verb = match run.publication.action.as_str() {
        "updated" => "Updated",
        _ => "Created",
    };
    println!(
        "{publication_verb} aggregate PR {} for {} pull request(s). Run artifacts saved in {}",
        run.publication.url,
        run.selected_count,
        run.run_dir.display()
    );
    Ok(())
}

fn build_dashboard_data(
    repository: &GithubRepository,
    pull_requests: &[GithubPullRequest],
) -> MergeDashboardData {
    MergeDashboardData {
        title: format!("meta merge ({})", repository.name_with_owner),
        repo_label: repository.name_with_owner.clone(),
        base_branch: repository.default_branch.clone(),
        pull_requests: pull_requests
            .iter()
            .map(|pr| MergeDashboardPullRequest {
                number: pr.number,
                title: pr.title.clone(),
                author: pr.author.login.clone(),
                head_ref: pr.head_ref_name.clone(),
                updated_at: pr.updated_at.clone(),
                url: pr.url.clone(),
            })
            .collect(),
    }
}

fn resolve_selected_pull_requests(
    pull_requests: &[GithubPullRequest],
    selected_numbers: &[u64],
) -> Result<Vec<GithubPullRequest>> {
    let by_number = pull_requests
        .iter()
        .cloned()
        .map(|pr| (pr.number, pr))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    let mut selected = Vec::new();
    for number in selected_numbers {
        if !seen.insert(*number) {
            bail!("pull request #{number} was selected more than once");
        }
        let pr = by_number.get(number).cloned().ok_or_else(|| {
            anyhow!("pull request #{number} is not open in the current repository")
        })?;
        selected.push(pr);
    }
    Ok(selected)
}

struct MergeExecution {
    run_dir: PathBuf,
    publication: PublicationArtifact,
    selected_count: usize,
}

struct MergeApplicationContext<'a> {
    root: &'a Path,
    workspace_path: &'a Path,
    args: &'a MergeArgs,
    repository: &'a GithubRepository,
    selected_pull_requests: &'a [GithubPullRequest],
    plan: &'a MergePlan,
    run_dir: &'a Path,
}

fn merge_progress_steps() -> [ProgressStepDefinition; 6] {
    [
        ProgressStepDefinition {
            key: STEP_PREPARE_WORKSPACE,
            label: "Workspace preparation",
        },
        ProgressStepDefinition {
            key: STEP_PLAN,
            label: "Plan generation",
        },
        ProgressStepDefinition {
            key: STEP_APPLY,
            label: "Merge application",
        },
        ProgressStepDefinition {
            key: STEP_VALIDATE,
            label: "Validation",
        },
        ProgressStepDefinition {
            key: STEP_PUSH,
            label: "Push",
        },
        ProgressStepDefinition {
            key: STEP_PUBLISH,
            label: "PR publication",
        },
    ]
}

fn execute_merge_run(
    root: &Path,
    args: &MergeArgs,
    gh: &GhCli,
    repository: &GithubRepository,
    selected_pull_requests: Vec<GithubPullRequest>,
) -> Result<MergeExecution> {
    let paths = PlanningPaths::new(root);
    ensure_dir(&paths.merge_runs_dir)?;

    let (run_id, run_dir) = reserve_run_dir(&paths)?;
    let progress_mode = if args.no_interactive {
        ProgressOutputMode::Text
    } else {
        ProgressOutputMode::Interactive
    };
    let mut tracker = ProgressTracker::start(
        format!("meta merge progress ({})", repository.name_with_owner),
        run_dir.join("progress.json"),
        &merge_progress_steps(),
        progress_mode,
    )?;

    let aggregate_branch = format!("meta-merge/{run_id}");
    tracker.start_step(
        STEP_PREPARE_WORKSPACE,
        format!(
            "Preparing an isolated aggregate workspace for `{aggregate_branch}` from `origin/{}`.",
            repository.default_branch
        ),
    )?;
    let workspace_path =
        match prepare_workspace(root, &run_id, &aggregate_branch, &repository.default_branch) {
            Ok(path) => path,
            Err(error) => {
                tracker.fail_step(
                    STEP_PREPARE_WORKSPACE,
                    format!("Workspace preparation failed: {error:#}"),
                    None,
                )?;
                return Err(error);
            }
        };
    tracker.complete_step(
        STEP_PREPARE_WORKSPACE,
        format!(
            "Workspace ready: `{}` checked out to `{aggregate_branch}`.",
            workspace_path.display()
        ),
    )?;

    let plan_prompt =
        build_merge_plan_prompt(repository, &selected_pull_requests, &aggregate_branch);
    write_text_file(&run_dir.join("agent-plan-prompt.md"), &plan_prompt, true)?;
    let resolution = resolve_merge_agent_resolution(root, args, &plan_prompt)?;
    let context_artifact = MergeContextArtifact {
        run_id: run_id.clone(),
        repository: repository.clone(),
        selected_pull_requests: selected_pull_requests.clone(),
        source_root: root.display().to_string(),
        workspace_path: workspace_path.display().to_string(),
        aggregate_branch: aggregate_branch.clone(),
        agent_resolution: resolution,
    };
    write_json_artifact(&run_dir.join("context.json"), &context_artifact)?;
    tracker.start_step(
        STEP_PLAN,
        format!(
            "Drafting the merge plan for {} selected pull request(s).",
            selected_pull_requests.len()
        ),
    )?;
    let plan = match request_merge_plan(root, &workspace_path, args, &plan_prompt) {
        Ok(plan) => plan,
        Err(error) => {
            tracker.fail_step(
                STEP_PLAN,
                format!("Plan generation failed: {error:#}"),
                None,
            )?;
            return Err(error);
        }
    };
    if let Err(error) = validate_merge_plan(&plan, &selected_pull_requests) {
        tracker.fail_step(
            STEP_PLAN,
            format!("Planner output was invalid: {error:#}"),
            None,
        )?;
        return Err(error);
    }
    write_json_artifact(&run_dir.join("plan.json"), &plan)?;
    tracker.complete_step(
        STEP_PLAN,
        format!(
            "Merge order recorded as [{}].",
            plan.merge_order
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    )?;

    tracker.start_step(
        STEP_APPLY,
        format!(
            "Applying {} pull request(s) in planner order.",
            plan.merge_order.len()
        ),
    )?;
    let progress = match apply_pull_requests(
        MergeApplicationContext {
            root,
            workspace_path: &workspace_path,
            args,
            repository,
            selected_pull_requests: &selected_pull_requests,
            plan: &plan,
            run_dir: &run_dir,
        },
        &mut tracker,
    ) {
        Ok(progress) => progress,
        Err(error) => {
            let detail = tracker
                .artifact()
                .active_detail
                .clone()
                .unwrap_or_else(|| "Merge application failed.".to_string());
            tracker.fail_step(STEP_APPLY, format!("{detail} {error:#}"), None)?;
            return Err(error);
        }
    };
    tracker.complete_step(
        STEP_APPLY,
        format!(
            "Applied {} pull request(s) to the aggregate branch.",
            progress.len()
        ),
    )?;
    write_json_artifact(
        &run_dir.join("merge-progress.json"),
        &MergeProgressArtifact {
            run: tracker.artifact().clone(),
            steps: progress.clone(),
        },
    )?;

    let validation_commands = match validation_commands(root, args) {
        Ok(commands) => commands,
        Err(error) => {
            tracker.fail_step(
                STEP_VALIDATE,
                format!("Validation setup failed: {error:#}"),
                None,
            )?;
            write_merge_progress_artifact(&run_dir, &tracker, &progress)?;
            return Err(error);
        }
    };
    tracker.start_step(
        STEP_VALIDATE,
        format!(
            "Running validation command(s): {}",
            validation_commands.join(" && ")
        ),
    )?;
    let validation = match run_validation_until_passes(
        root,
        &workspace_path,
        args,
        repository,
        &aggregate_branch,
        &selected_pull_requests,
        &plan,
        &run_dir,
        &mut tracker,
        validation_commands,
    ) {
        Ok(validation) => validation,
        Err(error) => {
            let detail = if error
                .to_string()
                .contains("validation failed for aggregate branch")
            {
                format!("{error:#}")
            } else {
                format!("Validation execution failed: {error:#}")
            };
            tracker.fail_step(STEP_VALIDATE, detail, None)?;
            write_merge_progress_artifact(&run_dir, &tracker, &progress)?;
            return Err(error);
        }
    };
    write_json_artifact(&run_dir.join("validation.json"), &validation)?;
    tracker.complete_step(
        STEP_VALIDATE,
        format!(
            "Validation passed after {} attempt(s) across {} command(s).",
            validation.attempts.len(),
            validation
                .attempts
                .last()
                .map(|attempt| attempt.commands.len())
                .unwrap_or(0)
        ),
    )?;

    tracker.start_step(
        STEP_PUSH,
        format!("Pushing aggregate branch `{aggregate_branch}` to origin."),
    )?;
    if let Err(error) = run_git(
        &workspace_path,
        &[
            "push",
            "--set-upstream",
            "origin",
            aggregate_branch.as_str(),
        ],
    ) {
        tracker.fail_step(
            STEP_PUSH,
            format!("Push failed for `{aggregate_branch}`: {error:#}"),
            None,
        )?;
        write_merge_progress_artifact(&run_dir, &tracker, &progress)?;
        return Err(error);
    }
    tracker.complete_step(
        STEP_PUSH,
        format!("Aggregate branch `{aggregate_branch}` is now on origin."),
    )?;

    let pr_title = aggregate_pr_title(&selected_pull_requests);
    let pr_body = aggregate_pr_body(repository, &selected_pull_requests, &plan, &run_id);
    let pr_body_path = run_dir.join("aggregate-pr-body.md");
    write_text_file(&pr_body_path, &pr_body, true)?;
    tracker.start_step(
        STEP_PUBLISH,
        format!(
            "Publishing the aggregate pull request into `{}`.",
            repository.default_branch
        ),
    )?;
    let publication = match gh.publish_aggregate_pull_request(
        &workspace_path,
        repository,
        &aggregate_branch,
        &repository.default_branch,
        &pr_title,
        &pr_body_path,
    ) {
        Ok(publication) => publication,
        Err(error) => {
            tracker.fail_step(
                STEP_PUBLISH,
                format!("Aggregate PR publication failed: {error:#}"),
                None,
            )?;
            write_merge_progress_artifact(&run_dir, &tracker, &progress)?;
            return Err(error);
        }
    };
    let publication_artifact = PublicationArtifact {
        aggregate_branch,
        title: pr_title,
        url: publication.url,
        action: publication.action.to_string(),
    };
    write_json_artifact(&run_dir.join("publication.json"), &publication_artifact)?;
    tracker.complete_step(
        STEP_PUBLISH,
        format!(
            "{} aggregate pull request {}.",
            match publication_artifact.action.as_str() {
                "updated" => "Updated",
                _ => "Created",
            },
            publication_artifact.url
        ),
    )?;
    tracker.finish_success(format!(
        "{} aggregate pull request {}. Review the run artifacts for planner, validation, and publication details.",
        match publication_artifact.action.as_str() {
            "updated" => "Updated",
            _ => "Created",
        },
        publication_artifact.url
    ))?;
    write_json_artifact(
        &run_dir.join("merge-progress.json"),
        &MergeProgressArtifact {
            run: tracker.artifact().clone(),
            steps: progress,
        },
    )?;

    Ok(MergeExecution {
        run_dir,
        publication: publication_artifact,
        selected_count: selected_pull_requests.len(),
    })
}

fn write_merge_progress_artifact(
    run_dir: &Path,
    tracker: &ProgressTracker,
    steps: &[MergeStepRecord],
) -> Result<()> {
    write_json_artifact(
        &run_dir.join("merge-progress.json"),
        &MergeProgressArtifact {
            run: tracker.artifact().clone(),
            steps: steps.to_vec(),
        },
    )
}

fn reserve_run_dir(paths: &PlanningPaths) -> Result<(String, PathBuf)> {
    reserve_run_dir_at(paths, Utc::now())
}

fn reserve_run_dir_at(
    paths: &PlanningPaths,
    timestamp: chrono::DateTime<Utc>,
) -> Result<(String, PathBuf)> {
    let base = timestamp.format("%Y%m%dT%H%M%SZ").to_string();
    for suffix in 0..100 {
        let run_id = if suffix == 0 {
            base.clone()
        } else {
            format!("{base}-{suffix:02}")
        };
        let run_dir = paths.merge_run_dir(&run_id);
        if ensure_dir(&run_dir)? {
            return Ok((run_id, run_dir));
        }
    }

    bail!(
        "failed to reserve a unique merge run directory under `{}`",
        paths.merge_runs_dir.display()
    )
}

fn prepare_workspace(
    root: &Path,
    run_id: &str,
    aggregate_branch: &str,
    base_branch: &str,
) -> Result<PathBuf> {
    let workspace_root = sibling_workspace_root(root)?.join("merge-runs");
    ensure_dir(&workspace_root)?;
    let workspace_path = workspace_root.join(run_id);
    if workspace_path.exists() {
        bail!(
            "refusing to reuse existing merge workspace `{}`",
            workspace_path.display()
        );
    }

    let remote_url = git_stdout(root, &["remote", "get-url", "origin"])
        .context("failed to resolve the repository origin remote")?;
    run_git(root, &["fetch", "origin", base_branch])?;
    run_git(
        root,
        &[
            "clone",
            "--origin",
            "origin",
            remote_url.as_str(),
            workspace_path
                .to_str()
                .ok_or_else(|| anyhow!("workspace path is not valid utf-8"))?,
        ],
    )?;

    ensure_workspace_path_is_safe(root, &workspace_root, &workspace_path)?;
    configure_workspace_git_identity(root, &workspace_path)?;

    run_git(&workspace_path, &["fetch", "origin", base_branch])?;
    run_git(
        &workspace_path,
        &[
            "checkout",
            "-B",
            aggregate_branch,
            &format!("origin/{base_branch}"),
        ],
    )?;
    Ok(workspace_path)
}

fn configure_workspace_git_identity(source_root: &Path, workspace_path: &Path) -> Result<()> {
    let email = git_config_value(source_root, "user.email")?
        .unwrap_or_else(|| "metastack-cli@example.com".to_string());
    let name =
        git_config_value(source_root, "user.name")?.unwrap_or_else(|| "MetaStack CLI".to_string());

    run_git(workspace_path, &["config", "user.email", email.as_str()])?;
    run_git(workspace_path, &["config", "user.name", name.as_str()])?;
    Ok(())
}

fn git_config_value(root: &Path, key: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get", key])
        .output()
        .with_context(|| format!("failed to read git config key `{key}`"))?;
    match output.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        )),
        Some(1) => Ok(None),
        _ => bail!(
            "git config --get {key} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

fn build_merge_plan_prompt(
    repository: &GithubRepository,
    selected_pull_requests: &[GithubPullRequest],
    aggregate_branch: &str,
) -> String {
    let mut lines = vec![
        format!(
            "Plan a one-shot aggregate merge run for `{}`.",
            repository.name_with_owner
        ),
        format!("Base branch: `{}`", repository.default_branch),
        format!("Aggregate branch: `{aggregate_branch}`"),
        "Choose an explicit merge order and call out likely conflict hotspots before execution."
            .to_string(),
        String::new(),
        "Return strict JSON with this shape:".to_string(),
        r#"{"merge_order":[101,102],"conflict_hotspots":["config.rs","README.md"],"summary":"why this order is safest"}"#.to_string(),
        String::new(),
        "Selected pull requests:".to_string(),
    ];

    for pr in selected_pull_requests {
        lines.push(format!(
            "- #{} {} | head=`{}` | base=`{}` | author=`{}` | url={}",
            pr.number, pr.title, pr.head_ref_name, pr.base_ref_name, pr.author.login, pr.url
        ));
        if !pr.body.trim().is_empty() {
            lines.push(format!("  body: {}", truncate_single_line(&pr.body, 240)));
        }
    }

    lines.join("\n")
}

fn request_merge_plan(
    root: &Path,
    workspace_path: &Path,
    args: &MergeArgs,
    prompt: &str,
) -> Result<MergePlan> {
    let output = run_agent_capture_in_dir(
        root,
        workspace_path,
        AgentConfigOverrides {
            provider: args.agent.clone(),
            model: args.model.clone(),
            reasoning: args.reasoning.clone(),
        },
        prompt,
        Vec::new(),
    )?;
    let plan = parse_json_block::<MergePlan>(&output)
        .context("merge planner did not return a valid JSON plan")?;
    if plan.merge_order.is_empty() {
        bail!("merge planner returned an empty merge order");
    }
    Ok(plan)
}

fn validate_merge_plan(
    plan: &MergePlan,
    selected_pull_requests: &[GithubPullRequest],
) -> Result<()> {
    let expected = selected_pull_requests
        .iter()
        .map(|pr| pr.number)
        .collect::<BTreeSet<_>>();
    let planned = plan.merge_order.iter().copied().collect::<BTreeSet<_>>();

    if planned.len() != plan.merge_order.len() {
        bail!("merge planner returned duplicate pull requests in the merge order");
    }

    if expected != planned {
        let expected_numbers = expected
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let planned_numbers = plan
            .merge_order
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "merge planner must return the full selected pull request set in merge_order; expected [{expected_numbers}], got [{planned_numbers}]"
        );
    }

    Ok(())
}

fn apply_pull_requests(
    context: MergeApplicationContext<'_>,
    tracker: &mut ProgressTracker,
) -> Result<Vec<MergeStepRecord>> {
    let selected_by_number = context
        .selected_pull_requests
        .iter()
        .cloned()
        .map(|pr| (pr.number, pr))
        .collect::<BTreeMap<_, _>>();
    let mut steps = Vec::new();

    for number in &context.plan.merge_order {
        let Some(pr) = selected_by_number.get(number) else {
            bail!("merge planner referenced unselected pull request #{number}");
        };
        tracker.update_detail(
            STEP_APPLY,
            format!(
                "Fetching pull request #{} ({}) into the merge workspace.",
                pr.number, pr.title
            ),
            Some(pr.number),
        )?;

        let fetch_ref = format!(
            "+refs/pull/{}/head:refs/remotes/origin/pr/{}",
            pr.number, pr.number
        );
        run_git(
            context.workspace_path,
            &["fetch", "origin", fetch_ref.as_str()],
        )?;
        let merge_target = format!("origin/pr/{}", pr.number);
        tracker.update_detail(
            STEP_APPLY,
            format!(
                "Merging pull request #{} ({}) onto the aggregate branch.",
                pr.number, pr.title
            ),
            Some(pr.number),
        )?;
        match run_git(
            context.workspace_path,
            &["merge", "--no-ff", "--no-edit", merge_target.as_str()],
        ) {
            Ok(()) => {
                steps.push(MergeStepRecord {
                    pull_request: pr.number,
                    status: "merged".to_string(),
                    detail: format!("Merged #{} into the aggregate branch", pr.number),
                });
                tracker.update_detail(
                    STEP_APPLY,
                    format!("Pull request #{} merged cleanly.", pr.number),
                    Some(pr.number),
                )?;
            }
            Err(error) => {
                let conflicted_files = git_stdout(
                    context.workspace_path,
                    &["diff", "--name-only", "--diff-filter=U"],
                )?;
                if conflicted_files.trim().is_empty() {
                    return Err(error)
                        .with_context(|| format!("failed to merge pull request #{}", pr.number));
                }
                tracker.update_detail(
                    STEP_APPLY,
                    format!(
                        "Conflict assistance invoked for pull request #{} across {}.",
                        pr.number,
                        conflicted_files.replace('\n', ", ")
                    ),
                    Some(pr.number),
                )?;

                let resolution_prompt = build_conflict_prompt(
                    context.repository,
                    pr,
                    context.plan,
                    context.workspace_path,
                    conflicted_files.trim(),
                )?;
                write_text_file(
                    &context
                        .run_dir
                        .join(format!("conflict-prompt-pr-{}.md", pr.number)),
                    &resolution_prompt,
                    true,
                )?;
                let output = run_agent_capture_in_dir(
                    context.root,
                    context.workspace_path,
                    AgentConfigOverrides {
                        provider: context.args.agent.clone(),
                        model: context.args.model.clone(),
                        reasoning: context.args.reasoning.clone(),
                    },
                    &resolution_prompt,
                    vec![(
                        "METASTACK_MERGE_CONFLICT_PULL_REQUEST".to_string(),
                        pr.number.to_string(),
                    )],
                )?;
                write_text_file(
                    &context
                        .run_dir
                        .join(format!("conflict-resolution-pr-{}.md", pr.number)),
                    &output,
                    true,
                )?;

                run_git(context.workspace_path, &["add", "-A"])?;
                let unresolved = git_stdout(
                    context.workspace_path,
                    &["diff", "--name-only", "--diff-filter=U"],
                )?;
                if !unresolved.trim().is_empty() {
                    bail!(
                        "merge conflict for pull request #{} remains unresolved after agent assistance: {}",
                        pr.number,
                        unresolved
                    );
                }

                run_git(context.workspace_path, &["commit", "--no-edit"])?;
                steps.push(MergeStepRecord {
                    pull_request: pr.number,
                    status: "conflict_resolved".to_string(),
                    detail: format!(
                        "Merged #{} after agent-assisted conflict resolution for {}",
                        pr.number,
                        conflicted_files.replace('\n', ", ")
                    ),
                });
                tracker.update_detail(
                    STEP_APPLY,
                    format!(
                        "Pull request #{} merged after agent-assisted conflict resolution.",
                        pr.number
                    ),
                    Some(pr.number),
                )?;
            }
        }
    }

    Ok(steps)
}

fn build_conflict_prompt(
    repository: &GithubRepository,
    pull_request: &GithubPullRequest,
    plan: &MergePlan,
    workspace_path: &Path,
    conflicted_files: &str,
) -> Result<String> {
    let head = git_stdout(workspace_path, &["rev-parse", "--short", "HEAD"])?;
    Ok(format!(
        "Resolve an in-progress git merge conflict inside `{}` for `{}`.\nCurrent aggregate HEAD: `{}`\nPull request: #{} {}\nLikely hotspots from the planner: {}\nConflicted files:\n{}\n\nEdit the working tree in place, stage the resolved files, and leave the repository ready for `git commit --no-edit`. Then print a short Markdown summary of what you changed.",
        workspace_path.display(),
        repository.name_with_owner,
        head,
        pull_request.number,
        pull_request.title,
        if plan.conflict_hotspots.is_empty() {
            "none recorded".to_string()
        } else {
            plan.conflict_hotspots.join(", ")
        },
        conflicted_files
    ))
}

fn validation_commands(root: &Path, args: &MergeArgs) -> Result<Vec<String>> {
    if !args.validate.is_empty() {
        return Ok(args.validate.clone());
    }

    let makefile = root.join("Makefile");
    if makefile.is_file() {
        let contents = fs::read_to_string(&makefile)
            .with_context(|| format!("failed to read `{}`", makefile.display()))?;
        if makefile_has_target(&contents, "quality") {
            return Ok(vec!["make quality".to_string()]);
        }
        if makefile_has_target(&contents, "all") {
            return Ok(vec!["make all".to_string()]);
        }
    }

    if root.join("Cargo.toml").is_file() {
        return Ok(vec!["cargo test".to_string()]);
    }

    bail!(
        "no default validation command was inferred for `{}`; pass one or more `--validate <COMMAND>` flags",
        root.display()
    )
}

fn makefile_has_target(contents: &str, target: &str) -> bool {
    contents.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.starts_with('\t') || trimmed.is_empty() {
            return false;
        }

        trimmed
            .split_once(':')
            .map(|(name, _)| name.trim() == target)
            .unwrap_or(false)
    })
}

fn run_validation_commands(
    workspace_path: &Path,
    commands: Vec<String>,
) -> Result<Vec<ValidationCommandRecord>> {
    let mut records = Vec::new();
    for command in commands {
        let output = Command::new("/bin/sh")
            .arg("-lc")
            .arg(&command)
            .current_dir(workspace_path)
            .output()
            .with_context(|| format!("failed to run validation command `{command}`"))?;
        records.push(ValidationCommandRecord {
            command,
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        });
    }
    Ok(records)
}

#[allow(clippy::too_many_arguments)]
fn run_validation_until_passes(
    root: &Path,
    workspace_path: &Path,
    args: &MergeArgs,
    repository: &GithubRepository,
    aggregate_branch: &str,
    selected_pull_requests: &[GithubPullRequest],
    plan: &MergePlan,
    run_dir: &Path,
    tracker: &mut ProgressTracker,
    validation_commands: Vec<String>,
) -> Result<ValidationArtifact> {
    let mut attempts = Vec::new();

    for attempt in 1..=(MAX_VALIDATION_REPAIR_ATTEMPTS + 1) {
        let commands = run_validation_commands(workspace_path, validation_commands.clone())?;
        let failing_command = commands
            .iter()
            .find(|record| record.exit_code != 0)
            .map(|record| format!("`{}` exited with {}", record.command, record.exit_code));
        let mut attempt_record = ValidationAttemptRecord {
            attempt,
            commands,
            repair: None,
        };

        if failing_command.is_none() {
            attempts.push(attempt_record);
            let artifact = ValidationArtifact {
                attempts,
                success: true,
                repair_attempts: attempt.saturating_sub(1),
            };
            write_json_artifact(&run_dir.join("validation.json"), &artifact)?;
            return Ok(artifact);
        }

        if attempt > MAX_VALIDATION_REPAIR_ATTEMPTS {
            attempts.push(attempt_record);
            let artifact = ValidationArtifact {
                attempts,
                success: false,
                repair_attempts: MAX_VALIDATION_REPAIR_ATTEMPTS,
            };
            write_json_artifact(&run_dir.join("validation.json"), &artifact)?;
            bail!(
                "validation failed for aggregate branch `{aggregate_branch}` after {} repair attempt(s); last failure: {}",
                MAX_VALIDATION_REPAIR_ATTEMPTS,
                failing_command.unwrap_or_else(|| "a validation command failed".to_string())
            );
        }

        let repair_attempt = attempt;
        tracker.update_detail(
            STEP_VALIDATE,
            format!(
                "Validation attempt {attempt} failed on {}. Invoking repair assistance ({repair_attempt}/{MAX_VALIDATION_REPAIR_ATTEMPTS}).",
                failing_command
                    .clone()
                    .unwrap_or_else(|| "a validation command failed".to_string())
            ),
            None,
        )?;

        let repair_prompt = build_validation_repair_prompt(
            repository,
            aggregate_branch,
            selected_pull_requests,
            plan,
            workspace_path,
            repair_attempt,
            &attempt_record.commands,
        )?;
        write_text_file(
            &run_dir.join(format!(
                "validation-repair-prompt-attempt-{repair_attempt}.md"
            )),
            &repair_prompt,
            true,
        )?;
        let repair_output = run_agent_capture_in_dir(
            root,
            workspace_path,
            AgentConfigOverrides {
                provider: args.agent.clone(),
                model: args.model.clone(),
                reasoning: args.reasoning.clone(),
            },
            &repair_prompt,
            vec![(
                "METASTACK_MERGE_VALIDATION_ATTEMPT".to_string(),
                repair_attempt.to_string(),
            )],
        )?;
        write_text_file(
            &run_dir.join(format!(
                "validation-repair-output-attempt-{repair_attempt}.md"
            )),
            &repair_output,
            true,
        )?;

        let repair_commit = commit_validation_repair(workspace_path, repair_attempt)?;
        tracker.update_detail(
            STEP_VALIDATE,
            match &repair_commit {
                Some(commit) => format!(
                    "Recorded validation repair commit `{commit}` for attempt {repair_attempt}; rerunning validation."
                ),
                None => format!(
                    "Repair attempt {repair_attempt} produced no tracked changes; rerunning validation."
                ),
            },
            None,
        )?;
        attempt_record.repair = Some(ValidationRepairRecord {
            attempt: repair_attempt,
            commit: repair_commit,
        });
        attempts.push(attempt_record);
        write_json_artifact(
            &run_dir.join("validation.json"),
            &ValidationArtifact {
                attempts: attempts.clone(),
                success: false,
                repair_attempts: repair_attempt,
            },
        )?;
    }

    bail!("validation retry loop terminated unexpectedly for aggregate branch `{aggregate_branch}`")
}

fn commit_validation_repair(
    workspace_path: &Path,
    repair_attempt: usize,
) -> Result<Option<String>> {
    run_git(workspace_path, &["add", "-A"])?;
    if !workspace_has_tracked_changes(workspace_path)? {
        return Ok(None);
    }
    run_git(
        workspace_path,
        &[
            "commit",
            "-m",
            &format!("meta merge: repair validation failures (attempt {repair_attempt})"),
        ],
    )?;
    Ok(Some(git_stdout(
        workspace_path,
        &["rev-parse", "--short", "HEAD"],
    )?))
}

fn workspace_has_tracked_changes(workspace_path: &Path) -> Result<bool> {
    Ok(!git_stdout(workspace_path, &["status", "--short"])?
        .trim()
        .is_empty())
}

fn build_validation_repair_prompt(
    repository: &GithubRepository,
    aggregate_branch: &str,
    selected_pull_requests: &[GithubPullRequest],
    plan: &MergePlan,
    workspace_path: &Path,
    repair_attempt: usize,
    commands: &[ValidationCommandRecord],
) -> Result<String> {
    let head = git_stdout(workspace_path, &["rev-parse", "--short", "HEAD"])?;
    let mut lines = vec![
        format!(
            "Repair a failing aggregate merge validation inside `{}` for `{}`.",
            workspace_path.display(),
            repository.name_with_owner
        ),
        format!("Aggregate branch: `{aggregate_branch}`"),
        format!("Current aggregate HEAD: `{head}`"),
        format!("Validation repair attempt: {repair_attempt}/{MAX_VALIDATION_REPAIR_ATTEMPTS}"),
        format!(
            "Selected pull requests: {}",
            selected_pull_requests
                .iter()
                .map(|pr| format!("#{} {}", pr.number, pr.title))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        format!(
            "Planner hotspots: {}",
            if plan.conflict_hotspots.is_empty() {
                "none recorded".to_string()
            } else {
                plan.conflict_hotspots.join(", ")
            }
        ),
        String::new(),
        "Validation failures:".to_string(),
    ];

    for record in commands.iter().filter(|record| record.exit_code != 0) {
        lines.push(format!(
            "- Command: `{}` (exit {})",
            record.command, record.exit_code
        ));
        if !record.stdout.trim().is_empty() {
            lines.push("  stdout:".to_string());
            lines.push(format!(
                "  ```\n{}\n  ```",
                truncate_validation_output(&record.stdout, 4000)
            ));
        }
        if !record.stderr.trim().is_empty() {
            lines.push("  stderr:".to_string());
            lines.push(format!(
                "  ```\n{}\n  ```",
                truncate_validation_output(&record.stderr, 4000)
            ));
        }
    }

    lines.push(String::new());
    lines.push("Edit the workspace in place to make the configured validation commands pass. You may run repo-local formatting, lint, and test commands as needed. Stage any intended changes before finishing. Leave the repository in a clean state ready for the merge runner to commit and rerun validation. Then print a short Markdown summary of the fix.".to_string());
    Ok(lines.join("\n"))
}

fn truncate_validation_output(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        value.to_string()
    } else {
        format!("{}...", &value[..max_len])
    }
}

fn aggregate_pr_title(selected_pull_requests: &[GithubPullRequest]) -> String {
    let numbers = selected_pull_requests
        .iter()
        .map(|pr| format!("#{}", pr.number))
        .collect::<Vec<_>>()
        .join(", ");
    format!("meta merge: {numbers}")
}

fn aggregate_pr_body(
    repository: &GithubRepository,
    selected_pull_requests: &[GithubPullRequest],
    plan: &MergePlan,
    run_id: &str,
) -> String {
    let mut body = vec![
        format!("# Aggregate merge for `{}`", repository.name_with_owner),
        String::new(),
        format!("Run ID: `{run_id}`"),
        String::new(),
        "Included pull requests:".to_string(),
    ];
    for pr in selected_pull_requests {
        body.push(format!("- #{} {} ({})", pr.number, pr.title, pr.url));
    }
    body.push(String::new());
    body.push("Planner summary:".to_string());
    body.push(plan.summary.clone());
    if !plan.conflict_hotspots.is_empty() {
        body.push(String::new());
        body.push("Conflict hotspots called out before execution:".to_string());
        for hotspot in &plan.conflict_hotspots {
            body.push(format!("- {hotspot}"));
        }
    }
    body.join("\n")
}

fn write_json_artifact<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let encoded = serde_json::to_string_pretty(value)?;
    write_text_file(path, &encoded, true)?;
    Ok(())
}

fn truncate_single_line(value: &str, max_len: usize) -> String {
    let collapsed = value.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    if collapsed.len() <= max_len {
        collapsed
    } else {
        format!("{}...", &collapsed[..max_len])
    }
}

fn parse_json_block<T: for<'de> Deserialize<'de>>(value: &str) -> Result<T> {
    if let Ok(parsed) = serde_json::from_str(value) {
        return Ok(parsed);
    }

    let Some(start) = value.find('{') else {
        bail!("agent output did not contain a JSON object");
    };
    let Some(end) = value.rfind('}') else {
        bail!("agent output did not contain a complete JSON object");
    };
    Ok(serde_json::from_str(&value[start..=end])?)
}

fn resolve_merge_agent_resolution(
    root: &Path,
    args: &MergeArgs,
    prompt: &str,
) -> Result<AgentResolutionArtifact> {
    let config = AppConfig::load()?;
    let planning_meta = PlanningMeta::load(root)?;
    let invocation = resolve_agent_invocation_for_planning(
        &config,
        &planning_meta,
        &crate::cli::RunAgentArgs {
            root: Some(root.to_path_buf()),
            route_key: Some(AGENT_ROUTE_MERGE.to_string()),
            agent: args.agent.clone(),
            prompt: prompt.to_string(),
            instructions: None,
            model: args.model.clone(),
            reasoning: args.reasoning.clone(),
            transport: None,
            attachments: Vec::new(),
        },
    )?;

    Ok(AgentResolutionArtifact {
        provider: invocation.agent,
        model: invocation.model,
        reasoning: invocation.reasoning,
        route_key: invocation.route_key,
        family_key: invocation.family_key,
        provider_source: format_agent_config_source(&invocation.provider_source),
        model_source: invocation
            .model_source
            .map(|source| format_agent_config_source(&source)),
        reasoning_source: invocation
            .reasoning_source
            .map(|source| format_agent_config_source(&source)),
    })
}

fn run_agent_capture_in_dir(
    root: &Path,
    workspace_path: &Path,
    overrides: AgentConfigOverrides,
    prompt: &str,
    extra_env: Vec<(String, String)>,
) -> Result<String> {
    let config = AppConfig::load()?;
    let planning_meta = PlanningMeta::load(root)?;
    let invocation = resolve_agent_invocation_for_planning(
        &config,
        &planning_meta,
        &crate::cli::RunAgentArgs {
            root: Some(root.to_path_buf()),
            route_key: Some(AGENT_ROUTE_MERGE.to_string()),
            agent: overrides.provider,
            prompt: prompt.to_string(),
            instructions: None,
            model: overrides.model,
            reasoning: overrides.reasoning,
            transport: None,
            attachments: Vec::new(),
        },
    )?;
    let command_args = command_args_for_invocation(&invocation, Some(workspace_path))?;
    let attempted_command = validate_invocation_command_surface(&invocation, &command_args)?;

    let mut command = Command::new(&invocation.command);
    command.args(&command_args);
    command.current_dir(workspace_path);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    apply_noninteractive_agent_environment(&mut command);
    apply_invocation_environment(&mut command, &invocation, prompt, None);
    for (key, value) in extra_env {
        command.env(key, value);
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch agent `{}` with command `{attempted_command}`",
            invocation.agent
        )
    })?;

    if invocation.transport == crate::config::PromptTransport::Stdin {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to open stdin for agent `{}`", invocation.agent))?;
        stdin
            .write_all(invocation.payload.as_bytes())
            .with_context(|| {
                format!(
                    "failed to write merge prompt payload to agent `{}`",
                    invocation.agent
                )
            })?;
    }

    let output = child
        .wait_with_output()
        .with_context(|| format!("failed to wait for agent `{}`", invocation.agent))?;
    if !output.status.success() {
        bail!(
            "agent `{}` exited unsuccessfully while running `{attempted_command}`: {}",
            invocation.agent,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

impl GhCli {
    fn resolve_repository(&self, root: &Path) -> Result<GithubRepository> {
        let output = self.run_json::<RepoViewResponse>(
            root,
            &[
                "repo",
                "view",
                "--json",
                "nameWithOwner,url,defaultBranchRef",
            ],
        )?;
        Ok(GithubRepository {
            name_with_owner: output.name_with_owner,
            url: output.url,
            default_branch: output.default_branch_ref.name,
        })
    }

    fn list_open_pull_requests(&self, root: &Path) -> Result<Vec<GithubPullRequest>> {
        self.run_json(
            root,
            &[
                "pr",
                "list",
                "--state",
                "open",
                "--json",
                "number,title,body,url,headRefName,baseRefName,updatedAt,author",
            ],
        )
    }

    fn publish_aggregate_pull_request(
        &self,
        workspace_path: &Path,
        repository: &GithubRepository,
        aggregate_branch: &str,
        base_branch: &str,
        title: &str,
        body_path: &Path,
    ) -> Result<AggregatePublication> {
        let existing = self.run_json::<Vec<ExistingAggregatePullRequest>>(
            workspace_path,
            &[
                "pr",
                "list",
                "--state",
                "open",
                "--head",
                aggregate_branch,
                "--base",
                base_branch,
                "--json",
                "number,url",
            ],
        )?;
        if let Some(pr) = existing.into_iter().next() {
            self.run_plain(
                workspace_path,
                &[
                    "pr",
                    "edit",
                    &pr.number.to_string(),
                    "--title",
                    title,
                    "--body-file",
                    body_path
                        .to_str()
                        .ok_or_else(|| anyhow!("invalid PR body path"))?,
                ],
            )?;
            return Ok(AggregatePublication {
                url: pr.url,
                action: "updated",
            });
        }

        let created = self
            .run_json::<CreatedPullRequest>(
                workspace_path,
                &[
                    "pr",
                    "create",
                    "--base",
                    base_branch,
                    "--head",
                    aggregate_branch,
                    "--title",
                    title,
                    "--body-file",
                    body_path
                        .to_str()
                        .ok_or_else(|| anyhow!("invalid PR body path"))?,
                    "--json",
                    "url",
                ],
            )
            .or_else(|_| {
                self.run_plain(
                    workspace_path,
                    &[
                        "pr",
                        "create",
                        "--base",
                        base_branch,
                        "--head",
                        aggregate_branch,
                        "--title",
                        title,
                        "--body-file",
                        body_path
                            .to_str()
                            .ok_or_else(|| anyhow!("invalid PR body path"))?,
                    ],
                )?;
                let mut prs = self.run_json::<Vec<CreatedPullRequest>>(
                    workspace_path,
                    &[
                        "pr",
                        "list",
                        "--state",
                        "open",
                        "--head",
                        aggregate_branch,
                        "--base",
                        base_branch,
                        "--json",
                        "url",
                    ],
                )?;
                prs.pop().ok_or_else(|| {
                    anyhow!(
                        "gh created an aggregate pull request for `{}` but no open PR was returned",
                        repository.name_with_owner
                    )
                })
            })?;

        Ok(AggregatePublication {
            url: created.url,
            action: "created",
        })
    }

    fn run_json<T: for<'de> Deserialize<'de>>(&self, root: &Path, args: &[&str]) -> Result<T> {
        let output = Command::new("gh")
            .args(args)
            .current_dir(root)
            .output()
            .with_context(|| format!("failed to run `gh {}`", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "gh {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("failed to decode JSON from `gh {}`", args.join(" ")))
    }

    fn run_plain(&self, root: &Path, args: &[&str]) -> Result<()> {
        let output = Command::new("gh")
            .args(args)
            .current_dir(root)
            .output()
            .with_context(|| format!("failed to run `gh {}`", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "gh {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }
}

fn run_git(root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run `git {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .with_context(|| format!("failed to run `git {}`", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anyhow::Result;
    use chrono::TimeZone;
    use tempfile::tempdir;

    use crate::cli::{MergeArgs, MergeDashboardEventArg};
    use crate::fs::PlanningPaths;

    use super::{
        GithubActor, GithubPullRequest, MergePlan, reserve_run_dir_at, validate_merge_plan,
        validation_commands,
    };

    fn empty_merge_args() -> MergeArgs {
        MergeArgs {
            root: PathBuf::from("."),
            json: false,
            no_interactive: false,
            pull_requests: Vec::new(),
            validate: Vec::new(),
            agent: None,
            model: None,
            reasoning: None,
            render_once: false,
            events: Vec::<MergeDashboardEventArg>::new(),
            width: 120,
            height: 32,
        }
    }

    #[test]
    fn reserve_run_dir_appends_suffix_when_timestamp_collides() -> Result<()> {
        let temp = tempdir()?;
        let paths = PlanningPaths::new(temp.path());
        std::fs::create_dir_all(paths.merge_runs_dir.join("20260316T180000Z"))?;
        std::fs::create_dir_all(paths.merge_runs_dir.join("20260316T180000Z-01"))?;

        let timestamp = chrono::Utc
            .with_ymd_and_hms(2026, 3, 16, 18, 0, 0)
            .single()
            .expect("valid timestamp");
        let (run_id, run_dir) = reserve_run_dir_at(&paths, timestamp)?;

        assert_eq!(run_id, "20260316T180000Z-02");
        assert!(run_dir.is_dir());
        Ok(())
    }

    #[test]
    fn validation_commands_prefers_make_quality_over_all() -> Result<()> {
        let temp = tempdir()?;
        std::fs::write(
            temp.path().join("Makefile"),
            ".PHONY: all quality\nall: quality\nquality:\n\tcargo test\n",
        )?;

        let commands = validation_commands(temp.path(), &empty_merge_args())?;

        assert_eq!(commands, vec!["make quality"]);
        Ok(())
    }

    #[test]
    fn validation_commands_falls_back_to_make_all_when_quality_is_missing() -> Result<()> {
        let temp = tempdir()?;
        std::fs::write(
            temp.path().join("Makefile"),
            ".PHONY: all\nall:\n\tcargo test\n",
        )?;

        let commands = validation_commands(temp.path(), &empty_merge_args())?;

        assert_eq!(commands, vec!["make all"]);
        Ok(())
    }

    #[test]
    fn validate_merge_plan_requires_the_full_selected_pull_request_set() {
        let selected = vec![
            GithubPullRequest {
                number: 101,
                title: "PR 101".to_string(),
                body: String::new(),
                url: "https://example.com/101".to_string(),
                head_ref_name: "feature/101".to_string(),
                base_ref_name: "main".to_string(),
                updated_at: "2026-03-16T18:00:00Z".to_string(),
                author: GithubActor {
                    login: "kames".to_string(),
                },
            },
            GithubPullRequest {
                number: 102,
                title: "PR 102".to_string(),
                body: String::new(),
                url: "https://example.com/102".to_string(),
                head_ref_name: "feature/102".to_string(),
                base_ref_name: "main".to_string(),
                updated_at: "2026-03-16T18:00:00Z".to_string(),
                author: GithubActor {
                    login: "kames".to_string(),
                },
            },
        ];

        let error = validate_merge_plan(
            &MergePlan {
                merge_order: vec![101],
                conflict_hotspots: Vec::new(),
                summary: "subset".to_string(),
            },
            &selected,
        )
        .expect_err("subset merge plan should fail");

        assert!(
            error
                .to_string()
                .contains("must return the full selected pull request set")
        );
    }

    #[test]
    fn validate_merge_plan_rejects_duplicate_pull_requests() {
        let selected = vec![GithubPullRequest {
            number: 101,
            title: "PR 101".to_string(),
            body: String::new(),
            url: "https://example.com/101".to_string(),
            head_ref_name: "feature/101".to_string(),
            base_ref_name: "main".to_string(),
            updated_at: "2026-03-16T18:00:00Z".to_string(),
            author: GithubActor {
                login: "kames".to_string(),
            },
        }];

        let error = validate_merge_plan(
            &MergePlan {
                merge_order: vec![101, 101],
                conflict_hotspots: Vec::new(),
                summary: "duplicate".to_string(),
            },
            &selected,
        )
        .expect_err("duplicate merge plan should fail");

        assert!(
            error
                .to_string()
                .contains("duplicate pull requests in the merge order")
        );
    }
}

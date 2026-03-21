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
use crate::github_pr::{
    GhCli, PullRequestLifecycleAction, PullRequestPublishMode, PullRequestPublishRequest,
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
const VALIDATION_TEST_FAILURE_MARKERS: &[&str] = &[
    "test result: FAILED",
    "error: test failed, to rerun pass",
    "timed out waiting for",
];

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MergeContextArtifact {
    run_id: String,
    repository: GithubRepository,
    selected_pull_requests: Vec<GithubPullRequest>,
    source_root: String,
    workspace_path: String,
    aggregate_branch: String,
    agent_resolution: AgentResolutionArtifact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MergeProgressArtifact {
    run: ProgressArtifact,
    steps: Vec<MergeStepRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MergeStepRecord {
    pull_request: u64,
    status: String,
    detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ValidationArtifact {
    attempts: Vec<ValidationAttemptRecord>,
    success: bool,
    repair_attempts: usize,
    final_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ValidationAttemptRecord {
    attempt: usize,
    commands: Vec<ValidationCommandRecord>,
    repair: Option<ValidationRepairRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ValidationCommandRecord {
    command: String,
    exit_code: i32,
    stdout: String,
    stderr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ValidationRepairRecord {
    attempt: usize,
    commit: Option<String>,
}

#[derive(Debug, Clone)]
struct ValidationFailureSummary {
    signature: String,
    command: String,
    test_name: Option<String>,
    rerun_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PublicationArtifact {
    aggregate_branch: String,
    title: String,
    url: String,
    action: String,
    validation_success: bool,
}

#[derive(Debug, Clone)]
struct AggregatePublication {
    url: String,
    action: &'static str,
}

fn resolve_repository(gh: &GhCli, root: &Path) -> Result<GithubRepository> {
    let output = gh.run_json::<RepoViewResponse>(
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

fn list_open_pull_requests(gh: &GhCli, root: &Path) -> Result<Vec<GithubPullRequest>> {
    gh.run_json(
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

pub async fn run_merge(args: &MergeArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let app_config = AppConfig::load()?;
    let _planning_meta = load_required_planning_meta(&root, "merge")?;
    ensure_planning_layout(&root, false)?;

    let gh = GhCli;
    if let Some(run_id) = &args.resume_run {
        let run = resume_merge_run(&root, args, &gh, run_id)?;
        let publication_verb = match run.publication.action.as_str() {
            "updated" => "Updated",
            _ => "Created",
        };
        if run.validation_success {
            println!(
                "{publication_verb} aggregate PR {} for {} pull request(s). Run artifacts saved in {}",
                run.publication.url,
                run.selected_count,
                run.run_dir.display()
            );
        } else {
            println!(
                "{publication_verb} aggregate PR {} for {} pull request(s), but validation still needs attention. Run artifacts saved in {}",
                run.publication.url,
                run.selected_count,
                run.run_dir.display()
            );
        }
        return Ok(());
    }
    let repository = resolve_repository(&gh, &root)?;
    let pull_requests = list_open_pull_requests(&gh, &root)?;

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
                vim_mode: app_config.vim_mode_enabled(),
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
                vim_mode: app_config.vim_mode_enabled(),
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
    if run.validation_success {
        println!(
            "{publication_verb} aggregate PR {} for {} pull request(s). Run artifacts saved in {}",
            run.publication.url,
            run.selected_count,
            run.run_dir.display()
        );
    } else {
        println!(
            "{publication_verb} aggregate PR {} for {} pull request(s), but validation still needs attention. Run artifacts saved in {}",
            run.publication.url,
            run.selected_count,
            run.run_dir.display()
        );
    }
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
    validation_success: bool,
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

    let validation_commands = validation_commands(root, args);
    tracker.start_step(
        STEP_VALIDATE,
        match &validation_commands {
            Ok(commands) => format!("Running validation command(s): {}", commands.join(" && ")),
            Err(error) => format!("Preparing validation commands failed: {error:#}"),
        },
    )?;
    let merge_settings = AppConfig::load()?.merge;
    let max_validation_repair_attempts = merge_settings.validation_repair_attempts();
    let max_validation_transient_retry_attempts =
        merge_settings.validation_transient_retry_attempts();
    let validation = match validation_commands {
        Ok(commands) => match run_validation_until_passes(
            root,
            &workspace_path,
            args,
            repository,
            &aggregate_branch,
            &selected_pull_requests,
            &plan,
            &run_dir,
            &mut tracker,
            commands,
            max_validation_repair_attempts,
            max_validation_transient_retry_attempts,
        ) {
            Ok(validation) => validation,
            Err(error) => ValidationArtifact {
                attempts: Vec::new(),
                success: false,
                repair_attempts: 0,
                final_error: Some(format!("Validation execution failed: {error:#}")),
            },
        },
        Err(error) => ValidationArtifact {
            attempts: Vec::new(),
            success: false,
            repair_attempts: 0,
            final_error: Some(format!("Validation setup failed: {error:#}")),
        },
    };
    write_json_artifact(&run_dir.join("validation.json"), &validation)?;
    if validation.success {
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
    } else {
        tracker.fail_step(
            STEP_VALIDATE,
            validation.final_error.clone().unwrap_or_else(|| {
                "Validation remained failing after automated recovery.".to_string()
            }),
            None,
        )?;
    }

    tracker.start_step(
        STEP_PUSH,
        format!("Pushing aggregate branch `{aggregate_branch}` to origin."),
    )?;
    if let Err(error) = push_aggregate_branch_until_published(
        &workspace_path,
        &aggregate_branch,
        merge_settings.publication_retry_attempts(),
        &mut tracker,
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
    let pr_body = aggregate_pr_body(
        repository,
        &selected_pull_requests,
        &plan,
        &run_id,
        &validation,
    );
    let pr_body_path = run_dir.join("aggregate-pr-body.md");
    write_text_file(&pr_body_path, &pr_body, true)?;
    tracker.start_step(
        STEP_PUBLISH,
        format!(
            "Publishing the aggregate pull request into `{}`.",
            repository.default_branch
        ),
    )?;
    let publication = match publish_aggregate_pull_request_until_published(
        gh,
        &workspace_path,
        repository,
        &aggregate_branch,
        &repository.default_branch,
        &pr_title,
        &pr_body_path,
        merge_settings.publication_retry_attempts(),
        &mut tracker,
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
        validation_success: validation.success,
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
        "{} aggregate pull request {}. {}",
        match publication_artifact.action.as_str() {
            "updated" => "Updated",
            _ => "Created",
        },
        publication_artifact.url,
        if validation.success {
            "Validation passed; review the run artifacts for planner, validation, and publication details.".to_string()
        } else {
            "Validation remains unresolved; review the run artifacts and aggregate PR for the current failure details.".to_string()
        }
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
        validation_success: validation.success,
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

fn resume_merge_run(
    root: &Path,
    args: &MergeArgs,
    gh: &GhCli,
    run_id: &str,
) -> Result<MergeExecution> {
    let paths = PlanningPaths::new(root);
    let run_dir = paths.merge_run_dir(run_id);
    if !run_dir.is_dir() {
        bail!(
            "merge run `{run_id}` does not exist under `{}`",
            paths.merge_runs_dir.display()
        );
    }

    let context: MergeContextArtifact = read_json_artifact(&run_dir.join("context.json"))?;
    let plan: MergePlan = read_json_artifact(&run_dir.join("plan.json"))?;
    let mut progress: MergeProgressArtifact =
        read_json_artifact(&run_dir.join("merge-progress.json"))?;
    let workspace_path = PathBuf::from(&context.workspace_path);
    if !workspace_path.is_dir() {
        bail!(
            "merge workspace for run `{run_id}` is missing at `{}`",
            workspace_path.display()
        );
    }

    let mut tracker =
        ProgressTracker::resume(run_dir.join("progress.json"), ProgressOutputMode::Text)?;
    tracker.update_detail(
        STEP_VALIDATE,
        format!("Resuming merge run `{run_id}` from the existing aggregate workspace."),
        None,
    )?;

    let merge_settings = AppConfig::load()?.merge;
    let validation_commands = validation_commands(root, args);
    let validation = match validation_commands {
        Ok(commands) => match run_validation_until_passes(
            root,
            &workspace_path,
            args,
            &context.repository,
            &context.aggregate_branch,
            &context.selected_pull_requests,
            &plan,
            &run_dir,
            &mut tracker,
            commands,
            merge_settings.validation_repair_attempts(),
            merge_settings.validation_transient_retry_attempts(),
        ) {
            Ok(validation) => validation,
            Err(error) => ValidationArtifact {
                attempts: Vec::new(),
                success: false,
                repair_attempts: 0,
                final_error: Some(format!("Validation execution failed: {error:#}")),
            },
        },
        Err(error) => ValidationArtifact {
            attempts: Vec::new(),
            success: false,
            repair_attempts: 0,
            final_error: Some(format!("Validation setup failed: {error:#}")),
        },
    };
    write_json_artifact(&run_dir.join("validation.json"), &validation)?;
    if validation.success {
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
    } else {
        tracker.fail_step(
            STEP_VALIDATE,
            validation.final_error.clone().unwrap_or_else(|| {
                "Validation remained failing after automated recovery.".to_string()
            }),
            None,
        )?;
    }

    tracker.start_step(
        STEP_PUSH,
        format!(
            "Synchronizing aggregate branch `{}` to origin after resume.",
            context.aggregate_branch
        ),
    )?;
    push_aggregate_branch_until_published(
        &workspace_path,
        &context.aggregate_branch,
        merge_settings.publication_retry_attempts(),
        &mut tracker,
    )?;
    tracker.complete_step(
        STEP_PUSH,
        format!(
            "Aggregate branch `{}` is now on origin.",
            context.aggregate_branch
        ),
    )?;

    let pr_title = aggregate_pr_title(&context.selected_pull_requests);
    let pr_body = aggregate_pr_body(
        &context.repository,
        &context.selected_pull_requests,
        &plan,
        &context.run_id,
        &validation,
    );
    let pr_body_path = run_dir.join("aggregate-pr-body.md");
    write_text_file(&pr_body_path, &pr_body, true)?;
    tracker.start_step(
        STEP_PUBLISH,
        format!(
            "Publishing the aggregate pull request into `{}`.",
            context.repository.default_branch
        ),
    )?;
    let publication = publish_aggregate_pull_request_until_published(
        gh,
        &workspace_path,
        &context.repository,
        &context.aggregate_branch,
        &context.repository.default_branch,
        &pr_title,
        &pr_body_path,
        merge_settings.publication_retry_attempts(),
        &mut tracker,
    )?;
    let publication_artifact = PublicationArtifact {
        aggregate_branch: context.aggregate_branch.clone(),
        title: pr_title,
        url: publication.url,
        action: publication.action.to_string(),
        validation_success: validation.success,
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
        "{} aggregate pull request {}. {}",
        match publication_artifact.action.as_str() {
            "updated" => "Updated",
            _ => "Created",
        },
        publication_artifact.url,
        if validation.success {
            "Validation passed; review the run artifacts for planner, validation, and publication details.".to_string()
        } else {
            "Validation remains unresolved; review the run artifacts and aggregate PR for the current failure details.".to_string()
        }
    ))?;

    progress.run = tracker.artifact().clone();
    write_json_artifact(&run_dir.join("merge-progress.json"), &progress)?;

    Ok(MergeExecution {
        run_dir,
        publication: publication_artifact,
        selected_count: context.selected_pull_requests.len(),
        validation_success: validation.success,
    })
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
    max_validation_repair_attempts: usize,
    max_validation_transient_retry_attempts: usize,
) -> Result<ValidationArtifact> {
    let mut attempts = Vec::new();
    let mut repair_attempts_used = 0usize;
    let mut transient_retry_streak = 0usize;
    let mut last_failure_signature: Option<String> = None;
    let mut last_failure_summary: Option<ValidationFailureSummary> = None;
    let mut last_repair_signature: Option<String> = None;
    let mut last_repair_had_changes = false;

    loop {
        let attempt = attempts.len() + 1;
        let commands_to_run =
            targeted_validation_commands(&validation_commands, last_failure_summary.as_ref());
        let commands = run_validation_commands(workspace_path, commands_to_run)?;
        let failing_command = commands
            .iter()
            .find(|record| record.exit_code != 0)
            .map(|record| format!("`{}` exited with {}", record.command, record.exit_code));
        let failure_summary = validation_failure_summary(&commands);
        let failure_signature = failure_summary
            .as_ref()
            .map(|summary| summary.signature.clone());
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
                repair_attempts: repair_attempts_used,
                final_error: None,
            };
            write_json_artifact(&run_dir.join("validation.json"), &artifact)?;
            return Ok(artifact);
        }

        let repeated_failure_signature = failure_signature
            .as_deref()
            .zip(last_failure_signature.as_deref())
            .is_some_and(|(current, previous)| current == previous);
        if validation_failure_looks_transient(&attempt_record.commands)
            && !repeated_failure_signature
            && transient_retry_streak < max_validation_transient_retry_attempts
        {
            transient_retry_streak += 1;
            last_failure_signature = failure_signature;
            last_failure_summary = failure_summary;
            attempts.push(attempt_record);
            write_json_artifact(
                &run_dir.join("validation.json"),
                &ValidationArtifact {
                    attempts: attempts.clone(),
                    success: false,
                    repair_attempts: repair_attempts_used,
                    final_error: None,
                },
            )?;
            tracker.update_detail(
                STEP_VALIDATE,
                format!(
                    "Validation attempt {attempt} failed on {}. Retrying without repair to rule out a transient test failure ({transient_retry_streak}/{max_validation_transient_retry_attempts}).",
                    failing_command
                        .clone()
                        .unwrap_or_else(|| "a validation command failed".to_string())
                ),
                None,
            )?;
            continue;
        }

        transient_retry_streak = 0;
        last_failure_signature = failure_signature.clone();
        last_failure_summary = failure_summary;

        if failure_signature == last_repair_signature && !last_repair_had_changes {
            attempts.push(attempt_record);
            let detail = format!(
                "validation is stuck on repeated failure signature `{}` after a no-op repair",
                failure_signature.unwrap_or_else(|| "unknown failure".to_string())
            );
            let artifact = ValidationArtifact {
                attempts,
                success: false,
                repair_attempts: repair_attempts_used,
                final_error: Some(detail),
            };
            write_json_artifact(&run_dir.join("validation.json"), &artifact)?;
            return Ok(artifact);
        }

        if repair_attempts_used >= max_validation_repair_attempts {
            attempts.push(attempt_record);
            let detail = format!(
                "validation failed for aggregate branch `{aggregate_branch}` after {} repair attempt(s); last failure: {}",
                repair_attempts_used,
                failing_command.unwrap_or_else(|| "a validation command failed".to_string())
            );
            let artifact = ValidationArtifact {
                attempts,
                success: false,
                repair_attempts: repair_attempts_used,
                final_error: Some(detail),
            };
            write_json_artifact(&run_dir.join("validation.json"), &artifact)?;
            return Ok(artifact);
        }

        let repair_attempt = repair_attempts_used + 1;
        tracker.update_detail(
            STEP_VALIDATE,
            format!(
                "Validation attempt {attempt} failed on {}. Invoking repair assistance ({repair_attempt}/{max_validation_repair_attempts}).",
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
            max_validation_repair_attempts,
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
        repair_attempts_used += 1;
        last_repair_signature = last_failure_signature.clone();
        last_repair_had_changes = attempt_record
            .repair
            .as_ref()
            .and_then(|repair| repair.commit.as_ref())
            .is_some();
        if attempt_record
            .repair
            .as_ref()
            .and_then(|repair| repair.commit.as_ref())
            .is_some()
        {
            last_failure_signature = None;
            last_failure_summary = None;
        }
        attempts.push(attempt_record);
        write_json_artifact(
            &run_dir.join("validation.json"),
            &ValidationArtifact {
                attempts: attempts.clone(),
                success: false,
                repair_attempts: repair_attempts_used,
                final_error: None,
            },
        )?;
    }
}

fn validation_failure_looks_transient(commands: &[ValidationCommandRecord]) -> bool {
    commands
        .iter()
        .filter(|record| record.exit_code != 0)
        .any(|record| {
            let combined = format!("{}\n{}", record.stdout, record.stderr);
            VALIDATION_TEST_FAILURE_MARKERS
                .iter()
                .any(|marker| combined.contains(marker))
        })
}

fn validation_failure_summary(
    commands: &[ValidationCommandRecord],
) -> Option<ValidationFailureSummary> {
    if let Some(record) = commands.iter().find(|record| record.exit_code != 0) {
        let mut test_name = None;
        let mut rerun_hint = None;
        for line in record.stdout.lines().chain(record.stderr.lines()) {
            let trimmed = line.trim();
            if test_name.is_none() {
                if let Some(name) = trimmed
                    .strip_prefix("---- ")
                    .and_then(|value| value.strip_suffix(" stdout ----"))
                {
                    test_name = Some(name.to_string());
                } else if let Some(name) = trimmed
                    .strip_prefix("test ")
                    .and_then(|value| value.strip_suffix(" ... FAILED"))
                {
                    test_name = Some(name.to_string());
                }
            }
            if rerun_hint.is_none() {
                if let Some(hint) = trimmed
                    .strip_prefix("error: test failed, to rerun pass `")
                    .and_then(|value| value.strip_suffix('`'))
                {
                    rerun_hint = Some(hint.to_string());
                }
            }
            if trimmed.starts_with("error[E") || trimmed.starts_with("Error: ") {
                return Some(ValidationFailureSummary {
                    signature: format!(
                        "{}:{}",
                        test_name.clone().unwrap_or_else(|| record.command.clone()),
                        trimmed
                    ),
                    command: record.command.clone(),
                    test_name,
                    rerun_hint,
                });
            }
        }
        Some(ValidationFailureSummary {
            signature: validation_failure_signature(commands)
                .unwrap_or_else(|| format!("command:{}:{}", record.command, record.exit_code)),
            command: record.command.clone(),
            test_name,
            rerun_hint,
        })
    } else {
        None
    }
}

fn validation_failure_signature(commands: &[ValidationCommandRecord]) -> Option<String> {
    if let Some(record) = commands.iter().find(|record| record.exit_code != 0) {
        for line in record.stdout.lines().chain(record.stderr.lines()) {
            let trimmed = line.trim();
            if let Some(name) = trimmed
                .strip_prefix("---- ")
                .and_then(|value| value.strip_suffix(" stdout ----"))
            {
                return Some(format!("test:{name}"));
            }
            if let Some(name) = trimmed
                .strip_prefix("test ")
                .and_then(|value| value.strip_suffix(" ... FAILED"))
            {
                return Some(format!("test:{name}"));
            }
            if trimmed.starts_with("error[E") {
                return Some(trimmed.to_string());
            }
            if trimmed.starts_with("Error: ") {
                return Some(trimmed.to_string());
            }
        }
        Some(format!("command:{}:{}", record.command, record.exit_code))
    } else {
        None
    }
}

fn targeted_validation_commands(
    validation_commands: &[String],
    last_failure: Option<&ValidationFailureSummary>,
) -> Vec<String> {
    let Some(summary) = last_failure else {
        return validation_commands.to_vec();
    };
    if validation_commands.len() != 1 || summary.command != validation_commands[0] {
        return validation_commands.to_vec();
    }

    if summary.command == "make quality" {
        if let Some(test_name) = &summary.test_name {
            if let Some(rerun_hint) = &summary.rerun_hint {
                if rerun_hint.starts_with("--test ") {
                    return vec![
                        format!("cargo test {rerun_hint} {test_name} -- --exact --nocapture"),
                        validation_commands[0].clone(),
                    ];
                }
            }
            return vec![
                format!("cargo test {test_name} -- --exact --nocapture"),
                validation_commands[0].clone(),
            ];
        }
        if summary.signature.starts_with("error[E") {
            return vec![
                "cargo clippy --all-targets --all-features -- -D warnings".to_string(),
                validation_commands[0].clone(),
            ];
        }
    }

    validation_commands.to_vec()
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

fn push_aggregate_branch_until_published(
    workspace_path: &Path,
    aggregate_branch: &str,
    max_attempts: usize,
    tracker: &mut ProgressTracker,
) -> Result<()> {
    for attempt in 1..=max_attempts {
        match run_git(
            workspace_path,
            &["push", "--set-upstream", "origin", aggregate_branch],
        ) {
            Ok(()) => return Ok(()),
            Err(error) if attempt < max_attempts => {
                tracker.update_detail(
                    STEP_PUSH,
                    format!(
                        "Push attempt {attempt}/{max_attempts} for `{aggregate_branch}` failed: {error:#}. Retrying."
                    ),
                    None,
                )?;
            }
            Err(error) => return Err(error),
        }
    }

    bail!("push retry loop terminated unexpectedly for `{aggregate_branch}`")
}

#[allow(clippy::too_many_arguments)]
fn publish_aggregate_pull_request_until_published(
    gh: &GhCli,
    workspace_path: &Path,
    repository: &GithubRepository,
    aggregate_branch: &str,
    base_branch: &str,
    title: &str,
    body_path: &Path,
    max_attempts: usize,
    tracker: &mut ProgressTracker,
) -> Result<AggregatePublication> {
    for attempt in 1..=max_attempts {
        match gh.publish_aggregate_pull_request(
            workspace_path,
            repository,
            aggregate_branch,
            base_branch,
            title,
            body_path,
        ) {
            Ok(publication) => return Ok(publication),
            Err(error) if attempt < max_attempts => {
                tracker.update_detail(
                    STEP_PUBLISH,
                    format!(
                        "Aggregate PR publication attempt {attempt}/{max_attempts} failed: {error:#}. Retrying."
                    ),
                    None,
                )?;
            }
            Err(error) => return Err(error),
        }
    }

    bail!("aggregate PR publication retry loop terminated unexpectedly")
}

#[allow(clippy::too_many_arguments)]
fn build_validation_repair_prompt(
    repository: &GithubRepository,
    aggregate_branch: &str,
    selected_pull_requests: &[GithubPullRequest],
    plan: &MergePlan,
    workspace_path: &Path,
    repair_attempt: usize,
    commands: &[ValidationCommandRecord],
    max_validation_repair_attempts: usize,
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
        format!("Validation repair attempt: {repair_attempt}/{max_validation_repair_attempts}"),
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
    validation: &ValidationArtifact,
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
    body.push(String::new());
    body.push("Validation status:".to_string());
    if validation.success {
        body.push(format!(
            "- passed after {} attempt(s) and {} repair commit(s)",
            validation.attempts.len(),
            validation.repair_attempts
        ));
    } else {
        body.push("- failing; aggregate branch published for continued repair".to_string());
        if let Some(error) = &validation.final_error {
            body.push(format!("- latest failure: {error}"));
        }
    }
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

fn read_json_artifact<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("failed to parse `{}`", path.display()))
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
    fn publish_aggregate_pull_request(
        &self,
        workspace_path: &Path,
        repository: &GithubRepository,
        aggregate_branch: &str,
        base_branch: &str,
        title: &str,
        body_path: &Path,
    ) -> Result<AggregatePublication> {
        let created = self.publish_branch_pull_request(
            workspace_path,
            PullRequestPublishRequest {
                head_branch: aggregate_branch,
                base_branch,
                title,
                body_path,
                mode: PullRequestPublishMode::Ready,
            },
        )?;

        Ok(AggregatePublication {
            url: created.url,
            action: match created.action {
                PullRequestLifecycleAction::CreatedReady => "created",
                PullRequestLifecycleAction::UpdatedExisting => "updated",
                _ => bail!(
                    "unexpected aggregate PR lifecycle action `{:?}` for `{}`",
                    created.action,
                    repository.name_with_owner
                ),
            },
        })
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
        GithubActor, GithubPullRequest, MergePlan, ValidationCommandRecord, reserve_run_dir_at,
        targeted_validation_commands, validate_merge_plan, validation_commands,
        validation_failure_summary,
    };

    fn empty_merge_args() -> MergeArgs {
        MergeArgs {
            root: PathBuf::from("."),
            json: false,
            no_interactive: false,
            resume_run: None,
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
    fn validation_failure_summary_extracts_test_name_and_rerun_hint() {
        let summary = validation_failure_summary(&[ValidationCommandRecord {
            command: "make quality".to_string(),
            exit_code: 2,
            stdout: "test plan_interactive_preserves_explicit_builtin_overrides_across_resumed_phases ... FAILED\n".to_string(),
            stderr: "error: test failed, to rerun pass `--test plan`\n".to_string(),
        }])
        .expect("summary should parse");

        assert_eq!(
            summary.test_name.as_deref(),
            Some("plan_interactive_preserves_explicit_builtin_overrides_across_resumed_phases")
        );
        assert_eq!(summary.rerun_hint.as_deref(), Some("--test plan"));
    }

    #[test]
    fn targeted_validation_commands_prepend_exact_test_rerun_for_make_quality() {
        let summary = validation_failure_summary(&[ValidationCommandRecord {
            command: "make quality".to_string(),
            exit_code: 2,
            stdout: "test plan_interactive_preserves_explicit_builtin_overrides_across_resumed_phases ... FAILED\n".to_string(),
            stderr: "error: test failed, to rerun pass `--test plan`\n".to_string(),
        }])
        .expect("summary should parse");

        let commands = targeted_validation_commands(&["make quality".to_string()], Some(&summary));
        assert_eq!(
            commands,
            vec![
                "cargo test --test plan plan_interactive_preserves_explicit_builtin_overrides_across_resumed_phases -- --exact --nocapture".to_string(),
                "make quality".to_string()
            ]
        );
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

use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::sleep;

use crate::agent_provider::builtin_provider_adapter;
use crate::agents::{
    AgentExecutionOptions, AgentTokenUsage, apply_invocation_environment,
    apply_noninteractive_agent_environment, command_args_for_invocation,
    command_args_for_invocation_with_options, render_invocation_diagnostics,
    resolve_agent_invocation_for_planning, validate_invocation_command_surface,
};
use crate::backlog::load_issue_metadata;
use crate::cli::{ListenWorkerArgs, RunAgentArgs};
use crate::config::{
    AGENT_ROUTE_AGENTS_LISTEN, AppConfig, LinearConfig, LinearConfigOverrides, PromptTransport,
};
use crate::fs::{PlanningPaths, canonicalize_existing_dir, write_text_file};
use crate::github_pr::{
    GhCli, PullRequestLifecycleAction, PullRequestLifecycleResult, PullRequestPublishMode,
    PullRequestPublishRequest,
};
use crate::linear::{
    AttachmentCreateRequest, IssueListFilters, IssueSummary, LinearClient, LinearService,
    ReqwestLinearClient, WorkflowState,
};
use crate::repo_target::RepoTarget;
use crate::workflow_contract::render_workflow_contract;

use super::{
    BACKLOG_STATE, LatestResumeHandle, MAX_STALLED_TURNS, ResumeProvider, SessionPhase, TokenUsage,
    agent_log_path, backlog_progress_for_issue_dir, capture_workspace_snapshot,
    compact_blocked_summary, compact_completed_summary, compact_running_summary,
    compare_workspace_snapshots, current_workspace_branch, issue_state_label, issue_team_key,
    listen_issue_is_active, now_epoch_seconds, now_timestamp, preflight, render_agent_prompt,
    try_transition_issue_to_review_state, workspace_has_meaningful_progress, write_listen_session,
};

const REQUIRED_LISTEN_PR_LABEL: &str = "metastack";
const LEGACY_LISTEN_PR_LABEL: &str = "symphony";
const REQUIRED_LISTEN_PR_LABEL_COLOR: &str = "0e8a16";
const REQUIRED_LISTEN_PR_LABEL_DESCRIPTION: &str = "MetaStack automation";
const LISTEN_PULL_REQUEST_BASE_BRANCH: &str = "main";

pub(super) async fn run_listen_worker(args: &ListenWorkerArgs) -> Result<()> {
    let source_root = canonicalize_existing_dir(&args.source_root)?;
    let workspace_path = canonicalize_existing_dir(&args.workspace)?;
    let planning_meta = crate::config::PlanningMeta::load(&source_root)?;
    let project_selector = args
        .project
        .as_deref()
        .or(planning_meta.linear.project_id.as_deref());
    let app_config = AppConfig::load()?;
    let linear_config = LinearConfig::new_with_root(
        Some(&source_root),
        LinearConfigOverrides {
            api_key: args.api_key.clone(),
            api_url: args.api_url.clone(),
            default_team: args.team.clone(),
            profile: args.profile.clone(),
        },
    )?;
    let service = LinearService::new(
        ReqwestLinearClient::new(linear_config.clone())?,
        linear_config.default_team.clone(),
    );
    let branch = current_workspace_branch(&workspace_path).ok();
    let worker_pid = std::process::id();
    let mut turns_completed = 0u32;
    let mut issue = load_worker_issue(&service, &args.issue).await?;
    let backlog_issue = match args.backlog_issue.as_deref() {
        Some(identifier) => Some(load_worker_backlog_issue(
            &workspace_path,
            identifier,
            &issue,
        )?),
        None => None,
    };
    let turn_context = ListenTurnContext {
        app_config: &app_config,
        planning_meta: &planning_meta,
        args,
        source_root: &source_root,
        project_selector,
        workspace_path: &workspace_path,
        workpad_comment_id: &args.workpad_comment_id,
        backlog_issue: backlog_issue.as_ref(),
        max_turns: args.max_turns,
    };
    let mut session_context = WorkerSessionContext {
        source_root: &source_root,
        project_selector,
        workspace_path: &workspace_path,
        branch: branch.as_deref(),
        workpad_comment_id: &args.workpad_comment_id,
        backlog_issue: backlog_issue.as_ref(),
        pid: Some(worker_pid),
        latest_resume_handle: None,
    };
    let mut session_tokens =
        load_existing_session_tokens(&source_root, project_selector, &args.issue)?;
    let mut session_id = load_existing_session_id(&source_root, project_selector, &args.issue)?;
    let mut saw_implementation_progress = workspace_has_meaningful_progress(&workspace_path)?;
    let mut stalled_turns = 0u32;
    let log_path = agent_log_path(&source_root, args.project.as_deref(), &args.issue);
    if let Err(error) = preflight::run_listen_preflight(
        &service,
        &linear_config,
        &app_config,
        &planning_meta,
        preflight::ListenPreflightRequest {
            working_dir: &workspace_path,
            agent: args.agent.as_deref(),
            model: args.model.as_deref(),
            reasoning: args.reasoning.as_deref(),
            require_write_access: true,
        },
    )
    .await
    {
        write_preflight_failure(&log_path, &error)?;
        let backlog_progress = backlog_issue
            .as_ref()
            .map(|backlog_issue| {
                backlog_progress_for_issue_dir(&workspace_path, &backlog_issue.identifier)
            })
            .transpose()?;
        write_listen_session(
            &source_root,
            project_selector,
            build_worker_session(
                &issue,
                SessionPhase::Blocked,
                compact_blocked_summary(
                    "Blocked | missing exec capability",
                    backlog_progress.as_ref(),
                    &log_path,
                ),
                &session_context,
                turns_completed,
                session_id.as_deref(),
                &session_tokens,
            ),
        )?;
        return Err(error);
    }
    loop {
        if !listen_issue_is_active(issue.state.as_ref().map(|state| state.name.as_str())) {
            write_listen_session(
                &source_root,
                project_selector,
                build_worker_session(
                    &issue,
                    SessionPhase::Completed,
                    compact_completed_summary(None, turns_completed, &issue_state_label(&issue)),
                    &session_context,
                    turns_completed,
                    session_id.as_deref(),
                    &session_tokens,
                ),
            )?;
            return Ok(());
        }

        if turns_completed >= args.max_turns {
            let backlog_progress = backlog_issue
                .as_ref()
                .map(|backlog_issue| {
                    backlog_progress_for_issue_dir(&workspace_path, &backlog_issue.identifier)
                })
                .transpose()?;
            write_listen_session(
                &source_root,
                project_selector,
                build_worker_session(
                    &issue,
                    SessionPhase::Blocked,
                    compact_blocked_summary(
                        "Blocked | turn limit reached",
                        backlog_progress.as_ref(),
                        &log_path,
                    ),
                    &session_context,
                    turns_completed,
                    session_id.as_deref(),
                    &session_tokens,
                ),
            )?;
            return Ok(());
        }

        let turn_number = turns_completed + 1;
        let snapshot_before = capture_workspace_snapshot(&workspace_path, &args.issue)?;
        let backlog_progress_before = backlog_issue
            .as_ref()
            .map(|backlog_issue| {
                backlog_progress_for_issue_dir(&workspace_path, &backlog_issue.identifier)
            })
            .transpose()?;
        session_context.latest_resume_handle = None;
        write_listen_session(
            &source_root,
            project_selector,
            build_worker_session(
                &issue,
                SessionPhase::Running,
                compact_running_summary(
                    backlog_progress_before.as_ref(),
                    turn_number,
                    args.max_turns,
                    0,
                ),
                &session_context,
                turns_completed,
                session_id.as_deref(),
                &session_tokens,
            ),
        )?;

        let session_id_state = RefCell::new(session_id.clone());
        let turn_result = match execute_agent_turn(
            &issue,
            turn_number,
            &turn_context,
            |current_session_id| {
                if session_id_state.borrow().as_deref() == Some(current_session_id) {
                    return Ok(());
                }
                *session_id_state.borrow_mut() = Some(current_session_id.to_string());
                write_listen_session(
                    &source_root,
                    project_selector,
                    build_worker_session(
                        &issue,
                        SessionPhase::Running,
                        compact_running_summary(
                            backlog_progress_before.as_ref(),
                            turn_number,
                            args.max_turns,
                            0,
                        ),
                        &session_context,
                        turns_completed,
                        session_id_state.borrow().as_deref(),
                        &session_tokens,
                    ),
                )
            },
            |usage| {
                let mut displayed_tokens = session_tokens.clone();
                displayed_tokens.accumulate(&TokenUsage {
                    input: usage.input,
                    output: usage.output,
                });
                write_listen_session(
                    &source_root,
                    project_selector,
                    build_worker_session(
                        &issue,
                        SessionPhase::Running,
                        compact_running_summary(
                            backlog_progress_before.as_ref(),
                            turn_number,
                            args.max_turns,
                            0,
                        ),
                        &session_context,
                        turns_completed,
                        session_id_state.borrow().as_deref(),
                        &displayed_tokens,
                    ),
                )
            },
        ) {
            Ok(result) => result,
            Err(error) => {
                write_listen_session(
                    &source_root,
                    project_selector,
                    build_worker_session(
                        &issue,
                        SessionPhase::Blocked,
                        compact_blocked_summary(
                            &format!("Blocked | turn {turn_number}/{} failed", args.max_turns),
                            backlog_progress_before.as_ref(),
                            &log_path,
                        ),
                        &session_context,
                        turns_completed,
                        session_id.as_deref(),
                        &session_tokens,
                    ),
                )?;
                return Err(error);
            }
        };
        session_context.latest_resume_handle = turn_result.latest_resume_handle;
        session_id = turn_result
            .session_id
            .or_else(|| session_id_state.into_inner());
        if let Some(usage) = turn_result.usage {
            session_tokens.accumulate(&TokenUsage {
                input: usage.input,
                output: usage.output,
            });
        }

        turns_completed = turn_number;
        let snapshot_after = capture_workspace_snapshot(&workspace_path, &args.issue)?;
        let turn_progress =
            compare_workspace_snapshots(&workspace_path, &snapshot_before, &snapshot_after)?;
        issue = load_worker_issue(&service, &args.issue).await?;

        if !listen_issue_is_active(issue.state.as_ref().map(|state| state.name.as_str())) {
            continue;
        }

        let backlog_progress = backlog_issue
            .as_ref()
            .map(|backlog_issue| {
                backlog_progress_for_issue_dir(&workspace_path, &backlog_issue.identifier)
            })
            .transpose()?;
        if turn_progress.implementation_changed() {
            saw_implementation_progress = true;
            stalled_turns = 0;
        } else {
            stalled_turns += 1;
        }

        if let Some(progress) = backlog_progress {
            if progress.is_complete() {
                if !saw_implementation_progress {
                    write_listen_session(
                        &source_root,
                        project_selector,
                        build_worker_session(
                            &issue,
                            SessionPhase::Blocked,
                            compact_blocked_summary(
                                "Blocked | backlog complete without code changes",
                                Some(&progress),
                                &log_path,
                            ),
                            &session_context,
                            turns_completed,
                            session_id.as_deref(),
                            &session_tokens,
                        ),
                    )?;
                    return Ok(());
                }

                let branch = branch.as_deref().ok_or_else(|| {
                    anyhow!("failed to inspect the workspace branch before promoting the review PR")
                })?;
                let _ = prepare_listener_pull_request_for_review(
                    &service,
                    &issue,
                    &workspace_path,
                    branch,
                )
                .await?;
                let transitioned_issue =
                    try_transition_issue_to_review_state(&service, &issue).await?;
                if let Some(backlog_issue) = backlog_issue.as_ref()
                    && !backlog_issue
                        .identifier
                        .eq_ignore_ascii_case(&issue.identifier)
                {
                    let _ = try_transition_issue_to_review_state(&service, backlog_issue).await?;
                }
                let refreshed_issue = load_worker_issue(&service, &args.issue)
                    .await
                    .unwrap_or_else(|_| {
                        transitioned_issue.clone().unwrap_or_else(|| issue.clone())
                    });
                let review_transition_applied = !listen_issue_is_active(
                    refreshed_issue
                        .state
                        .as_ref()
                        .map(|state| state.name.as_str()),
                );

                if review_transition_applied {
                    let summary = compact_completed_summary(
                        Some(&progress),
                        turns_completed,
                        &issue_state_label(&refreshed_issue),
                    );
                    write_listen_session(
                        &source_root,
                        project_selector,
                        build_worker_session(
                            &refreshed_issue,
                            SessionPhase::Completed,
                            summary,
                            &session_context,
                            turns_completed,
                            session_id.as_deref(),
                            &session_tokens,
                        ),
                    )?;
                    return Ok(());
                }

                write_listen_session(
                    &source_root,
                    project_selector,
                    build_worker_session(
                        &refreshed_issue,
                        SessionPhase::Blocked,
                        compact_blocked_summary(
                            "Blocked | backlog complete but review transition failed",
                            Some(&progress),
                            &log_path,
                        ),
                        &session_context,
                        turns_completed,
                        session_id.as_deref(),
                        &session_tokens,
                    ),
                )?;
                return Ok(());
            }

            if let Some(branch) = branch.as_deref() {
                let _ = publish_listener_pull_request(
                    &service,
                    &issue,
                    &workspace_path,
                    branch,
                    PullRequestPublishMode::Draft,
                )
                .await?;
            }

            if stalled_turns >= MAX_STALLED_TURNS {
                write_listen_session(
                    &source_root,
                    project_selector,
                    build_worker_session(
                        &issue,
                        SessionPhase::Blocked,
                        compact_blocked_summary(
                            &format!("Blocked | stalled after {stalled_turns} turn(s)"),
                            Some(&progress),
                            &log_path,
                        ),
                        &session_context,
                        turns_completed,
                        session_id.as_deref(),
                        &session_tokens,
                    ),
                )?;
                return Ok(());
            }

            write_listen_session(
                &source_root,
                project_selector,
                build_worker_session(
                    &issue,
                    SessionPhase::Running,
                    compact_running_summary(
                        Some(&progress),
                        turns_completed,
                        args.max_turns,
                        stalled_turns,
                    ),
                    &session_context,
                    turns_completed,
                    session_id.as_deref(),
                    &session_tokens,
                ),
            )?;
        } else {
            write_listen_session(
                &source_root,
                project_selector,
                build_worker_session(
                    &issue,
                    SessionPhase::Running,
                    compact_running_summary(None, turns_completed, args.max_turns, stalled_turns),
                    &session_context,
                    turns_completed,
                    session_id.as_deref(),
                    &session_tokens,
                ),
            )?;
        }
    }
}

struct ListenTurnContext<'a> {
    app_config: &'a AppConfig,
    planning_meta: &'a crate::config::PlanningMeta,
    args: &'a ListenWorkerArgs,
    source_root: &'a Path,
    project_selector: Option<&'a str>,
    workspace_path: &'a Path,
    workpad_comment_id: &'a str,
    backlog_issue: Option<&'a IssueSummary>,
    max_turns: u32,
}

struct WorkerSessionContext<'a> {
    source_root: &'a Path,
    project_selector: Option<&'a str>,
    workspace_path: &'a Path,
    branch: Option<&'a str>,
    workpad_comment_id: &'a str,
    backlog_issue: Option<&'a IssueSummary>,
    pid: Option<u32>,
    latest_resume_handle: Option<LatestResumeHandle>,
}

#[derive(Debug, Default)]
struct TurnExecutionResult {
    session_id: Option<String>,
    usage: Option<AgentTokenUsage>,
    latest_resume_handle: Option<LatestResumeHandle>,
}

async fn load_worker_issue<C>(service: &LinearService<C>, identifier: &str) -> Result<IssueSummary>
where
    C: LinearClient,
{
    let filters = IssueListFilters {
        team: issue_team_key(identifier),
        limit: 250,
        ..IssueListFilters::default()
    };

    for attempt in 0..2 {
        match service
            .find_issue_by_identifier(identifier, filters.clone())
            .await
        {
            Ok(Some(issue)) => return Ok(issue),
            Ok(None) => return Err(anyhow!("issue `{identifier}` was not found in Linear")),
            Err(error) if attempt == 0 && is_transient_linear_read_failure(&error) => {
                sleep(Duration::from_millis(100)).await;
            }
            Err(error) => return Err(error),
        }
    }

    Err(anyhow!("issue `{identifier}` was not found in Linear"))
}

fn is_transient_linear_read_failure(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .to_string()
            .contains("failed to reach the Linear GraphQL endpoint")
    })
}

fn load_worker_backlog_issue(
    workspace_path: &Path,
    identifier: &str,
    parent_issue: &IssueSummary,
) -> Result<IssueSummary> {
    let issue_dir = PlanningPaths::new(workspace_path).backlog_issue_dir(identifier);
    let metadata = load_issue_metadata(&issue_dir).ok();
    Ok(IssueSummary {
        id: metadata
            .as_ref()
            .map(|metadata| metadata.issue_id.clone())
            .unwrap_or_else(|| identifier.to_string()),
        identifier: identifier.to_string(),
        title: metadata
            .as_ref()
            .map(|metadata| metadata.title.clone())
            .unwrap_or_else(|| parent_issue.title.clone()),
        description: None,
        url: metadata
            .as_ref()
            .map(|metadata| metadata.url.clone())
            .unwrap_or_default(),
        priority: parent_issue.priority,
        estimate: None,
        updated_at: parent_issue.updated_at.clone(),
        team: parent_issue.team.clone(),
        project: parent_issue.project.clone(),
        assignee: None,
        labels: Vec::new(),
        comments: Vec::new(),
        state: Some(WorkflowState {
            id: String::new(),
            name: BACKLOG_STATE.to_string(),
            kind: Some("backlog".to_string()),
        }),
        attachments: Vec::new(),
        parent: None,
        children: Vec::new(),
    })
}

fn listener_pull_request_title(issue: &IssueSummary) -> String {
    format!("{}: {}", issue.identifier, issue.title)
}

fn listener_pull_request_body(issue: &IssueSummary) -> String {
    let mut lines = vec![
        format!("# {}", listener_pull_request_title(issue)),
        String::new(),
        "## Summary".to_string(),
        format!("- Linear issue: {}", issue.url),
        format!(
            "- Published automatically by `meta agents listen` for `{}`",
            issue.identifier
        ),
        String::new(),
        "## Lifecycle".to_string(),
        "- Initial publication uses a draft PR for unattended work in progress.".to_string(),
        "- The same PR is promoted to ready for review during the existing review handoff."
            .to_string(),
    ];

    if let Some(description) = issue.description.as_deref()
        && !description.trim().is_empty()
    {
        lines.push(String::new());
        lines.push("## Issue Context".to_string());
        lines.push(description.trim().to_string());
    }

    lines.join("\n")
}

fn write_listener_pull_request_body(
    workspace_path: &Path,
    issue: &IssueSummary,
) -> Result<std::path::PathBuf> {
    let path = PlanningPaths::new(workspace_path)
        .agent_dir
        .join(format!("{}-pull-request.md", issue.identifier));
    write_text_file(&path, &listener_pull_request_body(issue), true)?;
    Ok(path)
}

fn workspace_branch_is_published(workspace_path: &Path, branch: &str) -> Result<bool> {
    let remote_ref = format!("refs/remotes/origin/{branch}");
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_path)
        .args(["show-ref", "--verify", "--quiet", &remote_ref])
        .output()
        .with_context(|| format!("failed to run `git show-ref --verify --quiet {remote_ref}`"))?;
    if output.status.success() {
        return Ok(true);
    }
    if output.status.code() == Some(1) {
        return Ok(false);
    }
    bail!(
        "git show-ref --verify --quiet {} failed: {}",
        remote_ref,
        String::from_utf8_lossy(&output.stderr).trim()
    )
}

fn ensure_listener_pull_request_label(
    gh: &GhCli,
    workspace_path: &Path,
    pull_request: &PullRequestLifecycleResult,
) -> Result<()> {
    gh.ensure_label_exists(
        workspace_path,
        REQUIRED_LISTEN_PR_LABEL,
        REQUIRED_LISTEN_PR_LABEL_COLOR,
        REQUIRED_LISTEN_PR_LABEL_DESCRIPTION,
    )?;
    gh.add_label_to_pull_request(
        workspace_path,
        pull_request.number,
        REQUIRED_LISTEN_PR_LABEL,
    )
}

async fn ensure_listener_pull_request_attachment<C>(
    service: &LinearService<C>,
    issue: &IssueSummary,
    pull_request: &PullRequestLifecycleResult,
) -> Result<()>
where
    C: LinearClient,
{
    if issue
        .attachments
        .iter()
        .any(|attachment| attachment.url == pull_request.url)
    {
        return Ok(());
    }

    service
        .create_attachment(AttachmentCreateRequest {
            issue_id: issue.id.clone(),
            title: format!("GitHub PR #{}", pull_request.number),
            url: pull_request.url.clone(),
            metadata: json!({
                "provider": "github",
                "type": "pull_request"
            }),
        })
        .await?;
    Ok(())
}

async fn publish_listener_pull_request<C>(
    service: &LinearService<C>,
    issue: &IssueSummary,
    workspace_path: &Path,
    branch: &str,
    mode: PullRequestPublishMode,
) -> Result<Option<PullRequestLifecycleResult>>
where
    C: LinearClient,
{
    if branch.eq_ignore_ascii_case(LISTEN_PULL_REQUEST_BASE_BRANCH) {
        return Ok(None);
    }
    if !workspace_branch_is_published(workspace_path, branch)? {
        return Ok(None);
    }

    let gh = GhCli;
    let body_path = write_listener_pull_request_body(workspace_path, issue)?;
    let title = listener_pull_request_title(issue);
    let pull_request = gh.publish_branch_pull_request(
        workspace_path,
        PullRequestPublishRequest {
            head_branch: branch,
            base_branch: LISTEN_PULL_REQUEST_BASE_BRANCH,
            title: &title,
            body_path: &body_path,
            mode,
        },
    )?;
    ensure_listener_pull_request_label(&gh, workspace_path, &pull_request)?;
    ensure_listener_pull_request_attachment(service, issue, &pull_request).await?;
    Ok(Some(pull_request))
}

async fn prepare_listener_pull_request_for_review<C>(
    service: &LinearService<C>,
    issue: &IssueSummary,
    workspace_path: &Path,
    branch: &str,
) -> Result<PullRequestLifecycleResult>
where
    C: LinearClient,
{
    let pull_request = publish_listener_pull_request(
        service,
        issue,
        workspace_path,
        branch,
        PullRequestPublishMode::Draft,
    )
    .await?
    .ok_or_else(|| {
        anyhow!(
            "branch `{branch}` has not been pushed to origin, so the review handoff has no PR to promote"
        )
    })?;
    let gh = GhCli;
    let ready = gh.promote_branch_pull_request_to_ready(
        workspace_path,
        branch,
        LISTEN_PULL_REQUEST_BASE_BRANCH,
    )?;
    ensure_listener_pull_request_label(&gh, workspace_path, &ready)?;
    ensure_listener_pull_request_attachment(service, issue, &ready).await?;
    Ok(match ready.action {
        PullRequestLifecycleAction::PromotedToReady | PullRequestLifecycleAction::AlreadyReady => {
            ready
        }
        _ => pull_request,
    })
}

fn build_listen_run_args(
    issue: &IssueSummary,
    turn_number: u32,
    context: &ListenTurnContext<'_>,
) -> Result<RunAgentArgs> {
    let instructions = build_agent_instructions(issue, turn_number, context)?;
    Ok(RunAgentArgs {
        root: Some(context.source_root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_AGENTS_LISTEN.to_string()),
        agent: context.args.agent.clone(),
        prompt: render_agent_prompt(
            issue,
            context.workspace_path,
            context.workpad_comment_id,
            context.backlog_issue,
            turn_number,
            context.max_turns,
        ),
        instructions: Some(instructions),
        model: context.args.model.clone(),
        reasoning: context.args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    })
}

pub(super) fn write_preflight_failure(log_path: &Path, error: &anyhow::Error) -> Result<()> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("failed to open `{}`", log_path.display()))?;
    writeln!(
        log,
        "\n--- meta listen preflight failed @ {} ---\n{}\n",
        now_timestamp(),
        error
    )
    .with_context(|| format!("failed to write `{}`", log_path.display()))
}

fn execute_agent_turn(
    issue: &IssueSummary,
    turn_number: u32,
    context: &ListenTurnContext<'_>,
    mut on_session_started: impl FnMut(&str) -> Result<()>,
    mut on_usage: impl FnMut(&AgentTokenUsage) -> Result<()>,
) -> Result<TurnExecutionResult> {
    let run_args = build_listen_run_args(issue, turn_number, context)?;
    let invocation = resolve_agent_invocation_for_planning(
        context.app_config,
        context.planning_meta,
        &run_args,
    )?;
    let capture_output = invocation.builtin_provider;
    let command_args = if capture_output {
        command_args_for_invocation_with_options(
            &invocation,
            AgentExecutionOptions {
                working_dir: Some(context.workspace_path.to_path_buf()),
                extra_env: Vec::new(),
                capture_output: true,
                continuation: None,
            },
        )?
    } else {
        command_args_for_invocation(&invocation, Some(context.workspace_path))?
    };
    let attempted_command = validate_invocation_command_surface(&invocation, &command_args)?;
    let log_path = agent_log_path(
        context.source_root,
        context.project_selector,
        &issue.identifier,
    );
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    {
        let mut log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("failed to open `{}`", log_path.display()))?;
        writeln!(
            log,
            "\n--- meta listen turn {}/{} @ {} ---",
            turn_number,
            context.max_turns,
            now_timestamp()
        )
        .with_context(|| format!("failed to write `{}`", log_path.display()))?;
        writeln!(
            log,
            "command: {} {}",
            invocation.command,
            command_args.join(" ")
        )
        .with_context(|| format!("failed to write `{}`", log_path.display()))?;
        for line in render_invocation_diagnostics(&invocation) {
            writeln!(log, "{line}")
                .with_context(|| format!("failed to write `{}`", log_path.display()))?;
        }
    }
    let mut command = Command::new(&invocation.command);
    command.current_dir(context.workspace_path);
    command.args(&command_args);
    if capture_output {
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
    } else {
        let stdout = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("failed to open `{}`", log_path.display()))?;
        let stderr = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("failed to open `{}`", log_path.display()))?;
        command.stdout(Stdio::from(stdout));
        command.stderr(Stdio::from(stderr));
    }
    apply_noninteractive_agent_environment(&mut command);
    apply_invocation_environment(
        &mut command,
        &invocation,
        &run_args.prompt,
        run_args.instructions.as_deref(),
    );
    command.env("CI", "1");
    command.env("METASTACK_LISTEN_UNATTENDED", "1");
    command.env("METASTACK_LINEAR_ISSUE_ID", &issue.id);
    command.env("METASTACK_LINEAR_ISSUE_IDENTIFIER", &issue.identifier);
    command.env("METASTACK_LINEAR_ISSUE_URL", &issue.url);
    command.env(
        "METASTACK_LINEAR_WORKPAD_COMMENT_ID",
        context.workpad_comment_id,
    );
    command.env("METASTACK_WORKSPACE_PATH", context.workspace_path);
    command.env("METASTACK_SOURCE_ROOT", context.source_root);
    if let Some(backlog_issue) = context.backlog_issue {
        command.env("METASTACK_LINEAR_BACKLOG_ISSUE_ID", &backlog_issue.id);
        command.env(
            "METASTACK_LINEAR_BACKLOG_ISSUE_IDENTIFIER",
            &backlog_issue.identifier,
        );
        command.env("METASTACK_LINEAR_BACKLOG_ISSUE_URL", &backlog_issue.url);
        command.env(
            "METASTACK_LINEAR_BACKLOG_PATH",
            PlanningPaths::new(context.workspace_path).backlog_issue_dir(&backlog_issue.identifier),
        );
    }
    let attachment_context_path =
        PlanningPaths::new(context.workspace_path).agent_issue_context_dir(&issue.identifier);
    if attachment_context_path.is_dir() {
        command.env(
            "METASTACK_LINEAR_ATTACHMENT_CONTEXT_PATH",
            &attachment_context_path,
        );
    }
    for key in [
        "LINEAR_API_KEY",
        "LINEAR_API_URL",
        "LINEAR_TEAM",
        "METASTACK_CONFIG",
    ] {
        if let Ok(value) = std::env::var(key) {
            command.env(key, value);
        }
    }

    match invocation.transport {
        PromptTransport::Arg => {
            command.stdin(Stdio::null());
        }
        PromptTransport::Stdin => {
            command.stdin(Stdio::piped());
        }
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch agent `{}` with command `{attempted_command}`",
            invocation.agent
        )
    })?;
    let turn_started_at = now_epoch_seconds();

    if invocation.transport == PromptTransport::Stdin {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to open stdin for the listen agent turn"))?;
        use std::io::Write as _;
        stdin
            .write_all(invocation.payload.as_bytes())
            .context("failed to write prompt payload to the launched agent")?;
    }

    if capture_output {
        let provider = builtin_provider_adapter(&invocation.agent)
            .ok_or_else(|| anyhow!("builtin provider `{}` is not configured", invocation.agent))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("failed to capture stdout for listen turn {turn_number}"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("failed to capture stderr for listen turn {turn_number}"))?;
        let stderr_log_path = log_path.clone();
        let stderr_handle = thread::spawn(move || -> Result<()> {
            let mut stderr_log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&stderr_log_path)
                .with_context(|| format!("failed to open `{}`", stderr_log_path.display()))?;
            for line in BufReader::new(stderr).lines() {
                let line = line.with_context(|| {
                    format!("failed to read stderr for `{}`", stderr_log_path.display())
                })?;
                writeln!(stderr_log, "{line}")
                    .with_context(|| format!("failed to write `{}`", stderr_log_path.display()))?;
            }
            Ok(())
        });

        let mut stdout_log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("failed to open `{}`", log_path.display()))?;
        let mut raw_stdout = String::new();
        let mut continuation = None;
        let mut usage = None;
        let mut latest_resume_handle = None;
        for line in BufReader::new(stdout).lines() {
            let line = line
                .with_context(|| format!("failed to read stdout for `{}`", log_path.display()))?;
            writeln!(stdout_log, "{line}")
                .with_context(|| format!("failed to write `{}`", log_path.display()))?;
            raw_stdout.push_str(&line);
            raw_stdout.push('\n');
            if latest_resume_handle.is_none() {
                latest_resume_handle = parse_resume_handle_line(&invocation.agent, line.as_bytes());
            }
            let parsed = provider.parse_capture_output(&line)?;
            if let Some(current_session_id) = parsed.continuation
                && continuation.as_deref() != Some(current_session_id.as_str())
            {
                on_session_started(&current_session_id)?;
                continuation = Some(current_session_id);
            }
            if let Some(update) = parsed.usage
                && usage.as_ref() != Some(&update)
            {
                on_usage(&update)?;
                usage = Some(update);
            }
        }

        let status = child
            .wait()
            .with_context(|| format!("failed to wait for agent turn {turn_number}"))?;
        stderr_handle
            .join()
            .map_err(|_| anyhow!("stderr drain thread panicked for listen turn {turn_number}"))??;
        if !status.success() {
            let code = status
                .code()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "terminated by signal".to_string());
            bail!(
                "agent `{}` exited unsuccessfully during listen turn {turn_number} ({code})",
                invocation.agent
            );
        }
        let parsed = provider.parse_capture_output(&raw_stdout)?;
        let turn_finished_at = now_epoch_seconds();
        return Ok(TurnExecutionResult {
            session_id: parsed.continuation.or(continuation),
            usage: parsed.usage.or(usage),
            latest_resume_handle: latest_resume_handle.or_else(|| {
                if invocation.agent == "codex" {
                    resolve_codex_resume_handle(
                        context.workspace_path,
                        issue,
                        turn_started_at,
                        turn_finished_at,
                    )
                } else {
                    None
                }
            }),
        });
    }

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for agent turn {turn_number}"))?;
    if !status.success() {
        let code = status
            .code()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "terminated by signal".to_string());
        bail!(
            "agent `{}` exited unsuccessfully during listen turn {turn_number} ({code})",
            invocation.agent
        );
    }

    Ok(TurnExecutionResult::default())
}

fn parse_resume_handle_line(agent: &str, line: &[u8]) -> Option<LatestResumeHandle> {
    let trimmed = std::str::from_utf8(line).ok()?.trim();
    if trimmed.is_empty() {
        return None;
    }
    let value: Value = serde_json::from_str(trimmed).ok()?;
    match agent {
        "claude" => parse_claude_resume_handle(&value),
        "codex" => parse_codex_resume_handle(&value),
        _ => None,
    }
}

fn parse_claude_resume_handle(value: &Value) -> Option<LatestResumeHandle> {
    Some(LatestResumeHandle {
        provider: ResumeProvider::Claude,
        id: value.get("session_id")?.as_str()?.to_string(),
    })
}

fn parse_codex_resume_handle(value: &Value) -> Option<LatestResumeHandle> {
    (value.get("type")?.as_str()? == "thread.started").then_some(LatestResumeHandle {
        provider: ResumeProvider::Codex,
        id: value.get("thread_id")?.as_str()?.to_string(),
    })
}

fn resolve_codex_resume_handle(
    workspace_path: &Path,
    issue: &IssueSummary,
    turn_started_at: u64,
    turn_finished_at: u64,
) -> Option<LatestResumeHandle> {
    let codex_root = codex_root_dir()?;
    let index_candidates =
        read_codex_session_index(&codex_root, turn_started_at, turn_finished_at).ok()?;
    let state_db = latest_codex_state_db(&codex_root)?;
    let rows = query_codex_threads(
        &state_db,
        workspace_path,
        issue,
        turn_started_at,
        turn_finished_at,
        &index_candidates,
    )
    .ok()?;

    (rows.len() == 1).then(|| LatestResumeHandle {
        provider: ResumeProvider::Codex,
        id: rows[0].id.clone(),
    })
}

fn codex_root_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".codex"))
}

fn latest_codex_state_db(codex_root: &Path) -> Option<PathBuf> {
    let mut candidates = fs::read_dir(codex_root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.starts_with("state_") && value.ends_with(".sqlite"))
        })
        .filter_map(|path| {
            let modified = fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    candidates.into_iter().next().map(|(_, path)| path)
}

fn read_codex_session_index(
    codex_root: &Path,
    turn_started_at: u64,
    turn_finished_at: u64,
) -> Result<Vec<String>> {
    let index_path = codex_root.join("session_index.jsonl");
    let contents = fs::read_to_string(&index_path)
        .with_context(|| format!("failed to read `{}`", index_path.display()))?;
    let lower_bound = turn_started_at.saturating_sub(30);
    let upper_bound = turn_finished_at.saturating_add(30);
    let mut ids = Vec::new();

    for line in contents.lines() {
        let entry: CodexSessionIndexEntry = serde_json::from_str(line)
            .with_context(|| format!("failed to decode `{}`", index_path.display()))?;
        let updated_at = DateTime::parse_from_rfc3339(&entry.updated_at)
            .with_context(|| format!("failed to parse `{}` timestamp", entry.updated_at))?
            .with_timezone(&Utc)
            .timestamp();
        if updated_at >= lower_bound as i64 && updated_at <= upper_bound as i64 {
            ids.push(entry.id);
        }
    }

    Ok(ids)
}

fn query_codex_threads(
    state_db: &Path,
    workspace_path: &Path,
    issue: &IssueSummary,
    turn_started_at: u64,
    turn_finished_at: u64,
    recent_ids: &[String],
) -> Result<Vec<CodexThreadRow>> {
    let lower_bound = turn_started_at.saturating_sub(30);
    let upper_bound = turn_finished_at.saturating_add(30);
    let workspace_literal = sqlite_string_literal(&workspace_path.display().to_string());
    let issue_literal = sqlite_string_literal(&issue.identifier);
    let mut clauses = vec![
        "source = 'exec'".to_string(),
        format!("cwd = '{workspace_literal}'"),
        format!("title LIKE '%{issue_literal}%'"),
        format!("created_at >= {lower_bound}"),
        format!("created_at <= {upper_bound}"),
    ];
    if let Ok(branch) = current_workspace_branch(workspace_path)
        && !branch.trim().is_empty()
    {
        clauses.push(format!(
            "git_branch = '{}'",
            sqlite_string_literal(branch.trim())
        ));
    }
    if !recent_ids.is_empty() {
        let ids = recent_ids
            .iter()
            .map(|id| format!("'{}'", sqlite_string_literal(id)))
            .collect::<Vec<_>>()
            .join(", ");
        clauses.push(format!("id IN ({ids})"));
    }
    let query = format!(
        "SELECT id, created_at, updated_at FROM threads WHERE {} ORDER BY updated_at DESC;",
        clauses.join(" AND ")
    );
    let output = Command::new("sqlite3")
        .arg(state_db)
        .arg(&query)
        .output()
        .with_context(|| format!("failed to run `sqlite3 {}`", state_db.display()))?;
    if !output.status.success() {
        bail!(
            "sqlite3 query failed for `{}`: {}",
            state_db.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .filter_map(CodexThreadRow::from_sqlite_row)
        .collect())
}

fn sqlite_string_literal(value: &str) -> String {
    value.replace('\'', "''")
}

#[derive(Debug, Deserialize)]
struct CodexSessionIndexEntry {
    id: String,
    updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexThreadRow {
    id: String,
}

impl CodexThreadRow {
    fn from_sqlite_row(row: &str) -> Option<Self> {
        let mut parts = row.split('|');
        Some(Self {
            id: parts.next()?.trim().to_string(),
        })
    }
}

fn build_agent_instructions(
    issue: &IssueSummary,
    turn_number: u32,
    context: &ListenTurnContext<'_>,
) -> Result<String> {
    let repo_target = RepoTarget::with_workspace(context.source_root, context.workspace_path);
    let workflow_contract = render_workflow_contract(context.source_root, repo_target)?;
    let mut sections = vec![
        workflow_contract,
        "You are running inside `meta listen`, an unattended orchestration session.".to_string(),
        "Never ask a human to perform follow-up actions. Only stop early for a true blocker such as missing required auth, permissions, or secrets.".to_string(),
        "Work only in the provided workspace checkout and do not edit any other filesystem path.".to_string(),
        format!(
            "Use `{}` as the repository root for implementation, validation, commits, pushes, and PR creation.",
            context.workspace_path.display()
        ),
        "Keep implementation, validation, and local backlog updates anchored to the provided workspace checkout for the active repository.".to_string(),
        format!(
            "Reconcile the existing `## Codex Workpad` comment `{}` before doing new work and keep that single comment updated in place.",
            context.workpad_comment_id
        ),
        "Never overwrite the primary Linear issue description during `meta listen`. Put planning, progress, validation, and status updates in the workpad comment instead.".to_string(),
        "Reproduce the issue before changing code, refine the workpad plan and acceptance criteria, then implement and validate the fix.".to_string(),
        "Each turn must either leave meaningful non-`.metastack/` workspace updates or stop with a concrete blocker. Merely rewriting backlog files, briefs, or workpad notes is not enough.".to_string(),
        "If the Linear ticket contains `Validation`, `Test Plan`, or `Testing` sections, mirror them into the workpad and execute them as required checks.".to_string(),
        "Do not consider the task complete until the code is committed and pushed. Shared automation will create or update the branch PR as a draft, attach it to Linear, and promote it to ready during the review handoff.".to_string(),
        format!(
            "Shared automation keeps the `{}` label attached when it publishes or updates the GitHub PR for this ticket. If you touch PR metadata directly, preserve that label and do not use the legacy `{}` label.",
            REQUIRED_LISTEN_PR_LABEL, LEGACY_LISTEN_PR_LABEL
        ),
    ];

    if let Some(backlog_issue) = context.backlog_issue {
        sections.push(format!(
            "A local backlog exists for `{}` in `{}`. Use those files as the task list source of truth, keep them current as you work, and keep the original Linear issue comment updated in place.",
            backlog_issue.identifier,
            PlanningPaths::new(context.workspace_path)
                .backlog_issue_dir(&backlog_issue.identifier)
                .display()
        ));
    }

    let manifest_path = PlanningPaths::new(context.workspace_path)
        .agent_issue_context_manifest_path(&issue.identifier);
    if manifest_path.is_file() {
        sections.push(format!(
            "Additional Linear attachment context has been downloaded to `{}`. Review `{}` and use the downloaded markdown files and attachments as supporting context before implementation.",
            manifest_path.parent().unwrap_or(context.workspace_path).display(),
            manifest_path.display()
        ));
    }

    if turn_number > 1 {
        sections.push(format!(
            "This is continuation turn {turn_number} of {}. Resume from the current workspace and workpad state instead of restarting from scratch.",
            context.max_turns
        ));
        sections.push(
            "The previous turn completed normally, but the issue is still active. Do not repeat finished investigation or validation unless the new code changes require it."
                .to_string(),
        );
    }

    if issue.description.is_none() {
        sections.push(
            "Issue description is empty in Linear; rely on the current workspace and workpad state."
                .to_string(),
        );
    }

    Ok(sections.join("\n\n"))
}

fn build_worker_session(
    issue: &IssueSummary,
    phase: SessionPhase,
    summary: String,
    context: &WorkerSessionContext<'_>,
    turns: u32,
    session_id: Option<&str>,
    tokens: &TokenUsage,
) -> super::AgentSession {
    super::AgentSession {
        issue_id: Some(issue.id.clone()),
        issue_identifier: issue.identifier.clone(),
        issue_title: issue.title.clone(),
        project_name: issue.project.as_ref().map(|project| project.name.clone()),
        team_key: issue.team.key.clone(),
        issue_url: issue.url.clone(),
        phase,
        summary,
        brief_path: Some(
            PlanningPaths::new(context.workspace_path)
                .agent_briefs_dir
                .join(format!("{}.md", issue.identifier))
                .display()
                .to_string(),
        ),
        backlog_issue_identifier: context
            .backlog_issue
            .map(|backlog_issue| backlog_issue.identifier.clone()),
        backlog_issue_title: context
            .backlog_issue
            .map(|backlog_issue| backlog_issue.title.clone()),
        backlog_path: context.backlog_issue.map(|backlog_issue| {
            PlanningPaths::new(context.workspace_path)
                .backlog_issue_dir(&backlog_issue.identifier)
                .display()
                .to_string()
        }),
        workspace_path: Some(context.workspace_path.display().to_string()),
        branch: context.branch.map(str::to_string),
        workpad_comment_id: Some(context.workpad_comment_id.to_string()),
        updated_at_epoch_seconds: now_epoch_seconds(),
        pid: context.pid.filter(|value| *value > 0),
        session_id: session_id
            .map(str::to_string)
            .or_else(|| Some(issue.id.clone())),
        latest_resume_handle: context.latest_resume_handle.clone(),
        turns: Some(turns),
        tokens: tokens.clone(),
        log_path: Some(
            agent_log_path(
                context.source_root,
                context.project_selector,
                &issue.identifier,
            )
            .display()
            .to_string(),
        ),
    }
}

fn load_existing_session_tokens(
    root: &Path,
    project_selector: Option<&str>,
    issue_identifier: &str,
) -> Result<TokenUsage> {
    let store = super::store::ListenProjectStore::resolve(root, project_selector)?;
    let state = store.load_state()?;
    Ok(state
        .sessions
        .into_iter()
        .find(|session| session.issue_matches(issue_identifier))
        .map(|session| session.tokens)
        .unwrap_or_default())
}

fn load_existing_session_id(
    root: &Path,
    project_selector: Option<&str>,
    issue_identifier: &str,
) -> Result<Option<String>> {
    let store = super::store::ListenProjectStore::resolve(root, project_selector)?;
    let state = store.load_state()?;
    Ok(state
        .sessions
        .into_iter()
        .find(|session| session.issue_matches(issue_identifier))
        .and_then(|session| session.session_id))
}

#[cfg(test)]
mod tests {
    use super::{
        LatestResumeHandle, Path, ResumeProvider, Value, WorkerSessionContext,
        build_worker_session, parse_claude_resume_handle, parse_codex_resume_handle,
        query_codex_threads, read_codex_session_index,
    };
    use crate::linear::{IssueSummary, TeamRef};
    use crate::listen::{SessionPhase, TokenUsage};
    use std::fs;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;

    fn issue() -> IssueSummary {
        IssueSummary {
            id: "issue-1".to_string(),
            identifier: "ENG-10181".to_string(),
            title: "Track listen tokens".to_string(),
            description: None,
            url: "https://linear.app/issues/ENG-10181".to_string(),
            priority: None,
            estimate: None,
            updated_at: "2026-03-20T00:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "ENG".to_string(),
                name: "Engineering".to_string(),
            },
            project: None,
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: None,
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        }
    }

    fn test_issue(identifier: &str) -> IssueSummary {
        IssueSummary {
            id: format!("{identifier}-id"),
            identifier: identifier.to_string(),
            title: format!("{identifier} title"),
            description: None,
            url: format!("https://linear.app/issues/{identifier}"),
            priority: None,
            estimate: None,
            updated_at: "2026-03-19T00:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "ENG".to_string(),
                name: "Engineering".to_string(),
            },
            project: None,
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: None,
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn set_env_var(key: &str, value: &str) {
        unsafe {
            std::env::set_var(key, value);
        }
    }

    fn restore_env_var(key: &str, value: Option<String>) {
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn worker_session_updates_keep_cumulative_tokens() {
        let issue = issue();
        let context = WorkerSessionContext {
            source_root: Path::new("/tmp/source"),
            project_selector: None,
            workspace_path: Path::new("/tmp/workspace"),
            branch: Some("eng-10181"),
            workpad_comment_id: "comment-1",
            backlog_issue: None,
            pid: Some(1234),
            latest_resume_handle: None,
        };
        let mut tokens = TokenUsage::default();

        let first = build_worker_session(
            &issue,
            SessionPhase::Running,
            "turn 1".to_string(),
            &context,
            0,
            Some("thread-1"),
            &tokens,
        );
        assert_eq!(first.tokens.input, None);
        assert_eq!(first.tokens.output, None);

        tokens.accumulate(&TokenUsage {
            input: Some(120),
            output: None,
        });
        let second = build_worker_session(
            &issue,
            SessionPhase::Running,
            "turn 2".to_string(),
            &context,
            1,
            Some("thread-1"),
            &tokens,
        );
        assert_eq!(second.tokens.input, Some(120));
        assert_eq!(second.tokens.output, None);

        tokens.accumulate(&TokenUsage {
            input: None,
            output: Some(45),
        });
        let third = build_worker_session(
            &issue,
            SessionPhase::Completed,
            "done".to_string(),
            &context,
            2,
            Some("thread-1"),
            &tokens,
        );
        assert_eq!(third.tokens.input, Some(120));
        assert_eq!(third.tokens.output, Some(45));
        assert_eq!(third.tokens.total(), Some(165));
    }

    #[test]
    fn parses_claude_resume_handle_from_stream_json() {
        let value: Value = serde_json::from_str(
            r#"{"type":"system","subtype":"init","session_id":"513d2595-0968-4357-9339-489f1d21c1cf"}"#,
        )
        .expect("valid json");

        assert_eq!(
            parse_claude_resume_handle(&value),
            Some(LatestResumeHandle {
                provider: ResumeProvider::Claude,
                id: "513d2595-0968-4357-9339-489f1d21c1cf".to_string(),
            })
        );
    }

    #[test]
    fn parses_codex_resume_handle_from_thread_started_event() {
        let value: Value = serde_json::from_str(
            r#"{"type":"thread.started","thread_id":"019d0766-1ca5-70c3-ae80-afafe1fb7bff"}"#,
        )
        .expect("valid json");

        assert_eq!(
            parse_codex_resume_handle(&value),
            Some(LatestResumeHandle {
                provider: ResumeProvider::Codex,
                id: "019d0766-1ca5-70c3-ae80-afafe1fb7bff".to_string(),
            })
        );
    }

    #[test]
    fn read_codex_session_index_filters_recent_entries() {
        let temp = tempdir().expect("tempdir should build");
        let codex_root = temp.path().join(".codex");
        fs::create_dir_all(&codex_root).expect("codex dir should exist");
        fs::write(
            codex_root.join("session_index.jsonl"),
            concat!(
                "{\"id\":\"recent\",\"updated_at\":\"2026-03-19T15:00:05Z\"}\n",
                "{\"id\":\"old\",\"updated_at\":\"2026-03-19T14:58:00Z\"}\n"
            ),
        )
        .expect("session index should write");

        let ids =
            read_codex_session_index(&codex_root, 1_773_932_400, 1_773_932_420).expect("index");

        assert_eq!(ids, vec!["recent".to_string()]);
    }

    #[test]
    fn query_codex_threads_returns_only_matching_rows() {
        let _guard = env_lock().lock().expect("env mutex should lock");
        let temp = tempdir().expect("tempdir should build");
        let workspace = temp.path().join("workspace");
        let bin_dir = temp.path().join("bin");
        fs::create_dir_all(&workspace).expect("workspace dir should exist");
        fs::create_dir_all(&bin_dir).expect("bin dir should exist");
        let sqlite_path = bin_dir.join("sqlite3");
        fs::write(&sqlite_path, "#!/bin/sh\nprintf '%s' \"$SQLITE3_ROWS\"\n")
            .expect("sqlite stub should write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&sqlite_path)
                .expect("sqlite stub metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&sqlite_path, permissions).expect("sqlite stub permissions");
        }

        let original_path = std::env::var("PATH").ok();
        set_env_var(
            "PATH",
            &format!(
                "{}:{}",
                bin_dir.display(),
                original_path.clone().unwrap_or_default()
            ),
        );
        set_env_var("SQLITE3_ROWS", "thread-1|1773945466|1773945607\n");

        let init = std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&workspace)
            .status()
            .expect("git init should run");
        assert!(init.success());
        let checkout = std::process::Command::new("git")
            .arg("-C")
            .arg(&workspace)
            .args(["checkout", "-b", "eng-10194"])
            .status()
            .expect("git checkout should run");
        assert!(checkout.success());

        let rows = query_codex_threads(
            Path::new("/tmp/fake-state.sqlite"),
            &workspace,
            &test_issue("ENG-10194"),
            1_773_945_460,
            1_773_945_610,
            &["thread-1".to_string()],
        )
        .expect("sqlite query should succeed");

        restore_env_var("PATH", original_path);
        restore_env_var("SQLITE3_ROWS", None);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "thread-1");
    }

    #[test]
    fn query_codex_threads_rejects_ambiguous_rows() {
        let _guard = env_lock().lock().expect("env mutex should lock");
        let temp = tempdir().expect("tempdir should build");
        let workspace = temp.path().join("workspace");
        let bin_dir = temp.path().join("bin");
        fs::create_dir_all(&workspace).expect("workspace dir should exist");
        fs::create_dir_all(&bin_dir).expect("bin dir should exist");
        let sqlite_path = bin_dir.join("sqlite3");
        fs::write(&sqlite_path, "#!/bin/sh\nprintf '%s' \"$SQLITE3_ROWS\"\n")
            .expect("sqlite stub should write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&sqlite_path)
                .expect("sqlite stub metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&sqlite_path, permissions).expect("sqlite stub permissions");
        }

        let original_path = std::env::var("PATH").ok();
        set_env_var(
            "PATH",
            &format!(
                "{}:{}",
                bin_dir.display(),
                original_path.clone().unwrap_or_default()
            ),
        );
        set_env_var(
            "SQLITE3_ROWS",
            "thread-1|1773945466|1773945607\nthread-2|1773945468|1773945608\n",
        );

        let init = std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(&workspace)
            .status()
            .expect("git init should run");
        assert!(init.success());

        let rows = query_codex_threads(
            Path::new("/tmp/fake-state.sqlite"),
            &workspace,
            &test_issue("ENG-10194"),
            1_773_945_460,
            1_773_945_610,
            &[],
        )
        .expect("sqlite query should succeed");

        restore_env_var("PATH", original_path);
        restore_env_var("SQLITE3_ROWS", None);

        assert_eq!(rows.len(), 2);
    }
}

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
    AGENT_ROUTE_AGENTS_LISTEN, AppConfig, LinearConfig, LinearConfigOverrides, PlanningMeta,
    PromptTransport,
};
use crate::config_resolution::{AgentConfigOverrides, normalize_agent_name, resolve_agent_config};
use crate::fs::sibling_workspace_root;
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
use crate::workflow_contract::render_workflow_contract_for_listen;
use crate::workspace::{AutoCleanOutcome, try_auto_clean_workspace};

use super::{
    BACKLOG_STATE, CanonicalSessionData, LatestResumeHandle, MAX_STALLED_TURNS, PullRequestStatus,
    PullRequestSummary, ResumeProvider, SessionPhase, TokenUsage, TurnPromptMode,
    TurnTokenSnapshot, agent_log_path, backlog_progress_for_issue_dir, capture_workspace_snapshot,
    compact_blocked_summary, compact_completed_summary, compact_running_summary,
    compact_session_summary, compare_workspace_snapshots, current_workspace_branch,
    issue_state_label, issue_team_key, listen_issue_is_active, now_epoch_seconds, now_timestamp,
    preflight, render_agent_prompt, render_continuation_prompt,
    try_transition_issue_to_review_state, workspace_has_meaningful_progress, write_listen_session,
};

const REQUIRED_LISTEN_PR_LABEL: &str = "metastack";
const LEGACY_LISTEN_PR_LABEL: &str = "symphony";
const REQUIRED_LISTEN_PR_LABEL_COLOR: &str = "0e8a16";
const REQUIRED_LISTEN_PR_LABEL_DESCRIPTION: &str = "MetaStack automation";
const LINEAR_IDENTIFIER_PR_LABEL_COLOR: &str = "1d76db";
const LISTEN_PULL_REQUEST_BASE_BRANCH: &str = "main";

fn listen_preflight_failure_header(timestamp: &str) -> String {
    format!(
        "\n--- {} listen preflight failed @ {} ---\n",
        crate::branding::COMMAND_NAME,
        timestamp
    )
}

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
    let log_path = agent_log_path(&source_root, args.project.as_deref(), &args.issue);
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open `{}`", log_path.display()))?;
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
    let session_origin = load_existing_session_origin(&source_root, project_selector, &args.issue)?;
    let mut session_context = WorkerSessionContext {
        source_root: &source_root,
        project_selector,
        workspace_path: &workspace_path,
        branch: branch.as_deref(),
        workpad_comment_id: &args.workpad_comment_id,
        backlog_issue: backlog_issue.as_ref(),
        pid: Some(worker_pid),
        latest_resume_handle: load_existing_latest_resume_handle(
            &source_root,
            project_selector,
            &args.issue,
        )?,
        turn_history: load_existing_turn_history(&source_root, project_selector, &args.issue)?,
        canonical: load_existing_session_canonical(&source_root, project_selector, &args.issue)?,
        pull_request: load_existing_pull_request(&source_root, project_selector, &args.issue)?,
        origin: session_origin,
    };
    let mut session_tokens =
        load_existing_session_tokens(&source_root, project_selector, &args.issue)?;
    let mut provider_session_id =
        load_existing_provider_session_id(&source_root, project_selector, &args.issue)?;
    let _initial_meaningful_progress = workspace_has_meaningful_progress(&workspace_path, true)?;
    let mut stalled_turns = 0u32;
    let mut last_review: Option<ReviewReport> = None;
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
                provider_session_id.as_deref(),
                &session_context.canonical,
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
                    provider_session_id.as_deref(),
                    &session_context.canonical,
                ),
            )?;
            try_listener_auto_clean(&source_root, project_selector, &workspace_path, &args.issue);
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
                    provider_session_id.as_deref(),
                    &session_context.canonical,
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
                provider_session_id.as_deref(),
                &session_context.canonical,
            ),
        )?;

        // Determine whether this turn will actually attempt a resumed invocation.
        // Only retry on failure when resume was genuinely attempted (not just "handle exists").
        let attempted_resume = turn_number > 1
            && session_context
                .latest_resume_handle
                .as_ref()
                .is_some_and(|h| {
                    resolve_effective_listen_agent(
                        &app_config,
                        &planning_meta,
                        args.agent.as_deref(),
                    )
                    .as_deref()
                    .is_some_and(|a| h.matches_agent(a))
                });

        // Keep provider-native manual resume handles separate from provider session bookkeeping.
        let provider_session_id_state = RefCell::new(provider_session_id.clone());
        let turn_result = match execute_agent_turn(
            &issue,
            turn_number,
            &turn_context,
            last_review.as_ref(),
            session_context.latest_resume_handle.as_ref(),
            |current_session_id| {
                if provider_session_id_state.borrow().as_deref() == Some(current_session_id) {
                    return Ok(());
                }
                *provider_session_id_state.borrow_mut() = Some(current_session_id.to_string());
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
                        provider_session_id_state.borrow().as_deref(),
                        &session_context.canonical,
                    ),
                )
            },
            |usage| {
                let mut displayed_tokens = session_tokens.clone();
                let mut displayed_canonical = session_context.canonical.clone();
                displayed_tokens.accumulate(&TokenUsage {
                    input: usage.input,
                    output: usage.output,
                });
                displayed_canonical.tokens = displayed_tokens.clone();
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
                        provider_session_id_state.borrow().as_deref(),
                        &displayed_canonical,
                    ),
                )
            },
        ) {
            Ok(result) => result,
            Err(error)
                if attempted_resume
                    && resolve_effective_listen_agent(
                        &app_config,
                        &planning_meta,
                        args.agent.as_deref(),
                    )
                    .and_then(|agent| crate::agent_provider::builtin_provider_adapter(&agent))
                    .is_some_and(|provider| {
                        provider.is_invalid_resume_error(&error.to_string())
                    }) =>
            {
                eprintln!(
                    "listen: invalid resume for {} turn {turn_number}, retrying as cold start: {error}",
                    issue.identifier,
                );
                session_context.latest_resume_handle = None;
                let provider_session_id_retry = RefCell::new(provider_session_id.clone());
                match execute_agent_turn(
                    &issue,
                    turn_number,
                    &turn_context,
                    last_review.as_ref(),
                    None,
                    |current_session_id| {
                        if provider_session_id_retry.borrow().as_deref() == Some(current_session_id)
                        {
                            return Ok(());
                        }
                        *provider_session_id_retry.borrow_mut() =
                            Some(current_session_id.to_string());
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
                                provider_session_id_retry.borrow().as_deref(),
                                &session_context.canonical,
                            ),
                        )
                    },
                    |usage| {
                        let mut displayed_tokens = session_tokens.clone();
                        let mut displayed_canonical = session_context.canonical.clone();
                        displayed_tokens.accumulate(&TokenUsage {
                            input: usage.input,
                            output: usage.output,
                        });
                        displayed_canonical.tokens = displayed_tokens.clone();
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
                                provider_session_id_retry.borrow().as_deref(),
                                &displayed_canonical,
                            ),
                        )
                    },
                ) {
                    Ok(result) => {
                        // Sync the retry provider session ID back so the outer into_inner picks it up.
                        *provider_session_id_state.borrow_mut() =
                            provider_session_id_retry.into_inner();
                        result
                    }
                    Err(retry_error) => {
                        write_listen_session(
                            &source_root,
                            project_selector,
                            build_worker_session(
                                &issue,
                                SessionPhase::Blocked,
                                compact_blocked_summary(
                                    &format!(
                                        "Blocked | turn {turn_number}/{} failed (resume retry)",
                                        args.max_turns
                                    ),
                                    backlog_progress_before.as_ref(),
                                    &log_path,
                                ),
                                &session_context,
                                turns_completed,
                                provider_session_id.as_deref(),
                                &session_context.canonical,
                            ),
                        )?;
                        return Err(retry_error);
                    }
                }
            }
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
                        provider_session_id.as_deref(),
                        &session_context.canonical,
                    ),
                )?;
                return Err(error);
            }
        };
        session_context.latest_resume_handle = turn_result
            .latest_resume_handle
            .or(session_context.latest_resume_handle);
        if session_context.latest_resume_handle.is_some() {
            eprintln!(
                "listen: captured resume handle for {} on turn {turn_number}",
                issue.identifier,
            );
        } else {
            eprintln!(
                "listen: no resume handle captured for {} on turn {turn_number}",
                issue.identifier,
            );
        }
        provider_session_id = turn_result
            .session_id
            .or_else(|| provider_session_id_state.into_inner());
        if let Some(provider) = turn_result.provider {
            session_context.canonical.provider = Some(provider);
            session_context.canonical.model = turn_result.model;
            session_context.canonical.reasoning = turn_result.reasoning;
        }
        let turn_snapshot = TurnTokenSnapshot {
            turn: turn_number,
            prompt_mode: turn_result.prompt_mode,
            tokens: turn_result
                .usage
                .as_ref()
                .map(|usage| TokenUsage {
                    input: usage.input,
                    output: usage.output,
                })
                .unwrap_or_default(),
            captured_at_epoch_seconds: now_epoch_seconds(),
        };
        append_turn_token_summary(&log_path, &turn_snapshot)?;
        if let Some(existing) = session_context
            .turn_history
            .iter_mut()
            .find(|snapshot| snapshot.turn == turn_snapshot.turn)
        {
            *existing = turn_snapshot.clone();
        } else {
            session_context.turn_history.push(turn_snapshot);
        }
        if let Some(usage) = turn_result.usage {
            session_tokens.accumulate(&TokenUsage {
                input: usage.input,
                output: usage.output,
            });
        }
        session_context.canonical.tokens = session_tokens.clone();

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
        let meaningful_turn_progress =
            turn_progress.implementation_changed() || turn_progress.planning_changed();
        let review = run_review_phase(
            &service,
            &issue,
            turn_number,
            meaningful_turn_progress,
            &turn_context,
            WorkerPhaseContext {
                source_root: &source_root,
                project_selector,
                session_context: &session_context,
                provider_session_id: provider_session_id.as_deref(),
                log_path: &log_path,
            },
        )
        .await?;
        last_review = Some(review.clone());
        if meaningful_turn_progress {
            stalled_turns = 0;
        } else {
            stalled_turns += 1;
        }

        if review.complete {
            let final_review = run_final_review_phase(
                &issue,
                turn_number,
                &turn_context,
                &review,
                WorkerPhaseContext {
                    source_root: &source_root,
                    project_selector,
                    session_context: &session_context,
                    provider_session_id: provider_session_id.as_deref(),
                    log_path: &log_path,
                },
            )
            .await?;
            if final_review.approved {
                write_listen_session(
                    &source_root,
                    project_selector,
                    build_worker_session(
                        &issue,
                        SessionPhase::Publishing,
                        compact_session_summary([
                            Some("Publishing review-ready handoff".to_string()),
                            Some(format!("see {}", log_path.display())),
                        ]),
                        &session_context,
                        turns_completed,
                        provider_session_id.as_deref(),
                        &session_context.canonical,
                    ),
                )?;
                let branch = branch.as_deref().ok_or_else(|| {
                    anyhow!("failed to inspect the workspace branch before promoting the review PR")
                })?;
                let pull_request = prepare_listener_pull_request_for_review(
                    &service,
                    &issue,
                    &workspace_path,
                    branch,
                )
                .await?;
                session_context.pull_request = pull_request
                    .map(PullRequestSummary::from)
                    .unwrap_or_default();
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
                        backlog_progress.as_ref(),
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
                            provider_session_id.as_deref(),
                            &session_context.canonical,
                        ),
                    )?;
                    try_listener_auto_clean(
                        &source_root,
                        project_selector,
                        &workspace_path,
                        &args.issue,
                    );
                    return Ok(());
                }

                write_listen_session(
                    &source_root,
                    project_selector,
                    build_worker_session(
                        &refreshed_issue,
                        SessionPhase::Blocked,
                        compact_blocked_summary(
                            "Blocked | final review passed but review transition failed",
                            backlog_progress.as_ref(),
                            &log_path,
                        ),
                        &session_context,
                        turns_completed,
                        provider_session_id.as_deref(),
                        &session_context.canonical,
                    ),
                )?;
                return Ok(());
            }
            let follow_up_review = ReviewReport {
                summary: final_review.summary.clone(),
                complete: false,
                completed_items: review.completed_items.clone(),
                remaining_items: final_review.missing_items.clone(),
                validation_completed: review.validation_completed.clone(),
                validation_remaining: final_review.validation_gaps.clone(),
                risks: final_review.risks.clone(),
                notes: final_review.notes.clone(),
            };
            sync_review_tracking(&service, &issue, &turn_context, &follow_up_review).await?;
            last_review = Some(follow_up_review);
        }

        if let Some(progress) = backlog_progress {
            if let Some(branch) = branch.as_deref() {
                if let Some(pull_request) = publish_listener_pull_request(
                    &service,
                    &issue,
                    &workspace_path,
                    branch,
                    PullRequestPublishMode::Draft,
                )
                .await?
                .map(PullRequestSummary::from)
                {
                    session_context.pull_request = pull_request;
                }
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
                        provider_session_id.as_deref(),
                        &session_context.canonical,
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
                    provider_session_id.as_deref(),
                    &session_context.canonical,
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
                    provider_session_id.as_deref(),
                    &session_context.canonical,
                ),
            )?;
        }
    }
}

/// Best-effort auto-clean for a listener workspace after the session completes.
///
/// When the workspace is safe (clean git state, within expected sibling root), removes the
/// workspace clone and its ticket-scoped listen artifacts (session entry, detail, log). When the
/// workspace has uncommitted changes, unpushed commits, or other safety risks, logs the skip
/// reason and leaves the workspace in place for manual cleanup via `meta workspace prune`.
fn try_listener_auto_clean(
    source_root: &Path,
    project_selector: Option<&str>,
    workspace_path: &Path,
    issue_identifier: &str,
) {
    let workspace_root = match sibling_workspace_root(source_root) {
        Ok(root) => root,
        Err(error) => {
            eprintln!(
                "listen: auto-clean skipped for {issue_identifier}: \
                 failed to resolve workspace root: {error}"
            );
            return;
        }
    };

    match try_auto_clean_workspace(
        source_root,
        project_selector,
        &workspace_root,
        workspace_path,
        issue_identifier,
    ) {
        Ok(AutoCleanOutcome::Removed { bytes_reclaimed }) => {
            eprintln!(
                "listen: auto-cleaned workspace for {issue_identifier} \
                 (freed {} bytes)",
                bytes_reclaimed
            );
        }
        Ok(AutoCleanOutcome::Skipped { reason }) => {
            eprintln!(
                "listen: auto-clean skipped for {issue_identifier}: \
                 {reason} (manual review needed)"
            );
        }
        Err(error) => {
            eprintln!("listen: auto-clean failed for {issue_identifier}: {error:#}");
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
    turn_history: Vec<TurnTokenSnapshot>,
    canonical: CanonicalSessionData,
    pull_request: PullRequestSummary,
    origin: super::state::SessionOrigin,
}

struct AgentPhaseInvocation<'a> {
    issue: &'a IssueSummary,
    context: &'a ListenTurnContext<'a>,
    turn_number: u32,
    phase_label: &'a str,
    prompt_mode: TurnPromptMode,
    continuation_handle: Option<&'a LatestResumeHandle>,
}

struct WorkerPhaseContext<'a> {
    source_root: &'a Path,
    project_selector: Option<&'a str>,
    session_context: &'a WorkerSessionContext<'a>,
    provider_session_id: Option<&'a str>,
    log_path: &'a Path,
}

#[derive(Debug, Default)]
struct TurnExecutionResult {
    session_id: Option<String>,
    usage: Option<AgentTokenUsage>,
    latest_resume_handle: Option<LatestResumeHandle>,
    prompt_mode: TurnPromptMode,
    provider: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
    response_text: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ReviewReport {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    complete: bool,
    #[serde(default)]
    completed_items: Vec<String>,
    #[serde(default)]
    remaining_items: Vec<String>,
    #[serde(default)]
    validation_completed: Vec<String>,
    #[serde(default)]
    validation_remaining: Vec<String>,
    #[serde(default)]
    risks: Vec<String>,
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FinalReviewReport {
    #[serde(default)]
    approved: bool,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    missing_items: Vec<String>,
    #[serde(default)]
    validation_gaps: Vec<String>,
    #[serde(default)]
    risks: Vec<String>,
    #[serde(default)]
    notes: Vec<String>,
}

impl From<PullRequestLifecycleResult> for PullRequestSummary {
    fn from(value: PullRequestLifecycleResult) -> Self {
        Self {
            number: Some(value.number),
            url: Some(value.url),
            status: if value.is_draft {
                PullRequestStatus::Draft
            } else {
                PullRequestStatus::Ready
            },
        }
    }
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

    for attempt in 0..3 {
        match service
            .find_issue_by_identifier(identifier, filters.clone())
            .await
        {
            Ok(Some(issue)) => return Ok(issue),
            Ok(None) => return Err(anyhow!("issue `{identifier}` was not found in Linear")),
            Err(error) if attempt < 2 && is_transient_linear_read_failure(&error) => {
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

fn listener_linear_identifier_pr_label(issue: &IssueSummary) -> String {
    format!("id-{}", issue.identifier)
}

fn listener_pull_request_body(issue: &IssueSummary) -> String {
    let mut lines = vec![
        format!("# {}", listener_pull_request_title(issue)),
        String::new(),
        "## Summary".to_string(),
        format!("- Linear issue: {}", issue.url),
        format!(
            "- Published automatically by `{} agents listen` for `{}`",
            crate::branding::COMMAND_NAME,
            issue.identifier,
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
    issue: &IssueSummary,
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
    )?;

    let linear_identifier_label = listener_linear_identifier_pr_label(issue);
    gh.ensure_label_exists(
        workspace_path,
        &linear_identifier_label,
        LINEAR_IDENTIFIER_PR_LABEL_COLOR,
        &format!("Linear issue {}", issue.identifier),
    )?;
    gh.add_label_to_pull_request(
        workspace_path,
        pull_request.number,
        &linear_identifier_label,
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
    ensure_listener_pull_request_label(&gh, workspace_path, issue, &pull_request)?;
    ensure_listener_pull_request_attachment(service, issue, &pull_request).await?;
    Ok(Some(pull_request))
}

async fn prepare_listener_pull_request_for_review<C>(
    service: &LinearService<C>,
    issue: &IssueSummary,
    workspace_path: &Path,
    branch: &str,
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
    let Some(_existing) = gh.refresh_existing_branch_pull_request(
        workspace_path,
        PullRequestPublishRequest {
            head_branch: branch,
            base_branch: LISTEN_PULL_REQUEST_BASE_BRANCH,
            title: &title,
            body_path: &body_path,
            mode: PullRequestPublishMode::Draft,
        },
    )?
    else {
        return Ok(None);
    };

    let ready = gh.promote_branch_pull_request_to_ready(
        workspace_path,
        branch,
        LISTEN_PULL_REQUEST_BASE_BRANCH,
    )?;
    ensure_listener_pull_request_label(&gh, workspace_path, issue, &ready)?;
    ensure_listener_pull_request_attachment(service, issue, &ready).await?;
    Ok(Some(match ready.action {
        PullRequestLifecycleAction::PromotedToReady | PullRequestLifecycleAction::AlreadyReady => {
            ready
        }
        _ => unreachable!("review handoff promotion should only return ready states"),
    }))
}

fn build_listen_run_args(
    issue: &IssueSummary,
    turn_number: u32,
    context: &ListenTurnContext<'_>,
    previous_review: Option<&ReviewReport>,
    has_resume_handle: bool,
) -> Result<RunAgentArgs> {
    let use_continuation = has_resume_handle && turn_number > 1;

    let prompt = if turn_number > 1 {
        render_execution_delta_prompt(
            issue,
            turn_number,
            context.max_turns,
            previous_review,
            use_continuation,
        )
    } else {
        render_agent_prompt(
            issue,
            context.workspace_path,
            context.workpad_comment_id,
            context.backlog_issue,
            turn_number,
            context.max_turns,
        )
    };

    let instructions = if use_continuation {
        None
    } else {
        Some(build_agent_instructions(issue, turn_number, context)?)
    };

    Ok(RunAgentArgs {
        root: Some(context.source_root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_AGENTS_LISTEN.to_string()),
        agent: context.args.agent.clone(),
        prompt,
        instructions,
        model: context.args.model.clone(),
        reasoning: context.args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    })
}

fn build_review_run_args(
    issue: &IssueSummary,
    turn_number: u32,
    context: &ListenTurnContext<'_>,
) -> RunAgentArgs {
    RunAgentArgs {
        root: Some(context.source_root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_AGENTS_LISTEN.to_string()),
        agent: context.args.agent.clone(),
        prompt: render_review_prompt(issue, turn_number, context),
        instructions: Some(build_review_instructions(context)),
        model: context.args.model.clone(),
        reasoning: context.args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    }
}

fn build_final_review_run_args(
    issue: &IssueSummary,
    turn_number: u32,
    context: &ListenTurnContext<'_>,
    review: &ReviewReport,
) -> RunAgentArgs {
    RunAgentArgs {
        root: Some(context.source_root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_AGENTS_LISTEN.to_string()),
        agent: context.args.agent.clone(),
        prompt: render_final_review_prompt(issue, turn_number, context, review),
        instructions: Some(build_final_review_instructions(context)),
        model: context.args.model.clone(),
        reasoning: context.args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    }
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
        "{}{}\n",
        listen_preflight_failure_header(&now_timestamp()),
        error,
    )
    .with_context(|| format!("failed to write `{}`", log_path.display()))
}

fn execute_agent_turn(
    issue: &IssueSummary,
    turn_number: u32,
    context: &ListenTurnContext<'_>,
    previous_review: Option<&ReviewReport>,
    continuation_handle: Option<&LatestResumeHandle>,
    mut on_session_started: impl FnMut(&str) -> Result<()>,
    mut on_usage: impl FnMut(&AgentTokenUsage) -> Result<()>,
) -> Result<TurnExecutionResult> {
    let effective_agent = resolve_effective_listen_agent(
        context.app_config,
        context.planning_meta,
        context.args.agent.as_deref(),
    );
    let has_resume_handle = continuation_handle
        .filter(|h| {
            effective_agent
                .as_deref()
                .is_some_and(|a| h.matches_agent(a))
        })
        .is_some();
    let use_continuation = has_resume_handle && turn_number > 1;
    let prompt_mode = if use_continuation {
        TurnPromptMode::Continuation
    } else {
        TurnPromptMode::FullPrompt
    };
    eprintln!(
        "listen: turn {turn_number}/{} for {} | resume={has_resume_handle} | prompt_mode={}",
        context.max_turns,
        issue.identifier,
        prompt_mode.label(),
    );
    let run_args = build_listen_run_args(
        issue,
        turn_number,
        context,
        previous_review,
        has_resume_handle,
    )?;
    execute_agent_run(
        AgentPhaseInvocation {
            issue,
            context,
            turn_number,
            phase_label: "execute",
            prompt_mode,
            continuation_handle: if use_continuation {
                continuation_handle
            } else {
                None
            },
        },
        run_args,
        &mut on_session_started,
        &mut on_usage,
    )
}

fn execute_agent_run(
    phase: AgentPhaseInvocation<'_>,
    run_args: RunAgentArgs,
    mut on_session_started: impl FnMut(&str) -> Result<()>,
    mut on_usage: impl FnMut(&AgentTokenUsage) -> Result<()>,
) -> Result<TurnExecutionResult> {
    let issue = phase.issue;
    let context = phase.context;
    let turn_number = phase.turn_number;
    let phase_label = phase.phase_label;
    let prompt_mode = phase.prompt_mode;
    let invocation = resolve_agent_invocation_for_planning(
        context.app_config,
        context.planning_meta,
        &run_args,
    )?;
    let capture_output = invocation.builtin_provider;
    let command_args = if capture_output {
        let continuation =
            continuation_id_for_invocation(&invocation.agent, phase.continuation_handle);
        command_args_for_invocation_with_options(
            &invocation,
            AgentExecutionOptions {
                working_dir: Some(context.workspace_path.to_path_buf()),
                extra_env: Vec::new(),
                capture_output: true,
                continuation,
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
            "\n--- {} listen {} turn {}/{} @ {} ---",
            crate::branding::COMMAND_NAME,
            phase_label,
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
        let stderr_handle = thread::spawn(move || -> Result<String> {
            let mut stderr_log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&stderr_log_path)
                .with_context(|| format!("failed to open `{}`", stderr_log_path.display()))?;
            let mut collected = String::new();
            for line in BufReader::new(stderr).lines() {
                let line = line.with_context(|| {
                    format!("failed to read stderr for `{}`", stderr_log_path.display())
                })?;
                writeln!(stderr_log, "{line}")
                    .with_context(|| format!("failed to write `{}`", stderr_log_path.display()))?;
                collected.push_str(&line);
                collected.push('\n');
            }
            Ok(collected)
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
        let stderr_output = stderr_handle
            .join()
            .map_err(|_| anyhow!("stderr drain thread panicked for listen turn {turn_number}"))??;
        if !status.success() {
            let code = status
                .code()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "terminated by signal".to_string());
            bail!(
                "agent `{}` exited unsuccessfully during listen turn {turn_number} ({code}): {}",
                invocation.agent,
                stderr_output.trim()
            );
        }
        let parsed = provider.parse_capture_output(&raw_stdout)?;
        let turn_finished_at = now_epoch_seconds();
        return Ok(TurnExecutionResult {
            response_text: parsed.response_text.clone(),
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
            prompt_mode,
            provider: Some(invocation.agent.clone()),
            model: invocation.model.clone(),
            reasoning: invocation.reasoning.clone(),
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

    Ok(TurnExecutionResult {
        prompt_mode,
        ..TurnExecutionResult::default()
    })
}

fn append_turn_token_summary(log_path: &Path, snapshot: &TurnTokenSnapshot) -> Result<()> {
    let mut log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("failed to open `{}`", log_path.display()))?;
    writeln!(log, "{}", snapshot.display_compact())
        .with_context(|| format!("failed to write `{}`", log_path.display()))
}

fn resolve_effective_listen_agent(
    app_config: &AppConfig,
    planning_meta: &PlanningMeta,
    agent_override: Option<&str>,
) -> Option<String> {
    resolve_agent_config(
        app_config,
        planning_meta,
        Some(AGENT_ROUTE_AGENTS_LISTEN),
        AgentConfigOverrides {
            provider: agent_override.map(String::from),
            ..Default::default()
        },
    )
    .ok()
    .map(|resolved| normalize_agent_name(&resolved.provider))
}

fn continuation_id_for_invocation(
    agent: &str,
    continuation_handle: Option<&LatestResumeHandle>,
) -> Option<String> {
    continuation_handle
        .filter(|handle| handle.matches_agent(agent))
        .map(|handle| handle.id.clone())
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
    // Claude stream-json wraps each event in an array: [{"type":"system","session_id":"..."}]
    let obj = value
        .as_array()
        .and_then(|arr| arr.first())
        .unwrap_or(value);
    Some(LatestResumeHandle {
        provider: ResumeProvider::Claude,
        id: obj.get("session_id")?.as_str()?.to_string(),
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
    let workflow_contract = render_workflow_contract_for_listen(context.source_root, repo_target)?;
    let brief_path = PlanningPaths::new(context.workspace_path)
        .agent_briefs_dir
        .join(format!("{}.md", issue.identifier));
    let mut sections = vec![
        workflow_contract,
        format!(
            "You are running inside `{}` listen, an unattended orchestration session.",
            crate::branding::COMMAND_NAME
        ),
        "Never ask a human to perform follow-up actions. Only stop early for a true blocker such as missing required auth, permissions, or secrets.".to_string(),
        "Work only in the provided workspace checkout and do not edit any other filesystem path.".to_string(),
        format!(
            "Use `{}` as the repository root for implementation, validation, commits, pushes, and PR creation.",
            context.workspace_path.display()
        ),
        "Treat the Linear ticket title, description, labels, and attached instructions as the primary work contract. Execute that work directly instead of expanding it into extra planning unless the ticket explicitly asks for that.".to_string(),
        format!(
            "A generated brief is available at `{}` if you need repo context, but do not spend time restating or expanding it unless the ticket requires that depth.",
            brief_path.display()
        ),
        "Keep implementation, validation, and local backlog updates anchored to the provided workspace checkout for the active repository.".to_string(),
        format!(
            "Reconcile the existing `## Codex Workpad` comment `{}` before doing new work and keep that single comment updated in place.",
            context.workpad_comment_id
        ),
        format!(
            "Never overwrite the primary Linear issue description during `{}` listen. Put planning, progress, validation, and status updates in the workpad comment instead.",
            crate::branding::COMMAND_NAME
        ),
        "Execute the requested work directly, validate what you changed, and avoid adding extra planning, analysis, or decomposition unless the ticket explicitly asks for them.".to_string(),
        format!(
            "Each turn must either leave meaningful workspace updates or stop with a concrete blocker. Do not burn turns rewriting `{}/` files, briefs, or workpad notes unless that is part of the ticket's requested deliverable.",
            crate::branding::PROJECT_DIR
        ),
        "If the Linear ticket contains `Validation`, `Test Plan`, or `Testing` sections, mirror them into the workpad and execute them as required checks.".to_string(),
        "Do not consider the task complete until the requested ticket deliverables are committed and pushed. Shared automation will create or update the branch PR as a draft, attach it to Linear, and promote it to ready during the review handoff.".to_string(),
        format!(
            "Shared automation keeps the `{}` label attached when it publishes or updates the GitHub PR for this ticket. If you touch PR metadata directly, preserve that label and do not use the legacy `{}` label.",
            REQUIRED_LISTEN_PR_LABEL, LEGACY_LISTEN_PR_LABEL
        ),
    ];

    if let Some(backlog_issue) = context.backlog_issue {
        sections.push(format!(
            "A local backlog exists for `{}` in `{}`. Use it only as lightweight tracking. Do not expand, rewrite, or improve backlog files unless the ticket explicitly asks for that. If checklist items already exist, mark off only the work you actually completed.",
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

fn build_review_instructions(_: &ListenTurnContext<'_>) -> String {
    format!(
        "You are the review phase for `{}` listen. Review the current workspace against the Linear ticket and return JSON only.\n\nReturn an object with this exact shape:\n{{\n  \"summary\": \"short review summary\",\n  \"complete\": true,\n  \"completed_items\": [\"ticket requirement or deliverable completed\"],\n  \"remaining_items\": [\"specific remaining work item\"],\n  \"validation_completed\": [\"validation step completed\"],\n  \"validation_remaining\": [\"validation still required\"],\n  \"risks\": [\"risk or likely mistake\"],\n  \"notes\": [\"short operator note\"]\n}}\n\nUse the Linear ticket acceptance criteria and validation sections as the source of truth. Mark `complete` true only when the requested deliverables are done, validation is sufficient, and the branch is ready for final review.",
        crate::branding::COMMAND_NAME
    )
}

fn build_final_review_instructions(_: &ListenTurnContext<'_>) -> String {
    format!(
        "You are the final review phase for `{}` listen. Perform a fast safety review of the current workspace and return JSON only.\n\nReturn an object with this exact shape:\n{{\n  \"approved\": true,\n  \"summary\": \"short final review summary\",\n  \"missing_items\": [\"anything still missing from the ticket\"],\n  \"validation_gaps\": [\"validation still missing\"],\n  \"risks\": [\"residual risk or likely mistake\"],\n  \"notes\": [\"short operator note\"]\n}}\n\nSet `approved` true only if the work matches the Linear ticket, acceptance criteria are satisfied, and no material validation gaps remain.",
        crate::branding::COMMAND_NAME
    )
}

fn render_execution_delta_prompt(
    issue: &IssueSummary,
    turn_number: u32,
    max_turns: u32,
    previous_review: Option<&ReviewReport>,
    use_continuation: bool,
) -> String {
    let header = if use_continuation {
        render_continuation_prompt(issue, turn_number, max_turns)
    } else {
        format!(
            "Execution continuation for `{}` turn #{}/{}.\n\nThe previous execution/review cycle did not fully complete the ticket. Resume from the current workspace state using the remaining work below.\n",
            issue.identifier, turn_number, max_turns
        )
    };
    let review_block = previous_review.map(render_review_delta_block).unwrap_or_else(|| {
        "- No prior structured review is available. Resume from the current workspace and workpad state.\n".to_string()
    });
    format!(
        "{header}\nRemaining work for `{identifier}`:\n{review_block}\n\nIssue title: {title}\nURL: {url}",
        header = header,
        identifier = issue.identifier,
        review_block = review_block,
        title = issue.title,
        url = issue.url
    )
}

fn render_review_delta_block(review: &ReviewReport) -> String {
    let mut lines = Vec::new();
    if !review.completed_items.is_empty() {
        lines.push("Completed:".to_string());
        for item in &review.completed_items {
            lines.push(format!("- {item}"));
        }
    }
    if !review.remaining_items.is_empty() {
        lines.push("Remaining:".to_string());
        for item in &review.remaining_items {
            lines.push(format!("- {item}"));
        }
    }
    if !review.validation_remaining.is_empty() {
        lines.push("Validation still required:".to_string());
        for item in &review.validation_remaining {
            lines.push(format!("- {item}"));
        }
    }
    if !review.risks.is_empty() {
        lines.push("Risks to address:".to_string());
        for item in &review.risks {
            lines.push(format!("- {item}"));
        }
    }
    if lines.is_empty() {
        "- No explicit remaining work captured.".to_string()
    } else {
        lines.join("\n")
    }
}

fn render_review_prompt(
    issue: &IssueSummary,
    turn_number: u32,
    context: &ListenTurnContext<'_>,
) -> String {
    let acceptance = extract_acceptance_criteria(issue.description.as_deref());
    let validation = extract_validation_requirements(issue.description.as_deref());
    let backlog_path = context.backlog_issue.map(|backlog_issue| {
        PlanningPaths::new(context.workspace_path)
            .backlog_issue_dir(&backlog_issue.identifier)
            .join("index.md")
    });
    format!(
        "Review the current workspace for Linear ticket `{identifier}` after execution turn #{turn_number}.\n\nTicket title: {title}\nTicket URL: {url}\nWorkspace: {workspace}\nWorkpad comment ID: {workpad}\n{backlog}\n\nAcceptance criteria:\n{acceptance}\n\nValidation requirements:\n{validation}\n\nReview the current branch/workspace state against the ticket. Return JSON only.",
        identifier = issue.identifier,
        turn_number = turn_number,
        title = issue.title,
        url = issue.url,
        workspace = context.workspace_path.display(),
        workpad = context.workpad_comment_id,
        backlog = backlog_path
            .map(|path| format!("Backlog index: {}", path.display()))
            .unwrap_or_default(),
        acceptance = render_string_list(
            &acceptance,
            "_No explicit acceptance criteria found in the ticket description._"
        ),
        validation = render_string_list(
            &validation,
            "_No explicit validation section found in the ticket description._"
        ),
    )
}

fn render_final_review_prompt(
    issue: &IssueSummary,
    turn_number: u32,
    context: &ListenTurnContext<'_>,
    review: &ReviewReport,
) -> String {
    format!(
        "Perform a final safety review for Linear ticket `{identifier}` after execution turn #{turn_number}.\n\nTicket title: {title}\nTicket URL: {url}\nWorkspace: {workspace}\n\nLatest review summary: {summary}\n\nCompleted items:\n{completed}\n\nRemaining items from latest review:\n{remaining}\n\nValidation completed:\n{validation_completed}\n\nValidation remaining:\n{validation_remaining}\n\nReturn JSON only.",
        identifier = issue.identifier,
        turn_number = turn_number,
        title = issue.title,
        url = issue.url,
        workspace = context.workspace_path.display(),
        summary = review.summary,
        completed = render_string_list(&review.completed_items, "_None recorded._"),
        remaining = render_string_list(&review.remaining_items, "_None recorded._"),
        validation_completed = render_string_list(&review.validation_completed, "_None recorded._"),
        validation_remaining = render_string_list(&review.validation_remaining, "_None recorded._"),
    )
}

fn render_string_list(values: &[String], empty: &str) -> String {
    if values.is_empty() {
        empty.to_string()
    } else {
        values
            .iter()
            .map(|value| format!("- {value}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn extract_acceptance_criteria(description: Option<&str>) -> Vec<String> {
    extract_markdown_checklist_items(
        description.unwrap_or_default(),
        &["Acceptance Criteria", "Acceptance", "Requirements"],
    )
}

fn extract_validation_requirements(description: Option<&str>) -> Vec<String> {
    extract_markdown_checklist_items(
        description.unwrap_or_default(),
        &["Validation", "Test Plan", "Testing"],
    )
}

fn extract_markdown_checklist_items(body: &str, headings: &[&str]) -> Vec<String> {
    let section = extract_markdown_section(body, headings).unwrap_or_default();
    section
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .collect()
}

fn extract_markdown_section(body: &str, headings: &[&str]) -> Option<String> {
    let normalized_headings = headings
        .iter()
        .map(|heading| heading.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut in_section = false;
    let mut collected = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(heading) = trimmed.strip_prefix("## ") {
            let normalized = heading.trim().to_ascii_lowercase();
            if normalized_headings
                .iter()
                .any(|candidate| candidate == &normalized)
            {
                in_section = true;
                continue;
            }
            if in_section {
                break;
            }
        }
        if in_section {
            collected.push(line.to_string());
        }
    }

    let section = collected.join("\n").trim().to_string();
    (!section.is_empty()).then_some(section)
}

fn parse_agent_json<T>(raw: &str, phase: &str) -> Result<T>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let trimmed = raw.trim();
    for candidate in parse_json_candidates(trimmed) {
        if let Ok(parsed) = serde_json::from_str::<T>(&candidate) {
            return Ok(parsed);
        }
    }

    bail!(
        "listen {phase} returned invalid JSON: {}",
        preview_text(trimmed)
    )
}

fn parse_json_candidates(raw: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    push_json_candidate(&mut candidates, raw);
    if let Some(stripped) = strip_code_fence(raw) {
        push_json_candidate(&mut candidates, &stripped);
        append_progressive_json_candidates(&mut candidates, &stripped);
    }
    append_progressive_json_candidates(&mut candidates, raw);
    candidates
}

fn append_progressive_json_candidates(candidates: &mut Vec<String>, raw: &str) {
    let Some(end) = raw.rfind('}') else {
        return;
    };
    for (start, character) in raw.char_indices() {
        if character == '{' && start <= end {
            push_json_candidate(candidates, &raw[start..=end]);
        }
    }
}

fn push_json_candidate(candidates: &mut Vec<String>, candidate: &str) {
    if !candidate.is_empty() && !candidates.iter().any(|existing| existing == candidate) {
        candidates.push(candidate.to_string());
    }
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
    const MAX_PREVIEW_LEN: usize = 240;
    if value.len() <= MAX_PREVIEW_LEN {
        value.to_string()
    } else {
        format!("{}...", &value[..MAX_PREVIEW_LEN])
    }
}

async fn run_review_phase(
    service: &LinearService<ReqwestLinearClient>,
    issue: &IssueSummary,
    turn_number: u32,
    meaningful_turn_progress: bool,
    context: &ListenTurnContext<'_>,
    phase_context: WorkerPhaseContext<'_>,
) -> Result<ReviewReport> {
    write_listen_session(
        phase_context.source_root,
        phase_context.project_selector,
        build_worker_session(
            issue,
            SessionPhase::Reviewing,
            compact_session_summary([
                Some(format!("Reviewing execution turn {turn_number}")),
                Some(format!("see {}", phase_context.log_path.display())),
            ]),
            phase_context.session_context,
            turn_number,
            phase_context.provider_session_id,
            &phase_context.session_context.canonical,
        ),
    )?;
    let report = if agent_backed_review_enabled() {
        let run_args = build_review_run_args(issue, turn_number, context);
        let result = execute_agent_run(
            AgentPhaseInvocation {
                issue,
                context,
                turn_number,
                phase_label: "review",
                prompt_mode: TurnPromptMode::Continuation,
                continuation_handle: None,
            },
            run_args,
            |_| Ok(()),
            |_| Ok(()),
        )?;
        let raw = result
            .response_text
            .ok_or_else(|| anyhow!("listen review did not return any structured output"))?;
        parse_agent_json(&raw, "review")?
    } else {
        heuristic_review_report(issue, context, meaningful_turn_progress)?
    };
    sync_review_tracking(service, issue, context, &report).await?;
    Ok(report)
}

async fn run_final_review_phase(
    issue: &IssueSummary,
    turn_number: u32,
    context: &ListenTurnContext<'_>,
    review: &ReviewReport,
    phase_context: WorkerPhaseContext<'_>,
) -> Result<FinalReviewReport> {
    write_listen_session(
        phase_context.source_root,
        phase_context.project_selector,
        build_worker_session(
            issue,
            SessionPhase::FinalReview,
            compact_session_summary([
                Some(format!("Final review for execution turn {turn_number}")),
                Some(format!("see {}", phase_context.log_path.display())),
            ]),
            phase_context.session_context,
            turn_number,
            phase_context.provider_session_id,
            &phase_context.session_context.canonical,
        ),
    )?;
    if agent_backed_review_enabled() {
        let run_args = build_final_review_run_args(issue, turn_number, context, review);
        let result = execute_agent_run(
            AgentPhaseInvocation {
                issue,
                context,
                turn_number,
                phase_label: "final-review",
                prompt_mode: TurnPromptMode::Continuation,
                continuation_handle: None,
            },
            run_args,
            |_| Ok(()),
            |_| Ok(()),
        )?;
        let raw = result
            .response_text
            .ok_or_else(|| anyhow!("listen final review did not return any structured output"))?;
        parse_agent_json(&raw, "final review")
    } else {
        heuristic_final_review_report(review)
    }
}

async fn sync_review_tracking(
    service: &LinearService<ReqwestLinearClient>,
    issue: &IssueSummary,
    context: &ListenTurnContext<'_>,
    review: &ReviewReport,
) -> Result<()> {
    let body = render_review_workpad(issue, context, review);
    let _ = service.upsert_workpad_comment(issue, body).await?;
    if let Some(backlog_issue) = context.backlog_issue {
        sync_backlog_progress_section(context.workspace_path, &backlog_issue.identifier, review)?;
    }
    Ok(())
}

fn render_review_workpad(
    issue: &IssueSummary,
    context: &ListenTurnContext<'_>,
    review: &ReviewReport,
) -> String {
    let mut lines = vec![
        "## Codex Workpad".to_string(),
        String::new(),
        format!("- Ticket: `{}`", issue.identifier),
        format!("- Workspace: `{}`", context.workspace_path.display()),
        format!("- Summary: {}", review.summary),
        format!(
            "- Completion status: {}",
            if review.complete {
                "complete"
            } else {
                "incomplete"
            }
        ),
        String::new(),
        "### Completed".to_string(),
        String::new(),
    ];
    if review.completed_items.is_empty() {
        lines.push("- [ ] No completed items recorded yet.".to_string());
    } else {
        for item in &review.completed_items {
            lines.push(format!("- [x] {item}"));
        }
    }
    lines.extend([String::new(), "### Remaining".to_string(), String::new()]);
    if review.remaining_items.is_empty() {
        lines.push("- [x] No remaining items identified.".to_string());
    } else {
        for item in &review.remaining_items {
            lines.push(format!("- [ ] {item}"));
        }
    }
    lines.extend([String::new(), "### Validation".to_string(), String::new()]);
    for item in &review.validation_completed {
        lines.push(format!("- [x] {item}"));
    }
    for item in &review.validation_remaining {
        lines.push(format!("- [ ] {item}"));
    }
    if review.validation_completed.is_empty() && review.validation_remaining.is_empty() {
        lines.push("- [ ] No explicit validation status recorded.".to_string());
    }
    if !review.risks.is_empty() || !review.notes.is_empty() {
        lines.extend([String::new(), "### Review Notes".to_string(), String::new()]);
        for item in &review.risks {
            lines.push(format!("- Risk: {item}"));
        }
        for item in &review.notes {
            lines.push(format!("- Note: {item}"));
        }
    }
    lines.join("\n")
}

fn agent_backed_review_enabled() -> bool {
    std::env::var("METASTACK_LISTEN_AGENT_REVIEW")
        .ok()
        .as_deref()
        == Some("1")
}

fn heuristic_review_report(
    issue: &IssueSummary,
    context: &ListenTurnContext<'_>,
    meaningful_turn_progress: bool,
) -> Result<ReviewReport> {
    let acceptance = extract_acceptance_criteria(issue.description.as_deref());
    let validation = extract_validation_requirements(issue.description.as_deref());
    let backlog_progress = context
        .backlog_issue
        .as_ref()
        .map(|backlog_issue| {
            backlog_progress_for_issue_dir(context.workspace_path, &backlog_issue.identifier)
        })
        .transpose()?;
    let backlog_complete = backlog_progress
        .as_ref()
        .is_some_and(|progress| progress.total > 0 && progress.completed == progress.total);
    let complete = backlog_complete || (meaningful_turn_progress && acceptance.is_empty());
    let completed_items = if complete {
        if acceptance.is_empty() {
            vec![
                "Workspace changes are present and no explicit acceptance criteria remain."
                    .to_string(),
            ]
        } else {
            acceptance.clone()
        }
    } else {
        Vec::new()
    };
    let mut remaining_items = if complete {
        Vec::new()
    } else if let Some(progress) = backlog_progress.as_ref() {
        progress.next_step.clone().into_iter().collect::<Vec<_>>()
    } else {
        acceptance.clone()
    };
    if remaining_items.is_empty() && !complete && !acceptance.is_empty() {
        remaining_items = acceptance.clone();
    }

    Ok(ReviewReport {
        summary: if complete {
            "Heuristic review believes the ticket work is complete.".to_string()
        } else {
            "Heuristic review found remaining work.".to_string()
        },
        complete,
        completed_items,
        remaining_items,
        validation_completed: if complete && !validation.is_empty() {
            validation.clone()
        } else {
            Vec::new()
        },
        validation_remaining: if complete { Vec::new() } else { validation },
        risks: Vec::new(),
        notes: vec!["Using heuristic review; set `METASTACK_LISTEN_AGENT_REVIEW=1` to enable agent-backed review.".to_string()],
    })
}

fn heuristic_final_review_report(review: &ReviewReport) -> Result<FinalReviewReport> {
    Ok(FinalReviewReport {
        approved: review.complete && review.validation_remaining.is_empty(),
        summary: if review.complete && review.validation_remaining.is_empty() {
            "Heuristic final review approved the ticket.".to_string()
        } else {
            "Heuristic final review found missing work or validation gaps.".to_string()
        },
        missing_items: review.remaining_items.clone(),
        validation_gaps: review.validation_remaining.clone(),
        risks: review.risks.clone(),
        notes: vec!["Using heuristic final review; set `METASTACK_LISTEN_AGENT_REVIEW=1` to enable agent-backed review.".to_string()],
    })
}

fn sync_backlog_progress_section(
    workspace_path: &Path,
    identifier: &str,
    review: &ReviewReport,
) -> Result<()> {
    let path = PlanningPaths::new(workspace_path)
        .backlog_issue_dir(identifier)
        .join("index.md");
    if !path.is_file() {
        return Ok(());
    }
    let existing = fs::read_to_string(&path)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    let rendered = render_backlog_progress_section(review);
    let updated = upsert_marked_section(&existing, "metastack-listen-progress", &rendered);
    write_text_file(&path, &updated, true)?;
    Ok(())
}

fn render_backlog_progress_section(review: &ReviewReport) -> String {
    let mut lines = vec![
        "## Listener Progress Checklist".to_string(),
        String::new(),
        "### Completed".to_string(),
        String::new(),
    ];
    if review.completed_items.is_empty() {
        lines.push("- [ ] No completed items recorded yet.".to_string());
    } else {
        for item in &review.completed_items {
            lines.push(format!("- [x] {item}"));
        }
    }
    lines.extend([String::new(), "### Remaining".to_string(), String::new()]);
    if review.remaining_items.is_empty() {
        lines.push("- [x] No remaining items identified.".to_string());
    } else {
        for item in &review.remaining_items {
            lines.push(format!("- [ ] {item}"));
        }
    }
    lines.extend([String::new(), "### Validation".to_string(), String::new()]);
    for item in &review.validation_completed {
        lines.push(format!("- [x] {item}"));
    }
    for item in &review.validation_remaining {
        lines.push(format!("- [ ] {item}"));
    }
    if review.validation_completed.is_empty() && review.validation_remaining.is_empty() {
        lines.push("- [ ] No explicit validation status recorded.".to_string());
    }
    lines.join("\n")
}

fn upsert_marked_section(contents: &str, marker: &str, body: &str) -> String {
    let start = format!("<!-- {marker}:start -->");
    let end = format!("<!-- {marker}:end -->");
    let replacement = format!("{start}\n{body}\n{end}");
    if let Some(start_index) = contents.find(&start)
        && let Some(end_index) = contents.find(&end)
    {
        let suffix_start = end_index + end.len();
        return format!(
            "{}{}{}",
            &contents[..start_index],
            replacement,
            &contents[suffix_start..]
        );
    }
    let mut updated = contents.trim_end().to_string();
    if !updated.is_empty() {
        updated.push_str("\n\n");
    }
    updated.push_str(&replacement);
    updated.push('\n');
    updated
}

fn build_worker_session(
    issue: &IssueSummary,
    phase: SessionPhase,
    summary: String,
    context: &WorkerSessionContext<'_>,
    turns: u32,
    session_id: Option<&str>,
    canonical: &CanonicalSessionData,
) -> super::AgentSession {
    let pid = match phase {
        SessionPhase::Completed | SessionPhase::Blocked => None,
        _ => context.pid.filter(|value| *value > 0),
    };
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
        pull_request: context.pull_request.clone(),
        workpad_comment_id: Some(context.workpad_comment_id.to_string()),
        updated_at_epoch_seconds: now_epoch_seconds(),
        pid,
        session_id: session_id.map(str::to_string),
        latest_resume_handle: context.latest_resume_handle.clone(),
        turns: Some(turns),
        tokens: canonical.tokens.clone(),
        turn_history: context.turn_history.clone(),
        canonical: canonical.clone(),
        log_path: Some(
            agent_log_path(
                context.source_root,
                context.project_selector,
                &issue.identifier,
            )
            .display()
            .to_string(),
        ),
        origin: context.origin,
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

fn load_existing_turn_history(
    root: &Path,
    project_selector: Option<&str>,
    issue_identifier: &str,
) -> Result<Vec<TurnTokenSnapshot>> {
    let store = super::store::ListenProjectStore::resolve(root, project_selector)?;
    let state = store.load_state()?;
    Ok(state
        .sessions
        .into_iter()
        .find(|session| session.issue_matches(issue_identifier))
        .map(|session| session.turn_history)
        .unwrap_or_default())
}

fn load_existing_session_canonical(
    root: &Path,
    project_selector: Option<&str>,
    issue_identifier: &str,
) -> Result<CanonicalSessionData> {
    let store = super::store::ListenProjectStore::resolve(root, project_selector)?;
    let state = store.load_state()?;
    Ok(state
        .sessions
        .into_iter()
        .find(|session| session.issue_matches(issue_identifier))
        .map(|session| session.canonical)
        .unwrap_or_default())
}

fn load_existing_provider_session_id(
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

fn load_existing_latest_resume_handle(
    root: &Path,
    project_selector: Option<&str>,
    issue_identifier: &str,
) -> Result<Option<LatestResumeHandle>> {
    let store = super::store::ListenProjectStore::resolve(root, project_selector)?;
    let state = store.load_state()?;
    Ok(state
        .sessions
        .into_iter()
        .find(|session| session.issue_matches(issue_identifier))
        .and_then(|session| session.latest_resume_handle))
}

fn load_existing_session_origin(
    root: &Path,
    project_selector: Option<&str>,
    issue_identifier: &str,
) -> Result<super::state::SessionOrigin> {
    let store = super::store::ListenProjectStore::resolve(root, project_selector)?;
    let state = store.load_state()?;
    Ok(state
        .sessions
        .into_iter()
        .find(|session| session.issue_matches(issue_identifier))
        .map(|session| session.origin)
        .unwrap_or_default())
}

fn load_existing_pull_request(
    root: &Path,
    project_selector: Option<&str>,
    issue_identifier: &str,
) -> Result<PullRequestSummary> {
    let store = super::store::ListenProjectStore::resolve(root, project_selector)?;
    let state = store.load_state()?;
    Ok(state
        .sessions
        .into_iter()
        .find(|session| session.issue_matches(issue_identifier))
        .map(|session| session.pull_request)
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::{
        LatestResumeHandle, Path, ResumeProvider, Value, WorkerSessionContext,
        build_worker_session, continuation_id_for_invocation, parse_claude_resume_handle,
        parse_codex_resume_handle, query_codex_threads, read_codex_session_index,
    };
    use crate::linear::{IssueSummary, TeamRef};
    use crate::listen::{
        CanonicalSessionData, PullRequestSummary, SessionOrigin, SessionPhase, TokenUsage,
    };
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
            turn_history: Vec::new(),
            canonical: CanonicalSessionData::default(),
            pull_request: PullRequestSummary::default(),
            origin: SessionOrigin::Listen,
        };
        let mut tokens = TokenUsage::default();

        let first = build_worker_session(
            &issue,
            SessionPhase::Running,
            "turn 1".to_string(),
            &context,
            0,
            Some("thread-1"),
            &CanonicalSessionData::default(),
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
            &CanonicalSessionData {
                tokens: tokens.clone(),
                ..CanonicalSessionData::default()
            },
        );
        assert_eq!(second.tokens.input, Some(120));
        assert_eq!(second.tokens.output, None);
        assert_eq!(second.canonical.tokens.input, Some(120));
        assert_eq!(second.canonical.tokens.output, None);

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
            &CanonicalSessionData {
                tokens: tokens.clone(),
                ..CanonicalSessionData::default()
            },
        );
        assert_eq!(third.tokens.input, Some(120));
        assert_eq!(third.tokens.output, Some(45));
        assert_eq!(third.tokens.total(), Some(165));
        assert_eq!(third.canonical.tokens.input, Some(120));
        assert_eq!(third.canonical.tokens.output, Some(45));
        assert_eq!(third.canonical.tokens.total(), Some(165));
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
    fn continuation_id_for_invocation_reuses_matching_resume_handle() {
        let handle = LatestResumeHandle {
            provider: ResumeProvider::Codex,
            id: "thread-123".to_string(),
        };

        assert_eq!(
            continuation_id_for_invocation("codex", Some(&handle)),
            Some("thread-123".to_string())
        );
    }

    #[test]
    fn continuation_id_for_invocation_rejects_mismatched_provider() {
        let handle = LatestResumeHandle {
            provider: ResumeProvider::Claude,
            id: "session-123".to_string(),
        };

        assert_eq!(continuation_id_for_invocation("codex", Some(&handle)), None);
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

    #[test]
    fn build_listen_run_args_uses_continuation_prompt_on_resume() {
        let temp = tempdir().expect("tempdir should build");
        let workspace = temp.path();
        fs::create_dir_all(workspace.join(".metastack")).expect("metastack dir should build");
        let source_root = temp.path();

        let issue = test_issue("MET-57");
        let app_config = crate::config::AppConfig::default();
        let planning_meta = crate::config::PlanningMeta::default();
        let args = crate::cli::ListenWorkerArgs {
            source_root: source_root.to_path_buf(),
            project: None,
            workspace: workspace.to_path_buf(),
            issue: "MET-57".to_string(),
            workpad_comment_id: "comment-1".to_string(),
            backlog_issue: None,
            max_turns: 20,
            api_key: None,
            api_url: None,
            profile: None,
            team: None,
            agent: None,
            model: None,
            reasoning: None,
        };
        let context = super::ListenTurnContext {
            app_config: &app_config,
            planning_meta: &planning_meta,
            args: &args,
            source_root,
            project_selector: None,
            workspace_path: workspace,
            workpad_comment_id: "comment-1",
            backlog_issue: None,
            max_turns: 20,
        };

        // Turn 2 with resume handle → continuation prompt, no instructions.
        let resumed = super::build_listen_run_args(&issue, 2, &context, None, true)
            .expect("build_listen_run_args should succeed");
        assert!(
            resumed.prompt.contains("Continuation guidance"),
            "resume turn 2 should use continuation prompt"
        );
        assert!(
            resumed.instructions.is_none(),
            "resume turn 2 should omit instructions"
        );

        // Turn 2 without resume handle → full prompt with instructions.
        let cold = super::build_listen_run_args(&issue, 2, &context, None, false)
            .expect("build_listen_run_args should succeed");
        assert!(
            cold.prompt.contains("Execution continuation"),
            "cold turn 2 should use compact continuation prompt"
        );
        assert!(
            cold.instructions.is_some(),
            "cold turn 2 should include instructions"
        );
    }

    #[test]
    fn build_listen_run_args_uses_full_prompt_on_turn_one_even_with_resume() {
        let temp = tempdir().expect("tempdir should build");
        let workspace = temp.path();
        fs::create_dir_all(workspace.join(".metastack")).expect("metastack dir should build");
        let source_root = temp.path();

        let issue = test_issue("MET-57");
        let app_config = crate::config::AppConfig::default();
        let planning_meta = crate::config::PlanningMeta::default();
        let args = crate::cli::ListenWorkerArgs {
            source_root: source_root.to_path_buf(),
            project: None,
            workspace: workspace.to_path_buf(),
            issue: "MET-57".to_string(),
            workpad_comment_id: "comment-1".to_string(),
            backlog_issue: None,
            max_turns: 20,
            api_key: None,
            api_url: None,
            profile: None,
            team: None,
            agent: None,
            model: None,
            reasoning: None,
        };
        let context = super::ListenTurnContext {
            app_config: &app_config,
            planning_meta: &planning_meta,
            args: &args,
            source_root,
            project_selector: None,
            workspace_path: workspace,
            workpad_comment_id: "comment-1",
            backlog_issue: None,
            max_turns: 20,
        };

        // Turn 1 with resume handle should still use full prompt (initial context load).
        let result = super::build_listen_run_args(&issue, 1, &context, None, true)
            .expect("build_listen_run_args should succeed");
        assert!(
            result.prompt.contains("You are working on Linear ticket"),
            "turn 1 should always use full prompt"
        );
        assert!(
            result.instructions.is_some(),
            "turn 1 should always include instructions"
        );
    }

    #[test]
    fn build_agent_instructions_executes_ticket_without_label_based_modes() {
        let temp = tempdir().expect("tempdir should build");
        let workspace = temp.path();
        let source_root = temp.path();
        fs::create_dir_all(workspace.join(".metastack/agents/briefs"))
            .expect("brief dir should build");
        fs::create_dir_all(workspace.join(".metastack")).expect("metastack dir should build");
        fs::write(workspace.join("AGENTS.md"), "legacy").expect("agents should write");

        let app_config = crate::config::AppConfig::default();
        let planning_meta = crate::config::PlanningMeta::default();
        let args = crate::cli::ListenWorkerArgs {
            source_root: source_root.to_path_buf(),
            project: None,
            workspace: workspace.to_path_buf(),
            issue: "MET-57".to_string(),
            workpad_comment_id: "comment-1".to_string(),
            backlog_issue: None,
            max_turns: 20,
            api_key: None,
            api_url: None,
            profile: None,
            team: None,
            agent: None,
            model: None,
            reasoning: None,
        };
        let issue = test_issue("MET-57");
        let context = super::ListenTurnContext {
            app_config: &app_config,
            planning_meta: &planning_meta,
            args: &args,
            source_root,
            project_selector: None,
            workspace_path: workspace,
            workpad_comment_id: "comment-1",
            backlog_issue: None,
            max_turns: 20,
        };

        let instructions = super::build_agent_instructions(&issue, 1, &context)
            .expect("instructions should build");

        assert!(instructions.contains("Treat the Linear ticket title, description, labels, and attached instructions as the primary work contract."));
        assert!(instructions.contains("Execute the requested work directly"));
        assert!(instructions.contains("meaningful workspace updates"));
        assert!(instructions.contains("WORKFLOW.md"));
        assert!(!instructions.contains("refine the workpad plan and acceptance criteria"));
        assert!(!instructions.contains("plan/spec oriented"));
    }

    #[test]
    fn parse_agent_json_accepts_fenced_payloads() {
        let parsed: super::ReviewReport = super::parse_agent_json(
            "```json\n{\"summary\":\"ok\",\"complete\":false,\"remaining_items\":[\"finish docs\"]}\n```",
            "review",
        )
        .expect("review json should parse");

        assert_eq!(parsed.summary, "ok");
        assert_eq!(parsed.remaining_items, vec!["finish docs"]);
    }

    #[test]
    fn upsert_marked_section_replaces_existing_managed_block() {
        let original = "\
# Title

<!-- metastack-listen-progress:start -->
old
<!-- metastack-listen-progress:end -->
";
        let updated =
            super::upsert_marked_section(original, "metastack-listen-progress", "new body");

        assert!(updated.contains("new body"));
        assert!(!updated.contains("\nold\n"));
    }

    #[test]
    fn parse_claude_resume_handle_from_array_wrapped_stream_json() {
        // Claude --output-format=stream-json wraps each event in an array
        let line = r#"[{"type":"system","subtype":"init","session_id":"22ca497e-d7da-4118-9433-1902769c6737","tools":["Bash"]}]"#;
        let handle = super::parse_resume_handle_line("claude", line.as_bytes());
        assert!(
            handle.is_some(),
            "should parse session_id from array-wrapped JSON"
        );
        let handle = handle.unwrap();
        assert_eq!(handle.id, "22ca497e-d7da-4118-9433-1902769c6737");
        assert_eq!(handle.provider, super::super::state::ResumeProvider::Claude);
    }

    #[test]
    fn parse_claude_resume_handle_from_plain_object() {
        // Also works with unwrapped objects (e.g. --output-format=json)
        let line = r#"{"type":"result","session_id":"abc-123"}"#;
        let handle = super::parse_resume_handle_line("claude", line.as_bytes());
        assert!(
            handle.is_some(),
            "should parse session_id from plain JSON object"
        );
        assert_eq!(handle.unwrap().id, "abc-123");
    }
}

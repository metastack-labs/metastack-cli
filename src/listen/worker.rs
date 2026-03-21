use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result, anyhow, bail};

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
use crate::fs::{PlanningPaths, canonicalize_existing_dir};
use crate::linear::{
    IssueListFilters, IssueSummary, LinearClient, LinearService, ReqwestLinearClient, WorkflowState,
};
use crate::repo_target::RepoTarget;
use crate::workflow_contract::render_workflow_contract;

use super::{
    BACKLOG_STATE, MAX_STALLED_TURNS, SessionPhase, TokenUsage, agent_log_path,
    backlog_progress_for_issue_dir, capture_workspace_snapshot, compact_blocked_summary,
    compact_completed_summary, compact_running_summary, compare_workspace_snapshots,
    current_workspace_branch, issue_state_label, issue_team_key, listen_issue_is_active,
    now_epoch_seconds, now_timestamp, preflight, render_agent_prompt,
    try_transition_issue_to_review_state, workspace_has_meaningful_progress, write_listen_session,
};

const REQUIRED_LISTEN_PR_LABEL: &str = "metastack";
const LEGACY_LISTEN_PR_LABEL: &str = "symphony";

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
    let session_context = WorkerSessionContext {
        source_root: &source_root,
        project_selector,
        workspace_path: &workspace_path,
        branch: branch.as_deref(),
        workpad_comment_id: &args.workpad_comment_id,
        backlog_issue: backlog_issue.as_ref(),
        pid: Some(worker_pid),
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
}

#[derive(Debug, Default)]
struct TurnExecutionResult {
    session_id: Option<String>,
    usage: Option<AgentTokenUsage>,
}

async fn load_worker_issue<C>(service: &LinearService<C>, identifier: &str) -> Result<IssueSummary>
where
    C: LinearClient,
{
    service
        .find_issue_by_identifier(
            identifier,
            IssueListFilters {
                team: issue_team_key(identifier),
                limit: 250,
                ..IssueListFilters::default()
            },
        )
        .await?
        .ok_or_else(|| anyhow!("issue `{identifier}` was not found in Linear"))
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
        for line in BufReader::new(stdout).lines() {
            let line = line
                .with_context(|| format!("failed to read stdout for `{}`", log_path.display()))?;
            writeln!(stdout_log, "{line}")
                .with_context(|| format!("failed to write `{}`", log_path.display()))?;
            raw_stdout.push_str(&line);
            raw_stdout.push('\n');
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
        return Ok(TurnExecutionResult {
            session_id: parsed.continuation.or(continuation),
            usage: parsed.usage.or(usage),
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
        "Do not consider the task complete until the code is committed, pushed, a PR is opened, and the PR is attached back to the Linear issue.".to_string(),
        format!(
            "When publishing or updating the GitHub PR for this ticket, ensure the `{}` label is attached. If the repository does not have that label yet, create it and then attach it. Do not use the legacy `{}` label.",
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
    use super::{WorkerSessionContext, build_worker_session};
    use crate::linear::{IssueSummary, TeamRef};
    use crate::listen::{SessionPhase, TokenUsage};
    use std::path::Path;

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
}

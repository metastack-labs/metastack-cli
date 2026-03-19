pub mod dashboard;
mod preflight;
mod state;
pub(crate) mod store;
mod worker;
mod workpad;
mod workspace;

use std::collections::BTreeSet;
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use walkdir::WalkDir;

use crate::agents::{AgentBriefRequest, TicketMetadata, write_agent_brief};
use crate::backlog::{
    BacklogIssueMetadata, INDEX_FILE_NAME, ManagedFileRecord, TemplateContext,
    render_template_files, save_issue_metadata, write_issue_description,
};
use crate::cli::{
    ListenRunArgs, ListenSessionClearArgs, ListenSessionInspectArgs, ListenSessionListArgs,
    ListenSessionResumeArgs, ListenWorkerArgs,
};
use crate::config::{
    AppConfig, LinearConfig, LinearConfigOverrides, ListenAssignmentScope, PlanningListenSettings,
    PlanningMeta, load_required_planning_meta,
};
use crate::fs::{PlanningPaths, canonicalize_existing_dir, display_path};
use crate::linear::{
    IssueComment, IssueEditSpec, IssueListFilters, IssueSummary, LinearClient, LinearService,
    ReqwestLinearClient, UserRef,
};
use crate::listen::workpad::{extract_requirements, render_bootstrap_workpad};
use crate::listen::workspace::{TicketWorkspace, ensure_ticket_workspace};
use crate::scaffold::ensure_planning_layout;
pub use state::{AgentSession, PendingIssue, SessionPhase, TokenUsage};
use state::{COMPLETED_SESSION_TTL_SECONDS, ListenState};
use store::{
    ListenProjectStore, SessionSelector, StoredListenProjectSummary, pid_is_running,
    resolve_source_project_root,
};

const TODO_STATE: &str = "Todo";
const BACKLOG_STATE: &str = "Backlog";
const IN_PROGRESS_STATE: &str = "In Progress";
const ISSUE_ATTACHMENT_CONTEXT_FILES_DIR: &str = "files";
const DEFAULT_LISTEN_MAX_TURNS: u32 = 20;
const MAX_STALLED_TURNS: u32 = 2;
const TERMINAL_REFRESH_INTERVAL_SECONDS: u64 = 1;
const DEMO_NOW_EPOCH_SECONDS: u64 = 1_773_575_600;
const DEMO_START_EPOCH_SECONDS: u64 = DEMO_NOW_EPOCH_SECONDS - 7_351;
const REVIEW_STATE_CANDIDATES: &[&str] =
    &["Human Review", "In Review", "Review", "Ready for Review"];
#[derive(Debug, Clone)]
pub struct ListenDashboardData {
    pub title: String,
    pub scope: String,
    pub cycle_summary: String,
    pub runtime: ListenRuntimeSummary,
    pub pending_issues: Vec<PendingIssue>,
    pub sessions: Vec<AgentSession>,
    pub notes: Vec<String>,
    pub state_file: String,
}

impl ListenDashboardData {
    pub fn render_summary(&self) -> String {
        let mut lines = vec![
            self.title.clone(),
            self.cycle_summary.clone(),
            format!("Agents: {}", self.runtime.agents),
            format!("Throughput: {}", self.runtime.throughput),
            format!("Runtime: {}", self.runtime.runtime),
            format!("Tokens: {}", self.runtime.tokens),
            format!("Rate Limits: {}", self.runtime.rate_limits),
            format!("Project: {}", self.runtime.project),
            format!("Dashboard: {}", self.runtime.dashboard),
            format!("Terminal refresh: {}", self.runtime.dashboard_refresh),
            format!("Linear refresh: {}", self.runtime.linear_refresh),
            format!("State file: {}", self.state_file),
        ];

        if !self.notes.is_empty() {
            lines.push("Notes:".to_string());
            for note in &self.notes {
                lines.push(format!("  - {note}"));
            }
        }

        if !self.sessions.is_empty() {
            lines.push("Sessions:".to_string());
            for session in &self.sessions {
                lines.push(format!(
                    "  - {} [{}] {}",
                    session.issue_identifier,
                    session.phase.display_label(),
                    session.summary
                ));
            }
        }

        lines.join("\n")
    }

    pub(crate) fn session_counts(&self) -> SessionListCounts {
        SessionListCounts::from_sessions(&self.sessions)
    }

    pub(crate) fn sessions_for_view(&self, view: SessionListView) -> Vec<&AgentSession> {
        self.sessions
            .iter()
            .filter(|session| match view {
                SessionListView::Active => !session.phase.is_completed(),
                SessionListView::Completed => session.phase.is_completed(),
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ListenRuntimeSummary {
    pub agents: String,
    pub throughput: String,
    pub runtime: String,
    pub tokens: String,
    pub rate_limits: String,
    pub project: String,
    pub dashboard: String,
    pub dashboard_refresh: String,
    pub linear_refresh: String,
    pub current_epoch_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SessionListView {
    #[default]
    Active,
    Completed,
}

impl SessionListView {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Completed => "Completed",
        }
    }

    pub(crate) fn toggle(self) -> Self {
        match self {
            Self::Active => Self::Completed,
            Self::Completed => Self::Active,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SessionListCounts {
    pub active: usize,
    pub completed: usize,
}

impl SessionListCounts {
    fn from_sessions(sessions: &[AgentSession]) -> Self {
        sessions
            .iter()
            .fold(Self::default(), |mut counts, session| {
                if session.phase.is_completed() {
                    counts.completed += 1;
                } else {
                    counts.active += 1;
                }
                counts
            })
    }
}

#[derive(Debug, Clone)]
struct ListenCycleData {
    scope: String,
    claimed_this_cycle: usize,
    pending_issues: Vec<PendingIssue>,
    sessions: Vec<AgentSession>,
    notes: Vec<String>,
    state_file: String,
    rate_limits: Option<String>,
}

impl ListenCycleData {
    fn loading(scope: String, state_file: String) -> Self {
        Self {
            scope,
            claimed_this_cycle: 0,
            pending_issues: Vec::new(),
            sessions: Vec::new(),
            notes: vec![
                "Starting dashboard before the first Linear refresh completes.".to_string(),
                "Refreshing current Todo issues, listener sessions, and workspace state now."
                    .to_string(),
            ],
            state_file,
            rate_limits: None,
        }
    }

    fn demo(root: &Path, state_file: String) -> Self {
        Self::demo_at(root, DEMO_NOW_EPOCH_SECONDS, state_file)
    }

    fn demo_at(_root: &Path, reference_now: u64, state_file: String) -> Self {
        Self {
            scope: "MET / MetaStack CLI".to_string(),
            claimed_this_cycle: 1,
            pending_issues: vec![PendingIssue {
                identifier: "MET-18".to_string(),
                title: "Dashboard filter polish".to_string(),
                project: Some("MetaStack CLI".to_string()),
                team_key: "MET".to_string(),
            }],
            sessions: vec![
                AgentSession {
                    issue_id: Some("019cedb422937651b0b4dfac4af6a640".to_string()),
                    issue_identifier: "MET-13".to_string(),
                    issue_title: "Agent Daemon".to_string(),
                    project_name: Some("MetaStack CLI".to_string()),
                    team_key: "MET".to_string(),
                    issue_url: "https://linear.app/metastack-backlog/issue/MET-13/agent-daemon"
                        .to_string(),
                    phase: SessionPhase::BriefReady,
                    summary: "Brief ready | backlog MET-14 | worker active".to_string(),
                    brief_path: Some(".metastack/agents/briefs/MET-13.md".to_string()),
                    backlog_issue_identifier: Some("MET-14".to_string()),
                    backlog_issue_title: Some("Technical: Agent Daemon".to_string()),
                    backlog_path: Some(".metastack/backlog/MET-14".to_string()),
                    workspace_path: Some("/tmp/metastack-cli-workspace/MET-13".to_string()),
                    branch: Some("met-13-agent-daemon".to_string()),
                    workpad_comment_id: Some("comment-met-13".to_string()),
                    updated_at_epoch_seconds: reference_now - 1_180,
                    pid: Some(95_388),
                    session_id: Some(
                        "019cedb4-2293-7651-b0b4-dfac4af6a640-019cedb4-229b-7453-825e-3e3da4e1bf2a"
                            .to_string(),
                    ),
                    turns: Some(1),
                    tokens: TokenUsage {
                        input: Some(9_614_112),
                        output: Some(8_120),
                    },
                    log_path: Some(".metastack/agents/sessions/MET-13.log".to_string()),
                },
                AgentSession {
                    issue_id: Some("019ceda50a417ef1bf964f26683c1570".to_string()),
                    issue_identifier: "MET-17".to_string(),
                    issue_title: "Branch PR reconciliation".to_string(),
                    project_name: Some("MetaStack CLI".to_string()),
                    team_key: "MET".to_string(),
                    issue_url: "https://linear.app/metastack-backlog/issue/MET-17/branch-pr-reconciliation"
                        .to_string(),
                    phase: SessionPhase::Claimed,
                    summary: "Claimed | preparing workspace".to_string(),
                    brief_path: None,
                    backlog_issue_identifier: None,
                    backlog_issue_title: None,
                    backlog_path: None,
                    workspace_path: Some("/tmp/metastack-cli-workspace/MET-17".to_string()),
                    branch: Some("met-17-branch-pr-reconciliation".to_string()),
                    workpad_comment_id: None,
                    updated_at_epoch_seconds: reference_now - 2_940,
                    pid: Some(96_104),
                    session_id: Some(
                        "019ceda5-0a41-7ef1-bf96-4f26683c1570-019ceda5-0a57-7820-b050-c05e112d66dd"
                            .to_string(),
                    ),
                    turns: Some(1),
                    tokens: TokenUsage {
                        input: Some(8_380_959),
                        output: Some(49_960),
                    },
                    log_path: Some(".metastack/agents/sessions/MET-17.log".to_string()),
                },
            ],
            notes: vec![
                "Demo mode: no Linear requests were made.".to_string(),
                "The live terminal dashboard adapts to the full viewport.".to_string(),
                format!("State file: {state_file}"),
            ],
            state_file,
            rate_limits: Some(
                "codex | primary 12% / reset 1,773,515,901s | secondary 8% / reset 1,773,855,871s | credits n/a".to_string(),
            ),
        }
    }

    fn apply_state_snapshot(&mut self, state: ListenState) {
        self.sessions = state.sorted_sessions();
    }
}

#[derive(Debug, Clone)]
struct DashboardRuntimeContext {
    started_at_epoch_seconds: u64,
    now_epoch_seconds: u64,
    poll_interval_seconds: u64,
    dashboard_label: &'static str,
    dashboard_refresh_seconds: u64,
    linear_refresh_seconds: u64,
}

#[derive(Debug)]
struct AgentDaemon<C> {
    root: PathBuf,
    store: ListenProjectStore,
    filters: IssueListFilters,
    max_pickups: usize,
    linear_config: LinearConfig,
    app_config: AppConfig,
    planning_meta: PlanningMeta,
    worker_agent: Option<String>,
    worker_model: Option<String>,
    worker_reasoning: Option<String>,
    listen_settings: PlanningListenSettings,
    viewer: Option<UserRef>,
    service: LinearService<C>,
}

#[derive(Debug, Clone, Default)]
struct SessionArtifacts<'a> {
    brief_path: Option<String>,
    backlog_issue: Option<&'a IssueSummary>,
    backlog_path: Option<String>,
    workspace: Option<&'a TicketWorkspace>,
    workpad_comment: Option<&'a IssueComment>,
    pid: Option<u32>,
    turns: Option<u32>,
    log_path: Option<String>,
}

#[derive(Debug, Clone)]
struct BacklogIssueContext {
    issue: IssueSummary,
    issue_dir: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct BacklogProgress {
    completed: usize,
    total: usize,
    next_step: Option<String>,
}

impl BacklogProgress {
    fn is_complete(&self) -> bool {
        self.total > 0 && self.completed == self.total
    }

    fn compact_label(&self) -> Option<String> {
        (self.total > 0).then(|| format!("{}/{} tasks", self.completed, self.total))
    }
}

#[derive(Debug, Clone, Default)]
struct TurnProgress {
    implementation_entries: Vec<String>,
}

impl TurnProgress {
    fn implementation_changed(&self) -> bool {
        !self.implementation_entries.is_empty()
    }
}

#[derive(Debug, Clone, Default)]
struct AttachmentContextSummary {
    downloaded_paths: Vec<String>,
    failed_downloads: Vec<String>,
}

impl AttachmentContextSummary {
    fn downloaded_count(&self) -> usize {
        self.downloaded_paths.len()
    }

    fn has_entries(&self) -> bool {
        self.downloaded_count() > 0 || !self.failed_downloads.is_empty()
    }

    fn compact_label(&self) -> Option<String> {
        if self.downloaded_count() == 0 {
            return (!self.failed_downloads.is_empty())
                .then(|| "attachment context failed".to_string());
        }

        Some(format!(
            "{} attachment context file{}",
            self.downloaded_count(),
            if self.downloaded_count() == 1 {
                ""
            } else {
                "s"
            }
        ))
    }
}

fn compact_session_summary(parts: impl IntoIterator<Item = Option<String>>) -> String {
    let parts = parts
        .into_iter()
        .flatten()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        "Waiting for progress".to_string()
    } else {
        parts.join(" | ")
    }
}

fn backlog_progress_summary(progress: Option<&BacklogProgress>) -> Option<String> {
    progress.and_then(|progress| {
        progress.compact_label().map(|label| {
            if let Some(next_step) = progress.next_step.as_deref() {
                format!("{label} - next: {}", truncate_summary(next_step, 56))
            } else {
                label
            }
        })
    })
}

fn turn_counter_summary(turn: u32, max_turns: u32) -> String {
    format!("turn {turn}/{max_turns}")
}

fn stalled_turns_summary(stalled_turns: u32) -> Option<String> {
    (stalled_turns > 0).then(|| {
        if stalled_turns == 1 {
            "1 stalled turn".to_string()
        } else {
            format!("{stalled_turns} stalled turns")
        }
    })
}

fn log_reference_summary(log_path: &Path) -> String {
    format!("see {}", log_path.display())
}

fn truncate_summary(value: &str, max_chars: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{}...", truncated.trim_end())
    } else {
        collapsed
    }
}

fn compact_pickup_summary(
    workpad_reused: bool,
    attachment_context: &AttachmentContextSummary,
    pid: u32,
) -> String {
    compact_session_summary([
        Some("Picked up from Todo".to_string()),
        Some("local backlog ready".to_string()),
        attachment_context.compact_label(),
        Some(if workpad_reused {
            "workpad reused".to_string()
        } else {
            "workpad created".to_string()
        }),
        Some(format!("worker pid {pid}")),
    ])
}

fn compact_running_summary(
    progress: Option<&BacklogProgress>,
    turns_completed: u32,
    max_turns: u32,
    stalled_turns: u32,
) -> String {
    compact_session_summary([
        backlog_progress_summary(progress),
        Some(turn_counter_summary(turns_completed, max_turns)),
        stalled_turns_summary(stalled_turns),
    ])
}

fn compact_completed_summary(
    progress: Option<&BacklogProgress>,
    turns_completed: u32,
    state_label: &str,
) -> String {
    compact_session_summary([
        Some("Complete".to_string()),
        backlog_progress_summary(progress),
        Some(format!("{turns_completed} turn(s)")),
        Some(format!("moved to `{state_label}`")),
    ])
}

fn compact_blocked_summary(
    headline: &str,
    progress: Option<&BacklogProgress>,
    log_path: &Path,
) -> String {
    compact_session_summary([
        Some(headline.to_string()),
        backlog_progress_summary(progress),
        Some(log_reference_summary(log_path)),
    ])
}

fn mark_running_session_stale(
    session: &mut AgentSession,
    issue_identifier: &str,
    fallback_log_path: &Path,
    pid: u32,
) {
    let log_path = session
        .log_path
        .clone()
        .unwrap_or_else(|| fallback_log_path.display().to_string());
    session.phase = SessionPhase::Blocked;
    session.log_path = Some(log_path.clone());
    session.summary = compact_session_summary([
        Some("Blocked | worker died".to_string()),
        Some(format!("stale pid {pid}")),
        Some(log_reference_summary(Path::new(&log_path))),
    ]);
    session.updated_at_epoch_seconds = now_epoch_seconds();
    if session.issue_identifier.is_empty() {
        session.issue_identifier = issue_identifier.to_string();
    }
}

fn render_listen_backlog_file(
    relative_path: &str,
    contents: String,
    parent_issue: &IssueSummary,
) -> String {
    if relative_path != "validation.md" {
        return contents;
    }

    format!(
        "# Validation Plan\n\n## Command Proofs\n\n- Run the changed CLI flow against a deterministic local or mocked setup\n- Verify the original Linear issue description for `{}` remains unchanged\n- Update the existing `## Codex Workpad` comment with validation notes instead of running `meta sync push`\n\n## Notes\n\n- `meta listen` must not overwrite the primary Linear issue description.\n",
        parent_issue.identifier
    )
}

impl<C> AgentDaemon<C>
where
    C: LinearClient,
{
    async fn run_cycle(&self) -> Result<ListenCycleData> {
        self.store.ensure_layout()?;
        let mut state = self.store.load_state()?;
        let pending = self.service.list_issues(self.filters.clone()).await?;
        let mut notes = vec![format!(
            "Observed {} Todo issue(s) from Linear.",
            pending.len()
        )];
        self.reconcile_sessions(&mut state, &mut notes).await?;
        if let Some(label) = self.listen_settings.required_label.as_deref() {
            notes.push(format!("Listen label filter is active: `{label}`."));
        }
        if self.listen_settings.assignment_scope == ListenAssignmentScope::Viewer
            && let Some(viewer) = &self.viewer
        {
            notes.push(format!(
                "Listen assignee filter is active: only issues assigned to `{}` are eligible.",
                viewer.name
            ));
        }
        let mut claimed_this_cycle = 0usize;
        let mut claimed_identifiers = Vec::new();
        let mut eligible = Vec::new();

        for issue in &pending {
            if state.blocks_pickup(&issue.identifier) {
                continue;
            }
            if let Some(reason) = self.skip_reason(issue) {
                notes.push(format!("Skipped {}: {}.", issue.identifier, reason));
                continue;
            }
            eligible.push(issue.clone());
            if eligible.len() >= self.max_pickups.max(1) {
                break;
            }
        }

        for issue in &eligible {
            let session = self.pickup_issue(issue).await?;
            notes.push(format!(
                "Picked up {} and recorded stage `{}`.",
                session.issue_identifier,
                session.phase.label()
            ));
            claimed_this_cycle += 1;
            claimed_identifiers.push(issue.identifier.clone());
            state.upsert(session);
        }

        let pending_issues = pending
            .into_iter()
            .filter(|issue| {
                !claimed_identifiers
                    .iter()
                    .any(|identifier| identifier.eq_ignore_ascii_case(&issue.identifier))
            })
            .map(PendingIssue::from)
            .collect::<Vec<_>>();
        let sessions = state.sorted_sessions();
        self.store.save_state(&state)?;

        let scope = match (&self.filters.team, &self.filters.project) {
            (Some(team), Some(project)) => format!("{team} / {project}"),
            (Some(team), None) => team.clone(),
            (None, Some(project)) => project.clone(),
            (None, None) => "all teams".to_string(),
        };
        let state_file = display_path(&self.store.paths().state_path, &self.root);
        notes.push(format!("Persisted daemon state to {state_file}."));

        Ok(ListenCycleData {
            scope,
            claimed_this_cycle,
            pending_issues,
            sessions,
            notes,
            state_file,
            rate_limits: None,
        })
    }

    async fn reconcile_sessions(
        &self,
        state: &mut ListenState,
        notes: &mut Vec<String>,
    ) -> Result<()> {
        let pruned = state.prune_completed_sessions_older_than(
            now_epoch_seconds(),
            COMPLETED_SESSION_TTL_SECONDS,
        );
        if !pruned.is_empty() {
            notes.push(format!(
                "Pruned {} completed session(s) older than the 24-hour TTL: {}.",
                pruned.len(),
                pruned
                    .iter()
                    .map(|session| session.issue_identifier.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        let existing_sessions = state.sessions.clone();
        let mut reconciled = Vec::with_capacity(existing_sessions.len());

        for mut session in existing_sessions {
            let issue = match self.service.load_issue(&session.issue_identifier).await {
                Ok(issue) => issue,
                Err(error) => {
                    notes.push(format!(
                        "Retained {} session state because Linear refresh failed: {error:#}.",
                        session.issue_identifier
                    ));
                    reconciled.push(session);
                    continue;
                }
            };

            session.issue_id = Some(issue.id.clone());
            session.issue_title = issue.title.clone();
            session.project_name = issue.project.as_ref().map(|project| project.name.clone());
            session.team_key = issue.team.key.clone();
            session.issue_url = issue.url.clone();

            if !listen_issue_is_active(issue.state.as_ref().map(|state| state.name.as_str())) {
                if !matches!(session.phase, SessionPhase::Completed) {
                    session.phase = SessionPhase::Completed;
                    session.summary = compact_session_summary([
                        Some("Complete".to_string()),
                        Some(format!("moved to `{}`", issue_state_label(&issue))),
                    ]);
                    session.updated_at_epoch_seconds = now_epoch_seconds();
                }
                reconciled.push(session);
                continue;
            }

            if matches!(
                session.phase,
                SessionPhase::Completed | SessionPhase::Blocked
            ) {
                if normalize_issue_state_name(issue_state_label(&issue).as_str()) == "todo" {
                    notes.push(format!(
                        "{} returned to Todo; clearing the previous `{}` session so it can be picked up again.",
                        issue.identifier,
                        session.phase.label()
                    ));
                    continue;
                }

                reconciled.push(session);
                continue;
            }

            if matches!(session.phase, SessionPhase::Running)
                && session.pid.is_some_and(pid_is_running)
            {
                reconciled.push(session);
                continue;
            }

            if matches!(session.phase, SessionPhase::Running)
                && let Some(pid) = session.pid
            {
                mark_running_session_stale(
                    &mut session,
                    &issue.identifier,
                    &self.agent_log_path(&issue.identifier),
                    pid,
                );
                notes.push(format!(
                    "{} worker pid {} was no longer running; marked the stored session blocked instead of auto-resuming it.",
                    session.issue_identifier, pid
                ));
                reconciled.push(session);
                continue;
            }

            let (Some(workspace_path), Some(workpad_comment_id)) = (
                session.workspace_path.as_deref(),
                session.workpad_comment_id.as_deref(),
            ) else {
                session.phase = SessionPhase::Blocked;
                session.summary = "Blocked | missing workspace or workpad context".to_string();
                session.updated_at_epoch_seconds = now_epoch_seconds();
                reconciled.push(session);
                continue;
            };

            let workspace_path = PathBuf::from(workspace_path);
            if !workspace_path.is_dir() {
                session.phase = SessionPhase::Blocked;
                session.summary = "Blocked | workspace missing".to_string();
                session.updated_at_epoch_seconds = now_epoch_seconds();
                reconciled.push(session);
                continue;
            }

            match self.spawn_listen_worker_from_context(
                &issue,
                &workspace_path,
                workpad_comment_id,
                session.backlog_issue_identifier.as_deref(),
            ) {
                Ok(pid) => {
                    session.phase = SessionPhase::Running;
                    session.pid = Some(pid);
                    session.summary = compact_session_summary([
                        Some("Resumed worker".to_string()),
                        Some(format!("pid {pid}")),
                        session
                            .backlog_issue_identifier
                            .as_ref()
                            .map(|identifier| format!("backlog {identifier}")),
                    ]);
                    session.updated_at_epoch_seconds = now_epoch_seconds();
                    session.log_path =
                        Some(self.agent_log_path(&issue.identifier).display().to_string());
                    notes.push(format!(
                        "Resumed {} with a fresh listen worker pid {}.",
                        issue.identifier, pid
                    ));
                    reconciled.push(session);
                }
                Err(_error) => {
                    session.phase = SessionPhase::Blocked;
                    session.summary = compact_session_summary([Some(
                        "Blocked | worker resume failed".to_string(),
                    )]);
                    session.updated_at_epoch_seconds = now_epoch_seconds();
                    reconciled.push(session);
                }
            }
        }

        state.sessions = reconciled;
        Ok(())
    }

    async fn ensure_backlog_issue(
        &self,
        parent_issue: &IssueSummary,
        workspace_path: &Path,
    ) -> Result<BacklogIssueContext> {
        let backlog_issue = parent_issue.clone();
        let issue_dir =
            PlanningPaths::new(workspace_path).backlog_issue_dir(&parent_issue.identifier);
        let rendered_files = render_template_files(
            workspace_path,
            &TemplateContext {
                issue_identifier: Some(parent_issue.identifier.clone()),
                issue_title: Some(parent_issue.title.clone()),
                issue_url: Some(parent_issue.url.clone()),
                parent_identifier: Some(parent_issue.identifier.clone()),
                parent_title: Some(parent_issue.title.clone()),
                parent_url: Some(parent_issue.url.clone()),
                parent_description: parent_issue.description.clone(),
                ..TemplateContext::default()
            },
        )?;
        for rendered in rendered_files {
            let path = issue_dir.join(&rendered.relative_path);
            if path.exists() {
                continue;
            }

            let contents = if rendered.relative_path == INDEX_FILE_NAME {
                render_listen_backlog_description(
                    &rendered.contents,
                    parent_issue,
                    parent_issue.description.as_deref(),
                )
            } else {
                render_listen_backlog_file(&rendered.relative_path, rendered.contents, parent_issue)
            };
            fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| anyhow!("missing parent directory for `{}`", path.display()))?,
            )
            .with_context(|| format!("failed to create `{}`", issue_dir.display()))?;
            fs::write(&path, contents)
                .with_context(|| format!("failed to write `{}`", path.display()))?;
        }

        if !issue_dir.join(INDEX_FILE_NAME).exists() {
            let index_contents = render_listen_backlog_description(
                "",
                parent_issue,
                parent_issue.description.as_deref(),
            );
            write_issue_description(workspace_path, &parent_issue.identifier, &index_contents)?;
        }
        save_issue_metadata(
            &issue_dir,
            &BacklogIssueMetadata {
                issue_id: parent_issue.id.clone(),
                identifier: parent_issue.identifier.clone(),
                title: parent_issue.title.clone(),
                url: parent_issue.url.clone(),
                team_key: parent_issue.team.key.clone(),
                project_id: parent_issue
                    .project
                    .as_ref()
                    .map(|project| project.id.clone()),
                project_name: parent_issue
                    .project
                    .as_ref()
                    .map(|project| project.name.clone()),
                parent_id: Some(parent_issue.id.clone()),
                parent_identifier: Some(parent_issue.identifier.clone()),
                local_hash: None,
                remote_hash: None,
                last_sync_at: None,
                managed_files: Vec::<ManagedFileRecord>::new(),
            },
        )?;

        Ok(BacklogIssueContext {
            issue: backlog_issue,
            issue_dir,
        })
    }

    async fn pickup_issue(&self, issue: &IssueSummary) -> Result<AgentSession> {
        let updated_issue = self
            .service
            .edit_issue(IssueEditSpec {
                identifier: issue.identifier.clone(),
                title: None,
                description: None,
                project: None,
                state: Some(IN_PROGRESS_STATE.to_string()),
                priority: None,
            })
            .await?;

        let brief_metadata = TicketMetadata {
            title: Some(issue.title.clone()),
            description: issue.description.clone(),
            url: Some(issue.url.clone()),
            state: updated_issue
                .state
                .as_ref()
                .map(|state| state.name.clone())
                .or_else(|| Some(IN_PROGRESS_STATE.to_string())),
        };
        let updated_at_epoch_seconds = now_epoch_seconds();
        let detailed_issue = match self.service.load_issue(&updated_issue.identifier).await {
            Ok(issue) => issue,
            Err(_error) => {
                return Ok(self.build_session(
                    &updated_issue,
                    SessionPhase::Blocked,
                    "Blocked | issue refresh failed before workspace setup".to_string(),
                    SessionArtifacts::default(),
                    updated_at_epoch_seconds,
                ));
            }
        };

        let workspace = match ensure_ticket_workspace(
            &self.root,
            self.listen_settings.refresh_policy,
            &detailed_issue.identifier,
            &detailed_issue.title,
        ) {
            Ok(workspace) => workspace,
            Err(_error) => {
                return Ok(self.build_session(
                    &detailed_issue,
                    SessionPhase::Blocked,
                    "Blocked | workspace setup failed".to_string(),
                    SessionArtifacts::default(),
                    updated_at_epoch_seconds,
                ));
            }
        };
        if let Err(error) = preflight::run_listen_preflight(
            &self.service,
            &self.linear_config,
            &self.app_config,
            &self.planning_meta,
            preflight::ListenPreflightRequest {
                working_dir: &workspace.workspace_path,
                agent: self.worker_agent.as_deref(),
                model: self.worker_model.as_deref(),
                reasoning: self.worker_reasoning.as_deref(),
                require_write_access: true,
            },
        )
        .await
        {
            let log_path = self.agent_log_path(&detailed_issue.identifier);
            let _ = worker::write_preflight_failure(&log_path, &error);
            return Ok(self.build_session(
                &detailed_issue,
                SessionPhase::Blocked,
                compact_session_summary([
                    Some("Blocked | missing exec capability".to_string()),
                    Some(truncate_summary(&error.to_string(), 72)),
                ]),
                SessionArtifacts {
                    workspace: Some(&workspace),
                    log_path: Some(log_path.display().to_string()),
                    ..SessionArtifacts::default()
                },
                updated_at_epoch_seconds,
            ));
        }
        let backlog_issue = match self
            .ensure_backlog_issue(&detailed_issue, &workspace.workspace_path)
            .await
        {
            Ok(backlog_issue) => backlog_issue,
            Err(_error) => {
                return Ok(self.build_session(
                    &detailed_issue,
                    SessionPhase::Blocked,
                    "Blocked | backlog setup failed".to_string(),
                    SessionArtifacts {
                        workspace: Some(&workspace),
                        ..SessionArtifacts::default()
                    },
                    updated_at_epoch_seconds,
                ));
            }
        };
        let attachment_context = match sync_issue_attachment_context(
            &self.service,
            &workspace.workspace_path,
            &detailed_issue,
        )
        .await
        {
            Ok(summary) => summary,
            Err(_error) => {
                return Ok(self.build_session(
                    &detailed_issue,
                    SessionPhase::Blocked,
                    "Blocked | attachment context setup failed".to_string(),
                    SessionArtifacts {
                        backlog_issue: Some(&backlog_issue.issue),
                        backlog_path: Some(backlog_issue.issue_dir.display().to_string()),
                        workspace: Some(&workspace),
                        ..SessionArtifacts::default()
                    },
                    updated_at_epoch_seconds,
                ));
            }
        };

        let brief_path = write_agent_brief(
            &workspace.workspace_path,
            AgentBriefRequest {
                ticket: detailed_issue.identifier.clone(),
                title_override: Some(detailed_issue.title.clone()),
                goal: Some("Picked up automatically by `meta listen`.".to_string()),
                metadata: brief_metadata,
                output: None,
            },
        )
        .map(|path| path.display().to_string())
        .ok();

        let timestamp = now_timestamp();
        let workpad_body = render_bootstrap_workpad(&detailed_issue, &workspace, &timestamp);
        let existing_workpad_comment = active_workpad_comment(&detailed_issue);
        let workpad_reused = existing_workpad_comment.is_some();
        let workpad_comment = if let Some(comment) = existing_workpad_comment {
            comment
        } else {
            match self
                .service
                .upsert_workpad_comment(&detailed_issue, workpad_body)
                .await
            {
                Ok(comment) => comment,
                Err(_error) => {
                    return Ok(self.build_session(
                        &detailed_issue,
                        SessionPhase::Blocked,
                        compact_session_summary([
                            Some("Blocked | workpad sync failed".to_string()),
                            Some("local backlog".to_string()),
                        ]),
                        SessionArtifacts {
                            brief_path: brief_path.clone(),
                            backlog_issue: Some(&backlog_issue.issue),
                            backlog_path: Some(backlog_issue.issue_dir.display().to_string()),
                            workspace: Some(&workspace),
                            ..SessionArtifacts::default()
                        },
                        updated_at_epoch_seconds,
                    ));
                }
            }
        };

        let log_path = self.agent_log_path(&detailed_issue.identifier);
        match self.spawn_listen_worker(
            &detailed_issue,
            &workspace,
            &workpad_comment,
            Some(backlog_issue.issue.identifier.as_str()),
        ) {
            Ok(pid) => Ok(self.build_session(
                &detailed_issue,
                SessionPhase::Running,
                compact_pickup_summary(workpad_reused, &attachment_context, pid),
                SessionArtifacts {
                    brief_path,
                    backlog_issue: Some(&backlog_issue.issue),
                    backlog_path: Some(backlog_issue.issue_dir.display().to_string()),
                    workspace: Some(&workspace),
                    workpad_comment: Some(&workpad_comment),
                    pid: Some(pid),
                    turns: Some(0),
                    log_path: Some(log_path.display().to_string()),
                },
                updated_at_epoch_seconds,
            )),
            Err(_error) => Ok(self.build_session(
                &detailed_issue,
                SessionPhase::Blocked,
                compact_session_summary([
                    Some("Blocked | worker launch failed".to_string()),
                    Some("local backlog".to_string()),
                ]),
                SessionArtifacts {
                    brief_path,
                    backlog_issue: Some(&backlog_issue.issue),
                    backlog_path: Some(backlog_issue.issue_dir.display().to_string()),
                    workspace: Some(&workspace),
                    workpad_comment: Some(&workpad_comment),
                    log_path: Some(log_path.display().to_string()),
                    ..SessionArtifacts::default()
                },
                updated_at_epoch_seconds,
            )),
        }
    }

    fn skip_reason(&self, issue: &IssueSummary) -> Option<String> {
        let mut reasons = Vec::new();

        if let Some(required_label) = self.listen_settings.required_label.as_deref()
            && !issue
                .labels
                .iter()
                .any(|label| label.name.eq_ignore_ascii_case(required_label))
        {
            reasons.push(format!("missing required label `{required_label}`"));
        }

        if self.listen_settings.assignment_scope == ListenAssignmentScope::Viewer
            && let Some(viewer) = &self.viewer
        {
            match issue.assignee.as_ref() {
                Some(assignee) if assignee.id == viewer.id => {}
                Some(assignee) => reasons.push(format!(
                    "assigned to `{}` instead of `{}`",
                    assignee.name, viewer.name
                )),
                None => reasons.push("issue is unassigned".to_string()),
            }
        }

        (!reasons.is_empty()).then(|| reasons.join("; "))
    }

    fn build_session(
        &self,
        issue: &IssueSummary,
        phase: SessionPhase,
        summary: String,
        artifacts: SessionArtifacts<'_>,
        updated_at_epoch_seconds: u64,
    ) -> AgentSession {
        AgentSession {
            issue_id: Some(issue.id.clone()),
            issue_identifier: issue.identifier.clone(),
            issue_title: issue.title.clone(),
            project_name: issue.project.as_ref().map(|project| project.name.clone()),
            team_key: issue.team.key.clone(),
            issue_url: issue.url.clone(),
            phase,
            summary,
            brief_path: artifacts.brief_path,
            backlog_issue_identifier: artifacts
                .backlog_issue
                .map(|backlog_issue| backlog_issue.identifier.clone()),
            backlog_issue_title: artifacts
                .backlog_issue
                .map(|backlog_issue| backlog_issue.title.clone()),
            backlog_path: artifacts.backlog_path,
            workspace_path: artifacts
                .workspace
                .map(|entry| entry.workspace_path.display().to_string()),
            branch: artifacts.workspace.map(|entry| entry.branch.clone()),
            workpad_comment_id: artifacts.workpad_comment.map(|comment| comment.id.clone()),
            updated_at_epoch_seconds,
            pid: artifacts.pid.filter(|pid| *pid > 0),
            session_id: Some(issue.id.clone()),
            turns: artifacts.turns.or(Some(0)),
            tokens: TokenUsage::default(),
            log_path: artifacts.log_path,
        }
    }

    fn spawn_listen_worker(
        &self,
        issue: &IssueSummary,
        workspace: &TicketWorkspace,
        workpad_comment: &IssueComment,
        backlog_issue_identifier: Option<&str>,
    ) -> Result<u32> {
        self.spawn_listen_worker_from_context(
            issue,
            &workspace.workspace_path,
            &workpad_comment.id,
            backlog_issue_identifier,
        )
    }

    fn agent_log_path(&self, identifier: &str) -> PathBuf {
        self.store.log_path(identifier)
    }

    fn spawn_listen_worker_from_context(
        &self,
        issue: &IssueSummary,
        workspace_path: &Path,
        workpad_comment_id: &str,
        backlog_issue_identifier: Option<&str>,
    ) -> Result<u32> {
        let current_exe =
            std::env::current_exe().context("failed to resolve the meta executable")?;
        let log_path = self.agent_log_path(&issue.identifier);
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }
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

        let mut command = Command::new(current_exe);
        command.current_dir(&self.root);
        command.arg("listen-worker").arg("--source-root").arg(
            self.root
                .to_str()
                .ok_or_else(|| anyhow!("source root is not valid utf-8"))?,
        );
        if let Some(project_selector) = self.store.identity().project_selector.as_deref() {
            command.arg("--project").arg(project_selector);
        }
        command
            .arg("--workspace")
            .arg(
                workspace_path
                    .to_str()
                    .ok_or_else(|| anyhow!("workspace path is not valid utf-8"))?,
            )
            .arg("--issue")
            .arg(&issue.identifier)
            .arg("--workpad-comment-id")
            .arg(workpad_comment_id)
            .arg("--max-turns")
            .arg(DEFAULT_LISTEN_MAX_TURNS.to_string());
        if let Some(backlog_issue_identifier) = backlog_issue_identifier {
            command.arg("--backlog-issue").arg(backlog_issue_identifier);
        }
        if let Some(agent) = self.worker_agent.as_deref() {
            command.arg("--agent").arg(agent);
        }
        if let Some(model) = self.worker_model.as_deref() {
            command.arg("--model").arg(model);
        }
        if let Some(reasoning) = self.worker_reasoning.as_deref() {
            command.arg("--reasoning").arg(reasoning);
        }
        command.stdout(Stdio::from(stdout));
        command.stderr(Stdio::from(stderr));
        command.stdin(Stdio::null());
        command.env("LINEAR_API_KEY", &self.linear_config.api_key);
        command.env("LINEAR_API_URL", &self.linear_config.api_url);
        if let Some(team) = self.linear_config.default_team.as_deref() {
            command.env("LINEAR_TEAM", team);
        }
        command.env("METASTACK_WORKSPACE_PATH", workspace_path);
        command.env("METASTACK_SOURCE_ROOT", &self.root);

        let child = command
            .spawn()
            .context("failed to launch the hidden listen worker")?;

        Ok(child.id())
    }
}

pub async fn run_listen_worker(args: &ListenWorkerArgs) -> Result<()> {
    worker::run_listen_worker(args).await
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceSnapshot {
    head_sha: String,
    status_entries: Vec<String>,
}

fn write_listen_session(
    root: &Path,
    project_selector: Option<&str>,
    session: AgentSession,
) -> Result<()> {
    ListenProjectStore::resolve(root, project_selector)?.upsert_session(session)
}

fn current_workspace_branch(workspace_path: &Path) -> Result<String> {
    git_stdout(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .context("failed to inspect the workspace branch")
}

fn agent_log_path(root: &Path, project_selector: Option<&str>, identifier: &str) -> PathBuf {
    ListenProjectStore::resolve(root, project_selector)
        .map(|store| store.log_path(identifier))
        .unwrap_or_else(|_| PathBuf::from(format!("{identifier}.log")))
}

fn capture_workspace_snapshot(
    workspace_path: &Path,
    issue_identifier: &str,
) -> Result<WorkspaceSnapshot> {
    let head_sha = git_stdout(workspace_path, &["rev-parse", "HEAD"])?;
    let status = git_stdout(
        workspace_path,
        &["status", "--short", "--untracked-files=all"],
    )?;
    let ignored_brief_path = display_path(
        &PlanningPaths::new(workspace_path)
            .agent_briefs_dir
            .join(format!("{issue_identifier}.md")),
        workspace_path,
    );
    let status_entries = status
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.contains(&ignored_brief_path))
        .map(str::to_string)
        .collect::<Vec<_>>();

    Ok(WorkspaceSnapshot {
        head_sha,
        status_entries,
    })
}

fn compare_workspace_snapshots(
    workspace_path: &Path,
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
) -> Result<TurnProgress> {
    let before_entries = before
        .status_entries
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let after_entries = after
        .status_entries
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut changed_entries = before_entries
        .symmetric_difference(&after_entries)
        .cloned()
        .collect::<Vec<_>>();
    if before.head_sha != after.head_sha {
        changed_entries.extend(committed_change_entries(
            workspace_path,
            &before.head_sha,
            &after.head_sha,
        )?);
    }
    changed_entries.sort();
    changed_entries.dedup();

    let (_, implementation_entries): (Vec<_>, Vec<_>) = changed_entries
        .iter()
        .cloned()
        .partition(|entry| workspace_entry_is_planning_artifact(entry));

    Ok(TurnProgress {
        implementation_entries,
    })
}

fn committed_change_entries(
    workspace_path: &Path,
    before_head: &str,
    after_head: &str,
) -> Result<Vec<String>> {
    let diff = git_stdout(
        workspace_path,
        &["diff", "--name-only", before_head, after_head],
    )?;
    Ok(diff
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn workspace_entry_is_planning_artifact(entry: &str) -> bool {
    let path = if let Some((_, path)) = entry.split_once(' ') {
        path.trim()
    } else {
        entry.trim()
    };
    let path = path.rsplit(" -> ").next().unwrap_or(path).trim();
    path.starts_with(".metastack/")
}

fn workspace_has_meaningful_progress(workspace_path: &Path) -> Result<bool> {
    let ahead_count = git_stdout(
        workspace_path,
        &["rev-list", "--count", "origin/main..HEAD"],
    )?;
    if ahead_count.trim().parse::<u64>().unwrap_or(0) > 0 {
        return Ok(true);
    }

    let status = git_stdout(
        workspace_path,
        &["status", "--short", "--untracked-files=all"],
    )?;
    Ok(status
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .any(|line| !workspace_entry_is_planning_artifact(line)))
}

fn active_workpad_comment(issue: &IssueSummary) -> Option<IssueComment> {
    issue.comments.iter().find_map(|comment| {
        (comment.resolved_at.is_none() && comment.body.contains("## Codex Workpad"))
            .then(|| comment.clone())
    })
}

fn backlog_progress_for_issue_dir(
    workspace_path: &Path,
    identifier: &str,
) -> Result<BacklogProgress> {
    let issue_dir = PlanningPaths::new(workspace_path).backlog_issue_dir(identifier);
    if !issue_dir.is_dir() {
        bail!(
            "technical backlog directory `{}` is missing",
            issue_dir.display()
        );
    }

    let mut progress = BacklogProgress::default();
    for entry in WalkDir::new(&issue_dir) {
        let entry = entry.with_context(|| {
            format!(
                "failed to traverse technical backlog directory `{}`",
                issue_dir.display()
            )
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        if entry
            .path()
            .extension()
            .and_then(|extension| extension.to_str())
            != Some("md")
        {
            continue;
        }

        let contents = fs::read_to_string(entry.path())
            .with_context(|| format!("failed to read `{}`", entry.path().display()))?;
        progress = combine_backlog_progress(progress, parse_checklist_progress(&contents));
    }

    Ok(progress)
}

fn combine_backlog_progress(left: BacklogProgress, right: BacklogProgress) -> BacklogProgress {
    BacklogProgress {
        completed: left.completed + right.completed,
        total: left.total + right.total,
        next_step: left.next_step.or(right.next_step),
    }
}

fn parse_checklist_progress(contents: &str) -> BacklogProgress {
    let mut progress = BacklogProgress::default();

    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("- [x] ")
            || trimmed.starts_with("- [X] ")
            || trimmed.starts_with("* [x] ")
            || trimmed.starts_with("* [X] ")
        {
            progress.completed += 1;
            progress.total += 1;
        } else if trimmed.starts_with("- [ ] ") || trimmed.starts_with("* [ ] ") {
            progress.total += 1;
            if progress.next_step.is_none() {
                progress.next_step = checklist_item_label(trimmed);
            }
        }
    }

    progress
}

fn checklist_item_label(line: &str) -> Option<String> {
    let (_, item) = line.split_once(']')?;
    let item = item
        .trim()
        .trim_start_matches(|ch: char| ch.is_ascii_digit() || ch == '.' || ch == '\\')
        .trim();
    (!item.is_empty()).then(|| item.to_string())
}

async fn try_transition_issue_to_review_state<C>(
    service: &LinearService<C>,
    issue: &IssueSummary,
) -> Result<Option<IssueSummary>>
where
    C: LinearClient,
{
    for candidate in REVIEW_STATE_CANDIDATES {
        match service
            .edit_issue(IssueEditSpec {
                identifier: issue.identifier.clone(),
                title: None,
                description: None,
                project: None,
                state: Some((*candidate).to_string()),
                priority: None,
            })
            .await
        {
            Ok(updated) => return Ok(Some(updated)),
            Err(error) if is_missing_review_state_error(&error) => continue,
            Err(error) => return Err(error),
        }
    }

    Ok(None)
}

fn is_missing_review_state_error(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("state `") && message.contains("was not found on team")
}

fn git_stdout(workspace_path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_path)
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

fn listen_issue_is_active(state_name: Option<&str>) -> bool {
    matches!(
        state_name.map(normalize_issue_state_name).as_deref(),
        Some("todo") | Some("in progress") | Some("rework") | Some("merging")
    )
}

fn issue_state_label(issue: &IssueSummary) -> String {
    issue
        .state
        .as_ref()
        .map(|state| state.name.clone())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn issue_team_key(identifier: &str) -> Option<String> {
    identifier.split_once('-').map(|(team, _)| team.to_string())
}

fn normalize_issue_state_name(state_name: &str) -> String {
    state_name.trim().to_ascii_lowercase()
}

fn listen_scope_label(
    team: Option<&str>,
    project_selector: Option<&str>,
    project_label: &str,
) -> String {
    match (team, project_selector) {
        (Some(team), Some(_)) => format!("{team} / {project_label}"),
        (Some(team), None) => team.to_string(),
        (None, Some(_)) => project_label.to_string(),
        (None, None) => "all teams".to_string(),
    }
}

pub fn run_listen_session_list(_: &ListenSessionListArgs) -> Result<String> {
    let projects = ListenProjectStore::list_projects()?;
    if projects.is_empty() {
        return Ok("No stored MetaListen project sessions were found.".to_string());
    }

    let now = now_epoch_seconds();
    let mut lines = vec![
        "Stored MetaListen project sessions:".to_string(),
        "KEY  PHASE  UPDATED  ISSUE  PROJECT  ROOT".to_string(),
    ];
    for project in projects {
        let latest = project.latest_session.as_ref();
        let phase = latest
            .map(|session| session.phase.display_label().to_string())
            .unwrap_or_else(|| "Idle".to_string());
        let updated = latest
            .map(|session| format_duration(now.saturating_sub(session.updated_at_epoch_seconds)))
            .unwrap_or_else(|| "-".to_string());
        let issue = latest
            .map(|session| session.issue_identifier.clone())
            .unwrap_or_else(|| "-".to_string());
        lines.push(format!(
            "{}  {}  {}  {}  {}  {}",
            compact_identifier(&project.metadata.project_key),
            phase,
            updated,
            issue,
            project.metadata.project_label,
            project.metadata.source_root
        ));
    }

    Ok(lines.join("\n"))
}

pub fn run_listen_session_inspect(args: &ListenSessionInspectArgs) -> Result<String> {
    let store = resolve_session_store(&args.target)?;
    let metadata = store_summary(&store)?;
    let mut lines = vec![
        format!("Project key: {}", metadata.metadata.project_key),
        format!("Project: {}", metadata.metadata.project_label),
        format!("Source root: {}", metadata.metadata.source_root),
        format!("MetaStack root: {}", metadata.metadata.metastack_root),
        format!("State file: {}", metadata.state_path.display()),
        format!("Lock file: {}", metadata.lock_path.display()),
        format!("Logs dir: {}", metadata.logs_dir.display()),
    ];

    if let Some(lock) = metadata.active_lock {
        lines.push(format!(
            "Active listener: pid {}{}",
            lock.pid,
            if pid_is_running(lock.pid) {
                ""
            } else {
                " (stale)"
            }
        ));
    } else {
        lines.push("Active listener: none".to_string());
    }

    if let Some(session) = metadata.latest_session {
        lines.push(String::new());
        lines.push("Latest session:".to_string());
        lines.push(format!("  - Issue: {}", session.issue_identifier));
        lines.push(format!("  - Title: {}", session.issue_title));
        lines.push(format!("  - Phase: {}", session.phase.display_label()));
        lines.push(format!("  - Summary: {}", session.summary));
        lines.push(format!(
            "  - Updated: {}",
            now_timestamp_for_epoch(session.updated_at_epoch_seconds)
        ));
        if let Some(workspace_path) = session.workspace_path {
            lines.push(format!("  - Workspace: {workspace_path}"));
        }
        if let Some(log_path) = session.log_path {
            lines.push(format!("  - Log: {log_path}"));
        }
    }

    let state = store.load_state()?;
    if !state.sessions.is_empty() {
        lines.push(String::new());
        lines.push("Tracked sessions:".to_string());
        for session in state.sorted_sessions() {
            lines.push(format!(
                "  - {} [{}] {}",
                session.issue_identifier,
                session.phase.display_label(),
                session.summary
            ));
        }
    }

    Ok(lines.join("\n"))
}

pub fn run_listen_session_clear(args: &ListenSessionClearArgs) -> Result<String> {
    let store = resolve_session_store(&args.target)?;
    let label = store.identity().project_label.clone();
    let key = store.identity().project_key.clone();
    let selector = clear_selector(args);
    let outcome = store.clear_sessions(&selector)?;
    if outcome.cleared_sessions.is_empty() {
        return Ok(format!(
            "No stored MetaListen sessions matched {} for project `{label}` ({key}); {} tracked session(s) remain.",
            selector.display_label(),
            outcome.remaining_sessions
        ));
    }

    let cleared = outcome
        .cleared_sessions
        .iter()
        .map(|session| {
            format!(
                "{} [{}]",
                session.issue_identifier,
                session.phase.display_label()
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "Cleared {} stored MetaListen session(s) matched by {} for project `{label}` ({key}): {}. {} tracked session(s) remain.",
        outcome.cleared_sessions.len(),
        selector.display_label(),
        cleared,
        outcome.remaining_sessions
    ))
}

pub async fn run_listen_session_resume(args: &ListenSessionResumeArgs) -> Result<()> {
    let store = match args.project_key.as_deref() {
        Some(project_key) => ListenProjectStore::from_project_key(project_key)?,
        None => resolve_project_store(&args.run.root, args.run.project.as_deref())?,
    };
    let mut run_args = args.run.clone();
    run_args.root = store.identity().source_root.clone();
    run_args.project = store.identity().project_selector.clone();
    run_listen(&run_args).await
}

pub async fn run_listen(args: &ListenRunArgs) -> Result<()> {
    let requested_root = canonicalize_existing_dir(&args.root)?;
    let root = resolve_source_project_root(&requested_root)?;
    let planning_meta = load_required_planning_meta(&root, "listen")?;
    ensure_planning_layout(&root, false)?;
    let app_config = AppConfig::load()?;
    let poll_interval_seconds = resolve_listen_poll_interval_seconds(args, &planning_meta);

    if args.demo {
        let store = resolve_project_store_for_run(&root, args.project.as_deref(), &planning_meta)?;
        let _lock = store.acquire_listener_lock(std::process::id())?;
        let demo_now = now_epoch_seconds();
        let demo_state_file = display_path(&store.paths().state_path, &root);
        let cycle = ListenCycleData::demo_at(&root, demo_now, demo_state_file.clone());
        if args.render_once {
            let cycle = ListenCycleData::demo(&root, demo_state_file.clone());
            let data = build_dashboard_data(
                &cycle,
                &DashboardRuntimeContext {
                    started_at_epoch_seconds: DEMO_START_EPOCH_SECONDS,
                    now_epoch_seconds: DEMO_NOW_EPOCH_SECONDS,
                    poll_interval_seconds,
                    dashboard_label: "terminal snapshot",
                    dashboard_refresh_seconds: TERMINAL_REFRESH_INTERVAL_SECONDS,
                    linear_refresh_seconds: poll_interval_seconds,
                },
            );
            println!(
                "{}",
                dashboard::render_dashboard(&data, args.width, args.height)?
            );
            return Ok(());
        }
        if args.once {
            let cycle = ListenCycleData::demo(&root, demo_state_file.clone());
            let data = build_dashboard_data(
                &cycle,
                &DashboardRuntimeContext {
                    started_at_epoch_seconds: DEMO_START_EPOCH_SECONDS,
                    now_epoch_seconds: DEMO_NOW_EPOCH_SECONDS,
                    poll_interval_seconds,
                    dashboard_label: "terminal summary",
                    dashboard_refresh_seconds: TERMINAL_REFRESH_INTERVAL_SECONDS,
                    linear_refresh_seconds: poll_interval_seconds,
                },
            );
            println!("{}", data.render_summary());
            return Ok(());
        }

        let initial_cycle = cycle.clone();
        run_live_loop(
            args,
            poll_interval_seconds,
            demo_now - 7_351,
            initial_cycle,
            false,
            move || {
                let cycle = cycle.clone();
                async move { Ok(cycle) }
            },
            |_| Ok(()),
        )
        .await?;
        return Ok(());
    }

    let startup_provider_preflight = match preflight::run_listen_provider_preflight(
        &app_config,
        &planning_meta,
        preflight::ListenPreflightRequest {
            working_dir: &root,
            agent: args.agent.as_deref(),
            model: args.model.as_deref(),
            reasoning: args.reasoning.as_deref(),
            require_write_access: false,
        },
    ) {
        Ok(report) => Some(report),
        Err(error) if !args.check && preflight::is_missing_agent_selection(&error) => None,
        Err(error) => {
            if args.check {
                bail!("{}", preflight::render_listen_preflight_report(Err(&error)));
            }
            return Err(error);
        }
    };

    let config = LinearConfig::new_with_root(
        Some(&root),
        LinearConfigOverrides {
            api_key: args.api_key.clone(),
            api_url: args.api_url.clone(),
            default_team: args.team.clone(),
            profile: args.profile.clone(),
        },
    )?;
    let store = resolve_project_store_for_run(&root, args.project.as_deref(), &planning_meta)?;
    let _lock = store.acquire_listener_lock(std::process::id())?;
    let client = ReqwestLinearClient::new(config.clone())?;
    let service = LinearService::new(client, config.default_team.clone());
    if let Some(provider_report) = startup_provider_preflight {
        let startup_preflight =
            preflight::complete_listen_preflight(&service, &config, provider_report).await;
        if args.check {
            match startup_preflight {
                Ok(report) => {
                    println!("{}", preflight::render_listen_preflight_report(Ok(&report)));
                    return Ok(());
                }
                Err(error) => {
                    bail!("{}", preflight::render_listen_preflight_report(Err(&error)));
                }
            }
        }
        match startup_preflight {
            Ok(report) => preflight::emit_listen_preflight_warnings(&report),
            Err(error) => return Err(error),
        }
    } else if args.check {
        unreachable!("`--check` exits on provider preflight failures before Linear validation");
    }
    let viewer = if planning_meta.listen.assignment_scope == ListenAssignmentScope::Viewer {
        Some(service.viewer().await?)
    } else {
        None
    };
    let daemon = AgentDaemon {
        root: root.clone(),
        store,
        filters: IssueListFilters {
            team: args.team.clone().or(config.default_team.clone()),
            project: args.project.clone(),
            project_id: if args.project.is_some() {
                None
            } else {
                planning_meta.linear.project_id.clone()
            },
            state: Some(TODO_STATE.to_string()),
            limit: args.limit.max(1),
        },
        max_pickups: args.max_pickups.max(1),
        linear_config: config.clone(),
        app_config,
        planning_meta: planning_meta.clone(),
        worker_agent: args.agent.clone(),
        worker_model: args.model.clone(),
        worker_reasoning: args.reasoning.clone(),
        listen_settings: planning_meta.listen.clone(),
        viewer,
        service,
    };

    if args.render_once {
        let cycle = daemon.run_cycle().await?;
        let now = now_epoch_seconds();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: now,
                now_epoch_seconds: now,
                poll_interval_seconds,
                dashboard_label: "terminal snapshot",
                dashboard_refresh_seconds: TERMINAL_REFRESH_INTERVAL_SECONDS,
                linear_refresh_seconds: 0,
            },
        );
        println!(
            "{}",
            dashboard::render_dashboard(&data, args.width, args.height)?
        );
        return Ok(());
    }

    if args.once {
        let cycle = daemon.run_cycle().await?;
        let now = now_epoch_seconds();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: now,
                now_epoch_seconds: now,
                poll_interval_seconds,
                dashboard_label: "terminal summary",
                dashboard_refresh_seconds: TERMINAL_REFRESH_INTERVAL_SECONDS,
                linear_refresh_seconds: 0,
            },
        );
        println!("{}", data.render_summary());
        return Ok(());
    }

    let started_at_epoch_seconds = now_epoch_seconds();
    let initial_cycle = ListenCycleData::loading(
        listen_scope_label(
            daemon.filters.team.as_deref(),
            daemon.store.identity().project_selector.as_deref(),
            &daemon.store.identity().project_label,
        ),
        display_path(&daemon.store.paths().state_path, &daemon.root),
    );
    run_live_loop(
        args,
        poll_interval_seconds,
        started_at_epoch_seconds,
        initial_cycle,
        true,
        || daemon.run_cycle(),
        |cycle| {
            cycle.apply_state_snapshot(daemon.store.load_state()?);
            Ok(())
        },
    )
    .await
}

fn resolve_session_store(
    target: &crate::cli::ListenSessionTargetArgs,
) -> Result<ListenProjectStore> {
    match target.project_key.as_deref() {
        Some(project_key) => ListenProjectStore::from_project_key(project_key),
        None => resolve_project_store(&target.root, target.project.as_deref()),
    }
}

fn resolve_project_store(
    root: &Path,
    explicit_project: Option<&str>,
) -> Result<ListenProjectStore> {
    let requested_root = canonicalize_existing_dir(root)?;
    let source_root = resolve_source_project_root(&requested_root)?;
    let planning_meta = load_required_planning_meta(&source_root, "listen")?;
    resolve_project_store_for_run(&source_root, explicit_project, &planning_meta)
}

fn resolve_project_store_for_run(
    root: &Path,
    explicit_project: Option<&str>,
    planning_meta: &PlanningMeta,
) -> Result<ListenProjectStore> {
    ListenProjectStore::resolve(
        root,
        effective_listen_project_selector(explicit_project, planning_meta).as_deref(),
    )
}

fn effective_listen_project_selector(
    explicit_project: Option<&str>,
    planning_meta: &PlanningMeta,
) -> Option<String> {
    explicit_project
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| planning_meta.linear.project_id.clone())
}

fn store_summary(store: &ListenProjectStore) -> Result<StoredListenProjectSummary> {
    let mut projects = ListenProjectStore::list_projects()?;
    projects
        .drain(..)
        .find(|project| project.metadata.project_key == store.identity().project_key)
        .ok_or_else(|| {
            anyhow!(
                "stored MetaListen project session `{}` was not found",
                store.identity().project_key
            )
        })
}

fn clear_selector(args: &ListenSessionClearArgs) -> SessionSelector {
    if let Some(identifier) = args.issue_identifier.as_deref() {
        SessionSelector::IssueIdentifier(identifier.to_string())
    } else if args.blocked {
        SessionSelector::Blocked
    } else if args.completed {
        SessionSelector::Completed
    } else if args.stale {
        SessionSelector::Stale
    } else {
        SessionSelector::All
    }
}

async fn run_live_loop<F, Fut, S>(
    _args: &ListenRunArgs,
    poll_interval_seconds: u64,
    started_at_epoch_seconds: u64,
    initial_cycle: ListenCycleData,
    refresh_immediately: bool,
    mut next_cycle: F,
    mut refresh_dashboard_state: S,
) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<ListenCycleData>>,
    S: FnMut(&mut ListenCycleData) -> Result<()>,
{
    let _initial_data = build_dashboard_data(
        &initial_cycle,
        &DashboardRuntimeContext {
            started_at_epoch_seconds,
            now_epoch_seconds: started_at_epoch_seconds,
            poll_interval_seconds,
            dashboard_label: "terminal dashboard (TUI)",
            dashboard_refresh_seconds: TERMINAL_REFRESH_INTERVAL_SECONDS,
            linear_refresh_seconds: if refresh_immediately {
                0
            } else {
                poll_interval_seconds
            },
        },
    );
    let linear_refresh_interval = Duration::from_secs(poll_interval_seconds);
    let terminal_refresh_interval = Duration::from_secs(TERMINAL_REFRESH_INTERVAL_SECONDS);

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut cycle = initial_cycle;
    let mut session_view = SessionListView::Active;
    let mut next_linear_refresh_at = if refresh_immediately {
        Instant::now()
    } else {
        Instant::now() + linear_refresh_interval
    };
    let mut next_terminal_refresh_at = Instant::now() + terminal_refresh_interval;

    loop {
        let now = now_epoch_seconds();
        let linear_refresh_seconds = next_linear_refresh_at
            .saturating_duration_since(Instant::now())
            .as_secs();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds,
                now_epoch_seconds: now,
                poll_interval_seconds,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: TERMINAL_REFRESH_INTERVAL_SECONDS,
                linear_refresh_seconds,
            },
        );
        terminal.draw(|frame| dashboard::render(frame, &data, session_view))?;

        let wait_for_input = next_linear_refresh_at
            .saturating_duration_since(Instant::now())
            .min(
                next_terminal_refresh_at
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(250)),
            );

        if event::poll(wait_for_input)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let ctrl_c =
                key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
            if ctrl_c || matches!(key.code, KeyCode::Char('q')) {
                break;
            } else if matches!(key.code, KeyCode::Tab) {
                session_view = session_view.toggle();
            } else if matches!(key.code, KeyCode::Left) {
                session_view = SessionListView::Active;
            } else if matches!(key.code, KeyCode::Right) {
                session_view = SessionListView::Completed;
            }
        }

        let now = Instant::now();
        if now >= next_linear_refresh_at {
            cycle = next_cycle().await?;
            let refreshed_at = Instant::now();
            next_linear_refresh_at = refreshed_at + linear_refresh_interval;
            next_terminal_refresh_at = refreshed_at + terminal_refresh_interval;
        } else if now >= next_terminal_refresh_at {
            refresh_dashboard_state(&mut cycle)?;
            next_terminal_refresh_at = Instant::now() + terminal_refresh_interval;
        }
    }

    Ok(())
}

fn resolve_listen_poll_interval_seconds(args: &ListenRunArgs, planning_meta: &PlanningMeta) -> u64 {
    args.poll_interval
        .unwrap_or_else(|| planning_meta.listen.poll_interval_seconds())
        .max(1)
}

fn build_dashboard_data(
    cycle: &ListenCycleData,
    runtime: &DashboardRuntimeContext,
) -> ListenDashboardData {
    let session_counts = SessionListCounts::from_sessions(&cycle.sessions);
    let tokens = aggregate_token_usage(&cycle.sessions)
        .map(|totals| {
            format!(
                "in {} | out {} | total {}",
                format_number(totals.input),
                format_number(totals.output),
                format_number(totals.total)
            )
        })
        .unwrap_or_else(|| "n/a".to_string());
    ListenDashboardData {
        title: "meta listen".to_string(),
        scope: cycle.scope.clone(),
        cycle_summary: format!(
            "{} pending, {} active / {} completed sessions, {} claimed this cycle",
            cycle.pending_issues.len(),
            session_counts.active,
            session_counts.completed,
            cycle.claimed_this_cycle
        ),
        runtime: ListenRuntimeSummary {
            agents: format!(
                "{} active / {} completed / {} queued",
                session_counts.active,
                session_counts.completed,
                cycle.pending_issues.len()
            ),
            throughput: format!(
                "{:.2} tps",
                cycle.claimed_this_cycle as f64 / runtime.poll_interval_seconds.max(1) as f64
            ),
            runtime: format_duration(
                runtime
                    .now_epoch_seconds
                    .saturating_sub(runtime.started_at_epoch_seconds),
            ),
            tokens,
            rate_limits: cycle.rate_limits.clone().unwrap_or_else(|| {
                "n/a (agent rate-limit telemetry is not surfaced by this CLI yet)".to_string()
            }),
            project: cycle.scope.clone(),
            dashboard: runtime.dashboard_label.to_string(),
            dashboard_refresh: format!("{}s", runtime.dashboard_refresh_seconds),
            linear_refresh: format!("{}s", runtime.linear_refresh_seconds),
            current_epoch_seconds: runtime.now_epoch_seconds,
        },
        pending_issues: cycle.pending_issues.clone(),
        sessions: cycle.sessions.clone(),
        notes: cycle.notes.clone(),
        state_file: cycle.state_file.clone(),
    }
}

fn aggregate_token_usage(sessions: &[AgentSession]) -> Option<TokenTotals> {
    let mut input = 0u64;
    let mut output = 0u64;
    let mut has_any = false;

    for session in sessions {
        if let Some(value) = session.tokens.input {
            has_any = true;
            input += value;
        }
        if let Some(value) = session.tokens.output {
            has_any = true;
            output += value;
        }
    }

    has_any.then_some(TokenTotals {
        input,
        output,
        total: input + output,
    })
}

fn render_agent_prompt(
    issue: &IssueSummary,
    workspace_path: &Path,
    workpad_comment_id: &str,
    backlog_issue: Option<&IssueSummary>,
    turn_number: u32,
    max_turns: u32,
) -> String {
    let labels = if issue.labels.is_empty() {
        "none".to_string()
    } else {
        issue
            .labels
            .iter()
            .map(|label| label.name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let assignee = issue
        .assignee
        .as_ref()
        .map(|assignee| assignee.name.clone())
        .unwrap_or_else(|| "unassigned".to_string());
    let description = issue
        .description
        .clone()
        .unwrap_or_else(|| "No description provided.".to_string());
    let continuation = if turn_number > 1 {
        format!(
            "Continuation context:\n\n- This is continuation turn #{turn_number} of {max_turns} because the ticket is still in an active state.\n- Resume from the current workspace and workpad state instead of restarting from scratch.\n- Do not repeat completed investigation or validation unless the new code changes require it.\n- Do not end the turn while the issue remains active unless you are blocked by missing required auth, permissions, or secrets.\n\n"
        )
    } else {
        String::new()
    };
    let state = issue_state_label(issue);
    let backlog_context = backlog_issue.map_or_else(String::new, |backlog_issue| {
        format!(
            "\nLocal backlog path: {}\nBacklog identifier: {}",
            PlanningPaths::new(workspace_path)
                .backlog_issue_dir(&backlog_issue.identifier)
                .display(),
            backlog_issue.identifier,
        )
    });
    let attachment_context = {
        let manifest_path =
            PlanningPaths::new(workspace_path).agent_issue_context_manifest_path(&issue.identifier);
        if manifest_path.is_file() {
            format!(
                "\nAttachment context: {}\nAttachment manifest: {}",
                manifest_path.parent().unwrap_or(workspace_path).display(),
                manifest_path.display()
            )
        } else {
            String::new()
        }
    };

    format!(
        "You are working on Linear ticket `{identifier}`\n\n{continuation}Issue context:\nIdentifier: {identifier}\nTitle: {title}\nCurrent status: {state}\nAssignee: {assignee}\nLabels: {labels}\nURL: {url}\nWorkspace: {workspace}\nTracking workpad comment ID: {comment_id}{backlog_context}{attachment_context}\n\nDescription:\n\n{description}",
        identifier = issue.identifier,
        title = issue.title,
        state = state,
        assignee = assignee,
        labels = labels,
        url = issue.url,
        workspace = workspace_path.display(),
        comment_id = workpad_comment_id,
        backlog_context = backlog_context,
        attachment_context = attachment_context,
        description = description,
        continuation = continuation,
    )
}

fn render_listen_backlog_description(
    template_index: &str,
    parent_issue: &IssueSummary,
    existing_description: Option<&str>,
) -> String {
    if let Some(existing_description) = existing_description
        && backlog_description_has_task_list(existing_description)
    {
        return existing_description.to_string();
    }

    let mut lines = Vec::new();
    if !template_index.trim().is_empty() {
        lines.push(template_index.trim_end().to_string());
    }

    lines.extend([
        String::new(),
        "_Generated by `meta listen`._".to_string(),
        String::new(),
        "## Source Issue".to_string(),
        String::new(),
        format!("- Identifier: `{}`", parent_issue.identifier),
        format!("- Title: {}", parent_issue.title),
        format!("- URL: {}", parent_issue.url),
    ]);

    let mut requirements = extract_requirements(parent_issue.description.as_deref());
    if requirements.is_empty()
        && let Some(description) = parent_issue.description.as_deref().map(str::trim)
        && !description.is_empty()
    {
        requirements.push(description.to_string());
    }
    if !requirements.is_empty() {
        lines.extend([String::new(), "## Requirements".to_string(), String::new()]);
        lines.extend(
            requirements
                .into_iter()
                .map(|requirement| format!("- [ ] {requirement}")),
        );
    }

    lines.join("\n")
}

fn backlog_description_has_task_list(description: &str) -> bool {
    description.contains("## Requirements")
        || description.contains("- [ ] ")
        || description.contains("- [x] ")
        || description.contains("- [X] ")
        || description.contains("* [ ] ")
        || description.contains("* [x] ")
        || description.contains("* [X] ")
}

async fn sync_issue_attachment_context<C>(
    service: &LinearService<C>,
    workspace_path: &Path,
    issue: &IssueSummary,
) -> Result<AttachmentContextSummary>
where
    C: LinearClient,
{
    let paths = PlanningPaths::new(workspace_path);
    let context_dir = paths.agent_issue_context_dir(&issue.identifier);
    if context_dir.exists() {
        fs::remove_dir_all(&context_dir)
            .with_context(|| format!("failed to reset `{}`", context_dir.display()))?;
    }
    if issue.attachments.is_empty() {
        return Ok(AttachmentContextSummary::default());
    }

    fs::create_dir_all(context_dir.join(ISSUE_ATTACHMENT_CONTEXT_FILES_DIR))
        .with_context(|| format!("failed to create `{}`", context_dir.display()))?;

    let mut summary = AttachmentContextSummary::default();
    for (index, attachment) in issue.attachments.iter().enumerate() {
        let filename = attachment_download_name(attachment, index);
        let relative_path = format!("{ISSUE_ATTACHMENT_CONTEXT_FILES_DIR}/{filename}");
        let destination = context_dir.join(&relative_path);
        match service.download_file(&attachment.url).await {
            Ok(contents) => {
                if let Some(parent) = destination.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("failed to create `{}`", parent.display()))?;
                }
                fs::write(&destination, contents)
                    .with_context(|| format!("failed to write `{}`", destination.display()))?;
                summary.downloaded_paths.push(relative_path);
            }
            Err(error) => summary.failed_downloads.push(format!(
                "{} ({}) - {}",
                attachment.title,
                attachment.url,
                truncate_summary(&error.to_string(), 120)
            )),
        }
    }

    let manifest_path = paths.agent_issue_context_manifest_path(&issue.identifier);
    fs::write(
        &manifest_path,
        render_issue_attachment_manifest(issue, &summary, workspace_path),
    )
    .with_context(|| format!("failed to write `{}`", manifest_path.display()))?;

    Ok(summary)
}

fn attachment_download_name(attachment: &crate::linear::AttachmentSummary, index: usize) -> String {
    let preferred = file_name_candidate(attachment.title.as_str())
        .or_else(|| file_name_candidate(attachment.url.as_str()))
        .unwrap_or_else(|| "attachment".to_string());
    format!(
        "{:02}-{}",
        index + 1,
        sanitize_attachment_file_name(&preferred)
    )
}

fn file_name_candidate(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let trimmed = trimmed
        .split('?')
        .next()
        .unwrap_or(trimmed)
        .trim_end_matches('/');
    Path::new(trimmed)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

fn sanitize_attachment_file_name(name: &str) -> String {
    let mut sanitized = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while sanitized.contains("--") {
        sanitized = sanitized.replace("--", "-");
    }
    sanitized = sanitized.trim_matches('-').to_string();
    if sanitized.is_empty() {
        "attachment".to_string()
    } else {
        sanitized
    }
}

fn render_issue_attachment_manifest(
    issue: &IssueSummary,
    summary: &AttachmentContextSummary,
    workspace_path: &Path,
) -> String {
    let context_dir = PlanningPaths::new(workspace_path).agent_issue_context_dir(&issue.identifier);
    let mut lines = vec![
        format!("# Attachment Context for {}", issue.identifier),
        String::new(),
        format!("- Source issue: `{}`", issue.identifier),
        format!("- Linear URL: {}", issue.url),
        format!("- Download directory: `{}`", context_dir.display()),
        format!("- Attached items discovered: {}", issue.attachments.len()),
        format!("- Files downloaded: {}", summary.downloaded_count()),
        format!("- Download failures: {}", summary.failed_downloads.len()),
    ];

    if !summary.downloaded_paths.is_empty() {
        lines.extend([
            String::new(),
            "## Downloaded Files".to_string(),
            String::new(),
        ]);
        for path in &summary.downloaded_paths {
            lines.push(format!("- `{path}`"));
        }
    }

    if !summary.failed_downloads.is_empty() {
        lines.extend([
            String::new(),
            "## Download Failures".to_string(),
            String::new(),
        ]);
        for failure in &summary.failed_downloads {
            lines.push(format!("- {failure}"));
        }
    }

    if !summary.has_entries() {
        lines.extend([
            String::new(),
            "No downloadable attachment context was available.".to_string(),
        ]);
    }

    lines.join("\n")
}

fn now_timestamp() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| now_epoch_seconds().to_string())
}

fn now_timestamp_for_epoch(epoch_seconds: u64) -> String {
    epoch_seconds.to_string()
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn format_duration(total_seconds: u64) -> String {
    if total_seconds < 60 {
        return format!("{total_seconds}s");
    }

    let total_minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    if total_minutes < 60 * 24 {
        return format!("{total_minutes}m {seconds:02}s");
    }

    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    format!("{hours}h {minutes:02}m")
}

fn format_number(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(digit);
    }

    formatted.chars().rev().collect()
}

fn compact_identifier(value: &str) -> String {
    if value.len() <= 14 {
        return value.to_string();
    }

    format!("{}...{}", &value[..4], &value[value.len() - 6..])
}

#[derive(Debug, Clone, Copy)]
struct TokenTotals {
    input: u64,
    output: u64,
    total: u64,
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentSession, ListenCycleData, ListenState, SessionPhase, TokenUsage,
        capture_workspace_snapshot, compact_identifier, format_duration, format_number,
        listen_scope_label, mark_running_session_stale,
    };
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn session_phase_has_human_labels() {
        assert_eq!(SessionPhase::Claimed.display_label(), "Claimed");
        assert_eq!(SessionPhase::BriefReady.display_label(), "Brief Ready");
        assert_eq!(SessionPhase::Running.display_label(), "Running");
        assert_eq!(SessionPhase::Completed.display_label(), "Completed");
        assert_eq!(SessionPhase::Blocked.html_class(), "danger");
        assert_eq!(SessionPhase::Completed.html_class(), "success");
    }

    #[test]
    fn number_formatter_adds_grouping() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(1_234_567), "1,234,567");
    }

    #[test]
    fn duration_formatter_matches_dashboard_style() {
        assert_eq!(format_duration(5), "5s");
        assert_eq!(format_duration(95), "1m 35s");
        assert_eq!(format_duration(5_423), "90m 23s");
    }

    #[test]
    fn compact_identifier_shortens_long_values() {
        assert_eq!(compact_identifier("short-id"), "short-id");
        assert_eq!(
            compact_identifier("019cedb4-2293-7651-b0b4-dfac4af6a640"),
            "019c...f6a640"
        );
    }

    #[test]
    fn token_usage_compact_display_uses_total() {
        let usage = TokenUsage {
            input: Some(12_300),
            output: Some(40),
        };

        assert_eq!(usage.display_compact(), "12,340");
    }

    #[test]
    fn dead_running_session_is_marked_blocked_and_stale() {
        let mut session = AgentSession {
            issue_id: Some("issue-1".to_string()),
            issue_identifier: "ENG-10163".to_string(),
            issue_title: "Listen cleanup".to_string(),
            project_name: Some("MetaStack CLI".to_string()),
            team_key: "MET".to_string(),
            issue_url: "https://linear.app/issues/eng-10163".to_string(),
            phase: SessionPhase::Running,
            summary: "Running".to_string(),
            brief_path: Some(".metastack/agents/briefs/ENG-10163.md".to_string()),
            backlog_issue_identifier: Some("TECH-1".to_string()),
            backlog_issue_title: Some("Backlog".to_string()),
            backlog_path: Some(".metastack/backlog/TECH-1".to_string()),
            workspace_path: Some("/tmp/ENG-10163".to_string()),
            branch: Some("eng-10163".to_string()),
            workpad_comment_id: Some("comment-1".to_string()),
            updated_at_epoch_seconds: 1,
            pid: Some(42_424),
            session_id: Some("session-1".to_string()),
            turns: Some(2),
            tokens: TokenUsage::default(),
            log_path: Some("logs/ENG-10163.log".to_string()),
        };

        mark_running_session_stale(&mut session, "ENG-10163", Path::new("fallback.log"), 42_424);

        assert_eq!(session.phase, SessionPhase::Blocked);
        assert_eq!(session.workspace_path.as_deref(), Some("/tmp/ENG-10163"));
        assert_eq!(session.log_path.as_deref(), Some("logs/ENG-10163.log"));
        assert!(session.summary.contains("Blocked | worker died"));
        assert!(session.summary.contains("stale pid 42424"));
        assert!(session.summary.contains("see logs/ENG-10163.log"));
    }

    #[test]
    fn loading_cycle_starts_empty_and_explains_initial_refresh() {
        let cycle = ListenCycleData::loading(
            "MET / MetaStack CLI".to_string(),
            ".metastack/agents/sessions/listen-state.json".to_string(),
        );

        assert_eq!(cycle.scope, "MET / MetaStack CLI");
        assert_eq!(cycle.claimed_this_cycle, 0);
        assert!(cycle.pending_issues.is_empty());
        assert!(cycle.sessions.is_empty());
        assert_eq!(
            cycle.notes,
            vec![
                "Starting dashboard before the first Linear refresh completes.".to_string(),
                "Refreshing current Todo issues, listener sessions, and workspace state now."
                    .to_string(),
            ]
        );
        assert_eq!(
            cycle.state_file,
            ".metastack/agents/sessions/listen-state.json"
        );
        assert_eq!(cycle.rate_limits, None);
    }

    #[test]
    fn cycle_state_snapshot_refreshes_sessions_without_resetting_linear_data() {
        let mut cycle = ListenCycleData::demo(
            Path::new("."),
            ".metastack/agents/sessions/listen-state.json".to_string(),
        );
        let existing_pending_count = cycle.pending_issues.len();
        let existing_pending_identifier = cycle
            .pending_issues
            .first()
            .map(|issue| issue.identifier.clone());
        let existing_notes = cycle.notes.clone();
        let existing_scope = cycle.scope.clone();
        let existing_claimed = cycle.claimed_this_cycle;

        let mut completed = cycle
            .sessions
            .first()
            .expect("demo cycle should include a session")
            .clone();
        completed.issue_identifier = "MET-99".to_string();
        completed.issue_title = "Completed ticket".to_string();
        completed.phase = SessionPhase::Completed;
        completed.summary = "Complete | moved to `Human Review`".to_string();
        completed.updated_at_epoch_seconds += 60;

        cycle.apply_state_snapshot(ListenState::from_sessions(vec![completed.clone()]));

        assert_eq!(cycle.sessions.len(), 1);
        assert_eq!(
            cycle.sessions[0].issue_identifier,
            completed.issue_identifier
        );
        assert_eq!(cycle.sessions[0].phase, SessionPhase::Completed);
        assert_eq!(cycle.sessions[0].summary, completed.summary);
        assert_eq!(cycle.pending_issues.len(), existing_pending_count);
        assert_eq!(
            cycle
                .pending_issues
                .first()
                .map(|issue| issue.identifier.clone()),
            existing_pending_identifier
        );
        assert_eq!(cycle.notes, existing_notes);
        assert_eq!(cycle.scope, existing_scope);
        assert_eq!(cycle.claimed_this_cycle, existing_claimed);
    }

    #[test]
    fn workspace_snapshot_ignores_generated_agent_brief() {
        let temp = tempdir().expect("tempdir should build");
        let repo = temp.path();
        run_git(repo, &["init"]).expect("git init should succeed");
        run_git(repo, &["config", "user.email", "listen@example.com"])
            .expect("git config should succeed");
        run_git(repo, &["config", "user.name", "Listen Tests"]).expect("git config should succeed");
        fs::write(repo.join("README.md"), "# Demo\n").expect("readme should write");
        run_git(repo, &["add", "README.md"]).expect("git add should succeed");
        run_git(repo, &["commit", "-m", "init"]).expect("git commit should succeed");

        let brief_path = repo.join(".metastack/agents/briefs/MET-36.md");
        fs::create_dir_all(
            brief_path
                .parent()
                .expect("brief path should have a parent"),
        )
        .expect("brief dir should build");
        fs::write(&brief_path, "# brief\n").expect("brief should write");

        let baseline =
            capture_workspace_snapshot(repo, "MET-36").expect("snapshot should build for brief");
        assert!(
            baseline.status_entries.is_empty(),
            "generated brief should not count as local progress: {:?}",
            baseline.status_entries
        );

        fs::write(repo.join("src.rs"), "fn main() {}\n").expect("source file should write");
        let updated =
            capture_workspace_snapshot(repo, "MET-36").expect("snapshot should build for change");
        assert_ne!(baseline, updated);
        assert!(
            updated
                .status_entries
                .iter()
                .any(|entry: &String| entry.contains("src.rs")),
            "expected src.rs in status entries: {:?}",
            updated.status_entries
        );
    }

    #[test]
    fn listen_scope_label_uses_effective_default_project_identity() {
        assert_eq!(
            listen_scope_label(Some("MET"), Some("project-default"), "project-default"),
            "MET / project-default"
        );
    }

    #[test]
    fn listen_scope_label_falls_back_to_team_without_project_scope() {
        assert_eq!(listen_scope_label(Some("MET"), None, "All projects"), "MET");
        assert_eq!(listen_scope_label(None, None, "All projects"), "all teams");
    }

    fn run_git(repo: &std::path::Path, args: &[&str]) -> anyhow::Result<()> {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()?;
        anyhow::ensure!(status.success(), "git {} failed", args.join(" "));
        Ok(())
    }
}

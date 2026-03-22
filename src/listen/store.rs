use std::ffi::OsStr;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::resolve_data_root;
use crate::fs::{PlanningPaths, canonicalize_existing_dir, ensure_dir};
use crate::listen::compact_session_summary;
use crate::session_runtime::{
    ActiveSessionFile, WorkflowRootLayout, read_json, read_optional_json_lossy, write_json,
};

use super::state::{
    AgentSession, COMPLETED_SESSION_TTL_SECONDS, ListenState, PullRequestStatus,
    PullRequestSummary, SessionPhase, TokenUsage,
};

const LISTEN_STORE_VERSION: u8 = 1;
const LISTEN_SESSION_DETAIL_VERSION: u8 = 1;
const LOG_EXCERPT_LIMIT: usize = 6;
const LOG_EXCERPT_MAX_CHARS: usize = 120;

#[derive(Debug, Clone)]
pub(crate) struct ListenProjectStore {
    identity: ListenProjectIdentity,
    paths: ListenProjectPaths,
}

#[derive(Debug, Clone)]
pub(super) struct ListenProjectIdentity {
    pub(super) project_key: String,
    pub(super) source_root: PathBuf,
    pub(super) metastack_root: PathBuf,
    pub(super) source_label: String,
    pub(super) project_selector: Option<String>,
    pub(super) project_label: String,
}

#[derive(Debug, Clone)]
pub(super) struct ListenProjectPaths {
    pub(super) layout: WorkflowRootLayout,
    pub(super) projects_root: PathBuf,
    pub(super) project_dir: PathBuf,
    pub(super) project_metadata_path: PathBuf,
    pub(super) state_path: PathBuf,
    pub(super) lock_path: PathBuf,
    pub(super) logs_dir: PathBuf,
    pub(super) details_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ListenProjectMetadata {
    pub(super) version: u8,
    pub(super) project_key: String,
    #[serde(default)]
    pub(super) project_selector: Option<String>,
    pub(super) project_label: String,
    pub(super) source_root: String,
    pub(super) metastack_root: String,
    #[serde(default)]
    pub(super) source_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ActiveListenerLock {
    pub(super) pid: u32,
    pub(super) acquired_at_epoch_seconds: u64,
    pub(super) source_root: String,
    pub(super) metastack_root: String,
}

#[derive(Debug, Clone)]
pub(super) struct StoredListenProjectSummary {
    pub(super) metadata: ListenProjectMetadata,
    pub(super) state_path: PathBuf,
    pub(super) lock_path: PathBuf,
    pub(super) logs_dir: PathBuf,
    pub(super) latest_session: Option<AgentSession>,
    pub(super) active_lock: Option<ActiveListenerLock>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionDetailReferences {
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub backlog_path: Option<String>,
    #[serde(default)]
    pub brief_path: Option<String>,
    #[serde(default)]
    pub workpad_comment_id: Option<String>,
    #[serde(default)]
    pub log_path: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionContextReference {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionMilestone {
    pub at_epoch_seconds: u64,
    pub phase: SessionPhase,
    pub summary: String,
    #[serde(default)]
    pub turns: Option<u32>,
    #[serde(default)]
    pub pull_request_status: PullRequestStatus,
    #[serde(default)]
    pub pull_request_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionLogExcerpt {
    pub line_number: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ListenSessionDetail {
    pub(super) version: u8,
    pub(super) issue_identifier: String,
    pub(super) issue_title: String,
    pub(super) updated_at_epoch_seconds: u64,
    pub(super) session_updated_at_epoch_seconds: u64,
    pub(super) phase: SessionPhase,
    pub(super) summary: String,
    #[serde(default)]
    pub turns: Option<u32>,
    #[serde(default)]
    pub tokens: TokenUsage,
    #[serde(default)]
    pub pull_request: PullRequestSummary,
    #[serde(default)]
    pub references: SessionDetailReferences,
    #[serde(default)]
    pub prompt_context: Vec<SessionContextReference>,
    #[serde(default)]
    pub milestones: Vec<SessionMilestone>,
    #[serde(default)]
    pub log_excerpts: Vec<SessionLogExcerpt>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionSelector {
    IssueIdentifier(String),
    Blocked,
    Completed,
    Stale,
    All,
}

impl SessionSelector {
    pub(super) fn display_label(&self) -> String {
        match self {
            Self::IssueIdentifier(identifier) => format!("issue `{identifier}`"),
            Self::Blocked => "`--blocked`".to_string(),
            Self::Completed => "`--completed`".to_string(),
            Self::Stale => "`--stale`".to_string(),
            Self::All => "`--all`".to_string(),
        }
    }

    fn matches(&self, session: &AgentSession) -> bool {
        match self {
            Self::IssueIdentifier(identifier) => session.issue_matches(identifier),
            Self::Blocked => matches!(session.phase, SessionPhase::Blocked),
            Self::Completed => matches!(session.phase, SessionPhase::Completed),
            Self::Stale => session.pid.is_some_and(|pid| !pid_is_running(pid)),
            Self::All => true,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SessionClearOutcome {
    pub(super) cleared_sessions: Vec<AgentSession>,
    pub(super) remaining_sessions: usize,
}

#[derive(Debug)]
pub(super) struct ListenerLockGuard {
    lock_path: PathBuf,
    pid: u32,
}

impl ListenProjectStore {
    /// Resolves the install-scoped listen project store for the provided repository root.
    ///
    /// Returns an error when the repository root or install-scoped data root cannot be resolved.
    pub(crate) fn resolve(root: &Path, project_selector: Option<&str>) -> Result<Self> {
        let data_root = resolve_data_root()?;
        Self::resolve_with_data_root(root, data_root, project_selector)
    }

    fn resolve_with_data_root(
        root: &Path,
        data_root: PathBuf,
        project_selector: Option<&str>,
    ) -> Result<Self> {
        let identity = resolve_project_identity(root, project_selector)?;
        let projects_root = data_root.join("listen").join("projects");
        let project_dir = projects_root.join(&identity.project_key);
        let layout =
            WorkflowRootLayout::install_scoped(project_dir.clone(), "active-listener.lock.json");
        let paths = ListenProjectPaths {
            layout: layout.clone(),
            projects_root,
            project_dir: project_dir.clone(),
            project_metadata_path: layout.path("project.json"),
            state_path: layout.path("session.json"),
            lock_path: layout.active_session_path().to_path_buf(),
            logs_dir: layout.path("logs"),
            details_dir: layout.path("session-details"),
        };

        Ok(Self { identity, paths })
    }

    pub(super) fn from_project_key(project_key: &str) -> Result<Self> {
        let data_root = resolve_data_root()?;
        let project_dir = data_root.join("listen").join("projects").join(project_key);
        let metadata_path = project_dir.join("project.json");
        let metadata = read_json::<ListenProjectMetadata>(&metadata_path)?;
        let source_label = if metadata.source_label.trim().is_empty() {
            Path::new(&metadata.source_root)
                .file_name()
                .and_then(OsStr::to_str)
                .filter(|value| !value.trim().is_empty())
                .unwrap_or("project")
                .to_string()
        } else {
            metadata.source_label.clone()
        };
        let identity = ListenProjectIdentity {
            project_key: metadata.project_key.clone(),
            source_root: PathBuf::from(&metadata.source_root),
            metastack_root: PathBuf::from(&metadata.metastack_root),
            source_label,
            project_selector: metadata.project_selector.clone(),
            project_label: metadata.project_label.clone(),
        };
        let layout =
            WorkflowRootLayout::install_scoped(project_dir.clone(), "active-listener.lock.json");
        let paths = ListenProjectPaths {
            layout: layout.clone(),
            projects_root: data_root.join("listen").join("projects"),
            project_dir: project_dir.clone(),
            project_metadata_path: metadata_path,
            state_path: layout.path("session.json"),
            lock_path: layout.active_session_path().to_path_buf(),
            logs_dir: layout.path("logs"),
            details_dir: layout.path("session-details"),
        };
        Ok(Self { identity, paths })
    }

    pub(super) fn identity(&self) -> &ListenProjectIdentity {
        &self.identity
    }

    pub(super) fn paths(&self) -> &ListenProjectPaths {
        &self.paths
    }

    pub(super) fn ensure_layout(&self) -> Result<()> {
        ensure_dir(&self.paths.projects_root)?;
        ensure_dir(&self.paths.project_dir)?;
        ensure_dir(&self.paths.logs_dir)?;
        ensure_dir(&self.paths.details_dir)?;
        self.save_metadata()
    }

    pub(super) fn save_metadata(&self) -> Result<()> {
        write_json(
            &self.paths.project_metadata_path,
            &ListenProjectMetadata {
                version: LISTEN_STORE_VERSION,
                project_key: self.identity.project_key.clone(),
                project_selector: self.identity.project_selector.clone(),
                project_label: self.identity.project_label.clone(),
                source_root: self.identity.source_root.display().to_string(),
                metastack_root: self.identity.metastack_root.display().to_string(),
                source_label: self.identity.source_label.clone(),
            },
        )
    }

    pub(super) fn load_state(&self) -> Result<ListenState> {
        let (mut state, state_exists) = self.load_state_from_disk()?;
        let pruned = state.prune_completed_sessions_older_than(
            now_epoch_seconds(),
            COMPLETED_SESSION_TTL_SECONDS,
        );
        if state_exists && !pruned.is_empty() {
            self.save_state(&state)?;
        }
        Ok(state)
    }

    pub(super) fn save_state(&self, state: &ListenState) -> Result<()> {
        self.ensure_layout()?;
        write_json(&self.paths.state_path, state)?;
        self.remove_orphaned_session_details(state)?;
        for session in &state.sessions {
            self.refresh_session_detail(session)?;
        }
        Ok(())
    }

    pub(super) fn upsert_session(&self, session: AgentSession) -> Result<()> {
        let mut state = self.load_state()?;
        state.upsert(session);
        self.save_state(&state)
    }

    pub(super) fn retry_blocked_session(&self, identifier: &str) -> Result<bool> {
        let mut state = self.load_state()?;
        let session = state
            .sessions
            .iter_mut()
            .find(|s| s.issue_matches(identifier) && s.phase == SessionPhase::Blocked);
        let Some(session) = session else {
            return Ok(false);
        };
        session.phase = SessionPhase::BriefReady;
        session.pid = None;
        session.summary = "Retrying from previous workspace state".to_string();
        session.updated_at_epoch_seconds = now_epoch_seconds();
        self.save_state(&state)?;
        Ok(true)
    }

    pub(super) fn pause_running_session(&self, identifier: &str) -> Result<bool> {
        let mut state = self.load_state()?;
        let session = state
            .sessions
            .iter_mut()
            .find(|s| s.issue_matches(identifier) && s.phase == SessionPhase::Running);
        let Some(session) = session else {
            return Ok(false);
        };
        let Some(pid) = session.pid else {
            return Ok(false);
        };
        if !pid_is_running(pid) {
            return Ok(false);
        }
        send_process_signal(pid, ProcessSignal::Pause)?;
        session.phase = SessionPhase::Paused;
        session.summary = compact_session_summary([
            Some("Paused by operator".to_string()),
            Some(format!("pid {pid}")),
            session
                .backlog_issue_identifier
                .as_ref()
                .map(|identifier| format!("backlog {identifier}")),
        ]);
        session.updated_at_epoch_seconds = now_epoch_seconds();
        self.save_state(&state)?;
        Ok(true)
    }

    pub(super) fn resume_paused_session(&self, identifier: &str) -> Result<bool> {
        let mut state = self.load_state()?;
        let session = state
            .sessions
            .iter_mut()
            .find(|s| s.issue_matches(identifier) && s.phase == SessionPhase::Paused);
        let Some(session) = session else {
            return Ok(false);
        };
        let Some(pid) = session.pid else {
            return Ok(false);
        };
        if !pid_is_running(pid) {
            return Ok(false);
        }
        send_process_signal(pid, ProcessSignal::Resume)?;
        session.phase = SessionPhase::Running;
        session.summary = compact_session_summary([
            Some("Resumed by operator".to_string()),
            Some(format!("pid {pid}")),
            session
                .backlog_issue_identifier
                .as_ref()
                .map(|identifier| format!("backlog {identifier}")),
        ]);
        session.updated_at_epoch_seconds = now_epoch_seconds();
        self.save_state(&state)?;
        Ok(true)
    }

    pub(super) fn clear_sessions(&self, selector: &SessionSelector) -> Result<SessionClearOutcome> {
        let mut state = self.load_state()?;
        let live_sessions = state
            .sessions
            .iter()
            .filter(|session| selector.matches(session))
            .filter_map(|session| {
                session
                    .pid
                    .filter(|pid| pid_is_running(*pid))
                    .map(|pid| (session.issue_identifier.clone(), pid))
            })
            .collect::<Vec<_>>();
        if !live_sessions.is_empty() {
            let sessions = live_sessions
                .into_iter()
                .map(|(identifier, pid)| format!("{identifier} (pid {pid})"))
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "cannot clear live MetaListen session record(s) matched by {}: {}",
                selector.display_label(),
                sessions
            );
        }

        let cleared_sessions = state.remove_sessions(|session| selector.matches(session));
        if !cleared_sessions.is_empty() {
            self.save_state(&state)?;
        }

        Ok(SessionClearOutcome {
            cleared_sessions,
            remaining_sessions: state.sessions.len(),
        })
    }

    pub(super) fn log_path(&self, issue_identifier: &str) -> PathBuf {
        self.paths.logs_dir.join(format!("{issue_identifier}.log"))
    }

    pub(super) fn detail_path(&self, issue_identifier: &str) -> PathBuf {
        self.paths
            .details_dir
            .join(format!("{issue_identifier}.json"))
    }

    pub(super) fn load_session_detail(
        &self,
        issue_identifier: &str,
    ) -> Result<Option<ListenSessionDetail>> {
        read_optional_json_lossy(&self.detail_path(issue_identifier))
    }

    pub(super) fn load_session_details(
        &self,
        sessions: &[AgentSession],
    ) -> Result<Vec<ListenSessionDetail>> {
        let mut details = Vec::new();
        for session in sessions {
            if let Some(detail) = self.load_session_detail(&session.issue_identifier)? {
                details.push(detail);
            }
        }
        Ok(details)
    }

    pub(super) fn acquire_listener_lock(&self, pid: u32) -> Result<ListenerLockGuard> {
        self.ensure_layout()?;
        let active_lock_file = self.active_lock_file();

        loop {
            if let Some(existing) = self.load_active_lock()? {
                if pid_is_running(existing.pid) {
                    bail!(
                        "another `meta listen` instance already owns project `{}` (pid {}); active lock: {}",
                        self.identity.project_label,
                        existing.pid,
                        self.paths.lock_path.display()
                    );
                }

                if active_lock_file.remove_if(|lock| lock.pid == existing.pid)? {
                    continue;
                }
                continue;
            }

            let lock = ActiveListenerLock {
                pid,
                acquired_at_epoch_seconds: now_epoch_seconds(),
                source_root: self.identity.source_root.display().to_string(),
                metastack_root: self.identity.metastack_root.display().to_string(),
            };
            match active_lock_file.try_create_new(&lock)? {
                true => {
                    return Ok(ListenerLockGuard {
                        lock_path: self.paths.lock_path.clone(),
                        pid,
                    });
                }
                false => continue,
            }
        }
    }

    pub(super) fn load_active_lock(&self) -> Result<Option<ActiveListenerLock>> {
        self.active_lock_file().load_optional()
    }

    /// Removes the stored session entry, structured detail artifact, and per-ticket log file for
    /// one Linear ticket.
    ///
    /// Returns an error when the persisted state cannot be read or updated, or when the matching
    /// detail/log files cannot be removed.
    pub(crate) fn remove_ticket_artifacts(&self, issue_identifier: &str) -> Result<()> {
        let mut state = self.load_state()?;
        if state.remove_issue(issue_identifier) {
            self.save_state(&state)?;
        }

        let detail_path = self.detail_path(issue_identifier);
        match fs::remove_file(&detail_path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to remove `{}`", detail_path.display()));
            }
        }

        let log_path = self.log_path(issue_identifier);
        match fs::remove_file(&log_path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to remove `{}`", log_path.display()));
            }
        }

        Ok(())
    }

    pub(super) fn list_projects() -> Result<Vec<StoredListenProjectSummary>> {
        let data_root = resolve_data_root()?;
        Self::list_projects_with_data_root(data_root)
    }

    fn list_projects_with_data_root(data_root: PathBuf) -> Result<Vec<StoredListenProjectSummary>> {
        let projects_root = data_root.join("listen").join("projects");
        let mut projects = Vec::new();

        let entries = match fs::read_dir(&projects_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(projects),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read `{}`", projects_root.display()));
            }
        };

        for entry in entries {
            let entry =
                entry.with_context(|| format!("failed to read `{}`", projects_root.display()))?;
            if !entry
                .file_type()
                .with_context(|| format!("failed to inspect `{}`", entry.path().display()))?
                .is_dir()
            {
                continue;
            }

            let project_dir = entry.path();
            let metadata_path = project_dir.join("project.json");
            let state_path = project_dir.join("session.json");
            let lock_path = project_dir.join("active-listener.lock.json");
            let logs_dir = project_dir.join("logs");
            let details_dir = project_dir.join("session-details");
            let metadata = match read_json::<ListenProjectMetadata>(&metadata_path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let store = Self {
                identity: ListenProjectIdentity {
                    project_key: metadata.project_key.clone(),
                    source_root: PathBuf::from(&metadata.source_root),
                    metastack_root: PathBuf::from(&metadata.metastack_root),
                    source_label: metadata.source_label.clone(),
                    project_selector: metadata.project_selector.clone(),
                    project_label: metadata.project_label.clone(),
                },
                paths: ListenProjectPaths {
                    layout: WorkflowRootLayout::install_scoped(
                        project_dir.clone(),
                        "active-listener.lock.json",
                    ),
                    projects_root: projects_root.clone(),
                    project_dir: project_dir.clone(),
                    project_metadata_path: metadata_path.clone(),
                    state_path: state_path.clone(),
                    lock_path: lock_path.clone(),
                    logs_dir: logs_dir.clone(),
                    details_dir,
                },
            };
            let latest_session = match store.load_state() {
                Ok(state) => state.latest_session(),
                Err(_) => None,
            };
            let active_lock = store.load_active_lock().ok().flatten();

            projects.push(StoredListenProjectSummary {
                metadata,
                state_path,
                lock_path,
                logs_dir,
                latest_session,
                active_lock,
            });
        }

        projects.sort_by(|left, right| {
            right
                .latest_session
                .as_ref()
                .map(|session| session.updated_at_epoch_seconds)
                .unwrap_or_default()
                .cmp(
                    &left
                        .latest_session
                        .as_ref()
                        .map(|session| session.updated_at_epoch_seconds)
                        .unwrap_or_default(),
                )
                .then_with(|| {
                    left.metadata
                        .project_label
                        .cmp(&right.metadata.project_label)
                })
        });
        Ok(projects)
    }

    fn load_state_from_disk(&self) -> Result<(ListenState, bool)> {
        match read_json(&self.paths.state_path) {
            Ok(state) => Ok((state, true)),
            Err(error) if is_not_found_error(&error) => Ok((ListenState::default(), false)),
            Err(error) => Err(error),
        }
    }

    fn refresh_session_detail(&self, session: &AgentSession) -> Result<()> {
        let path = self.detail_path(&session.issue_identifier);
        let mut detail = self
            .load_session_detail(&session.issue_identifier)?
            .unwrap_or_else(|| ListenSessionDetail {
                version: LISTEN_SESSION_DETAIL_VERSION,
                issue_identifier: session.issue_identifier.clone(),
                issue_title: session.issue_title.clone(),
                updated_at_epoch_seconds: session.updated_at_epoch_seconds,
                session_updated_at_epoch_seconds: session.updated_at_epoch_seconds,
                phase: session.phase,
                summary: session.summary.clone(),
                turns: session.turns,
                tokens: session.tokens.clone(),
                pull_request: session.pull_request.clone(),
                references: SessionDetailReferences::default(),
                prompt_context: Vec::new(),
                milestones: Vec::new(),
                log_excerpts: Vec::new(),
            });

        detail.version = LISTEN_SESSION_DETAIL_VERSION;
        detail.issue_identifier = session.issue_identifier.clone();
        detail.issue_title = session.issue_title.clone();
        detail.updated_at_epoch_seconds = now_epoch_seconds();
        detail.session_updated_at_epoch_seconds = session.updated_at_epoch_seconds;
        detail.phase = session.phase;
        detail.summary = session.summary.clone();
        detail.turns = session.turns;
        detail.tokens = session.tokens.clone();
        detail.pull_request = session.pull_request.clone();
        detail.references = SessionDetailReferences {
            workspace_path: session.workspace_path.clone(),
            backlog_path: session.backlog_path.clone(),
            brief_path: session.brief_path.clone(),
            workpad_comment_id: session.workpad_comment_id.clone(),
            log_path: session.log_path.clone(),
            branch: session.branch.clone(),
        };
        detail.prompt_context = build_prompt_context_references(session);
        detail.log_excerpts = read_log_excerpts(session.log_path.as_deref())?;
        append_milestone(&mut detail.milestones, session);

        write_json(&path, &detail)
    }

    fn remove_orphaned_session_details(&self, state: &ListenState) -> Result<()> {
        let valid = state
            .sessions
            .iter()
            .map(|session| session.issue_identifier.to_ascii_lowercase())
            .collect::<std::collections::BTreeSet<_>>();
        let entries = match fs::read_dir(&self.paths.details_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to read `{}`", self.paths.details_dir.display())
                });
            }
        };

        for entry in entries {
            let entry = entry.with_context(|| {
                format!("failed to read `{}`", self.paths.details_dir.display())
            })?;
            if !entry
                .file_type()
                .with_context(|| format!("failed to inspect `{}`", entry.path().display()))?
                .is_file()
            {
                continue;
            }
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(OsStr::to_str) else {
                continue;
            };
            if valid.contains(&stem.to_ascii_lowercase()) {
                continue;
            }
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove `{}`", path.display()))?;
        }

        Ok(())
    }

    fn active_lock_file(&self) -> ActiveSessionFile<ActiveListenerLock> {
        self.paths.layout.active_session_file()
    }
}

impl Drop for ListenerLockGuard {
    fn drop(&mut self) {
        let Ok(contents) = fs::read_to_string(&self.lock_path) else {
            return;
        };
        let Ok(lock) = serde_json::from_str::<ActiveListenerLock>(&contents) else {
            return;
        };
        if lock.pid == self.pid {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}

fn resolve_project_identity(
    root: &Path,
    project_selector: Option<&str>,
) -> Result<ListenProjectIdentity> {
    let requested_root = canonicalize_existing_dir(root)?;
    let source_root = resolve_source_root(&requested_root)?;
    let metastack_root = canonicalize_existing_dir(&source_root.join(".metastack"))?;
    let source_label = source_root
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("project")
        .to_string();
    let project_selector = normalize_project_selector(project_selector);
    let project_label = project_selector
        .clone()
        .unwrap_or_else(|| "All projects".to_string());

    Ok(ListenProjectIdentity {
        project_key: project_key_for_metastack_root(&metastack_root, project_selector.as_deref()),
        source_root,
        metastack_root,
        source_label,
        project_selector,
        project_label,
    })
}

/// Resolves the source repository root for a requested path, collapsing git worktrees back to the
/// owning repository when the shared `.metastack/` directory lives there.
///
/// Returns an error when the requested path cannot be resolved.
pub(crate) fn resolve_source_project_root(root: &Path) -> Result<PathBuf> {
    let requested_root = canonicalize_existing_dir(root)?;
    resolve_source_root(&requested_root)
}

fn resolve_source_root(root: &Path) -> Result<PathBuf> {
    let common_dir = git_stdout(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    if let Ok(common_dir) = common_dir {
        let common_dir = PathBuf::from(common_dir);
        if common_dir.file_name() == Some(OsStr::new(".git"))
            && let Some(source_root) = common_dir.parent()
            && source_root.join(".metastack").is_dir()
        {
            return canonicalize_existing_dir(source_root);
        }
    }

    Ok(root.to_path_buf())
}

fn project_key_for_metastack_root(metastack_root: &Path, project_selector: Option<&str>) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    metastack_root.display().to_string().hash(&mut hasher);
    normalized_project_scope_key(project_selector).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn normalize_project_selector(project_selector: Option<&str>) -> Option<String> {
    project_selector
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalized_project_scope_key(project_selector: Option<&str>) -> String {
    match normalize_project_selector(project_selector) {
        Some(selector) => format!("project:{}", selector.to_ascii_lowercase()),
        None => "project:all".to_string(),
    }
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

fn is_not_found_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
        .any(|io_error| io_error.kind() == ErrorKind::NotFound)
}

fn append_milestone(milestones: &mut Vec<SessionMilestone>, session: &AgentSession) {
    let candidate = SessionMilestone {
        at_epoch_seconds: session.updated_at_epoch_seconds,
        phase: session.phase,
        summary: session.summary.clone(),
        turns: session.turns,
        pull_request_status: session.pull_request.status,
        pull_request_number: session.pull_request.number,
    };
    if milestones.last().is_some_and(|last| {
        last.phase == candidate.phase
            && last.summary == candidate.summary
            && last.turns == candidate.turns
            && last.pull_request_status == candidate.pull_request_status
            && last.pull_request_number == candidate.pull_request_number
    }) {
        return;
    }
    milestones.push(candidate);
}

fn build_prompt_context_references(session: &AgentSession) -> Vec<SessionContextReference> {
    let mut references = Vec::new();

    if let Some(path) = session.brief_path.as_ref() {
        references.push(SessionContextReference {
            label: "Brief".to_string(),
            value: path.clone(),
        });
    }
    if let Some(path) = session.backlog_path.as_ref() {
        references.push(SessionContextReference {
            label: "Backlog".to_string(),
            value: path.clone(),
        });
    }
    if let Some(workspace_path) = session.workspace_path.as_deref() {
        let paths = PlanningPaths::new(Path::new(workspace_path));
        let issue_identifier = session
            .backlog_issue_identifier
            .as_deref()
            .unwrap_or(&session.issue_identifier);
        let backlog_index = paths.backlog_issue_dir(issue_identifier).join("index.md");
        if backlog_index.is_file() {
            references.push(SessionContextReference {
                label: "Backlog index".to_string(),
                value: backlog_index.display().to_string(),
            });
        }

        let manifest_path = paths.agent_issue_context_manifest_path(&session.issue_identifier);
        if manifest_path.is_file() {
            references.push(SessionContextReference {
                label: "Attachment context manifest".to_string(),
                value: manifest_path.display().to_string(),
            });
        }
    }

    references
}

fn read_log_excerpts(log_path: Option<&str>) -> Result<Vec<SessionLogExcerpt>> {
    let Some(log_path) = log_path else {
        return Ok(Vec::new());
    };
    let path = Path::new(log_path);
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read `{}`", path.display()));
        }
    };

    let lines = contents
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            summarize_log_line(line).map(|text| SessionLogExcerpt {
                line_number: index + 1,
                text,
            })
        })
        .collect::<Vec<_>>();
    let start = lines.len().saturating_sub(LOG_EXCERPT_LIMIT);
    Ok(lines.into_iter().skip(start).collect())
}

fn summarize_log_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(message) = value.get("message").and_then(Value::as_str) {
            return Some(truncate_log_excerpt(message));
        }
        if let Some(message) = value.get("msg").and_then(Value::as_str) {
            return Some(truncate_log_excerpt(message));
        }
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return Some(truncate_log_excerpt(&format!("error: {error}")));
        }
        if let Some(kind) = value.get("type").and_then(Value::as_str) {
            let detail = value
                .get("subtype")
                .and_then(Value::as_str)
                .or_else(|| value.get("event").and_then(Value::as_str))
                .or_else(|| value.get("thread_id").and_then(Value::as_str))
                .or_else(|| value.get("session_id").and_then(Value::as_str))
                .unwrap_or_default();
            let summary = if detail.is_empty() {
                kind.to_string()
            } else {
                format!("{kind}: {detail}")
            };
            return Some(truncate_log_excerpt(&summary));
        }
    }

    Some(truncate_log_excerpt(trimmed))
}

fn truncate_log_excerpt(value: &str) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let truncated = chars
        .by_ref()
        .take(LOG_EXCERPT_MAX_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{}...", truncated.trim_end())
    } else {
        collapsed
    }
}

pub(super) fn pid_is_running(pid: u32) -> bool {
    Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(true)
}

#[derive(Debug, Clone, Copy)]
enum ProcessSignal {
    Pause,
    Resume,
}

#[cfg(unix)]
fn send_process_signal(pid: u32, signal: ProcessSignal) -> Result<()> {
    let signal_arg = match signal {
        ProcessSignal::Pause => "-STOP",
        ProcessSignal::Resume => "-CONT",
    };
    let status = Command::new("kill")
        .arg(signal_arg)
        .arg(pid.to_string())
        .status()
        .with_context(|| format!("failed to run `kill {signal_arg} {pid}`"))?;
    if !status.success() {
        bail!("`kill {signal_arg} {pid}` exited with status {status}");
    }
    Ok(())
}

#[cfg(not(unix))]
fn send_process_signal(_pid: u32, signal: ProcessSignal) -> Result<()> {
    match signal {
        ProcessSignal::Pause => bail!("listen pause is only supported on Unix hosts"),
        ProcessSignal::Resume => bail!("listen resume is only supported on Unix hosts"),
    }
}

fn now_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::{Child, Command, Stdio};

    use anyhow::{Context, Result};
    use tempfile::tempdir;

    use crate::config::data_root_from_config_path;
    use crate::listen::{ListenSessionDetail, PullRequestSummary, TokenUsage};

    use super::{
        ActiveListenerLock, AgentSession, COMPLETED_SESSION_TTL_SECONDS,
        LISTEN_SESSION_DETAIL_VERSION, ListenProjectStore, ListenState, SessionDetailReferences,
        SessionPhase, SessionSelector, project_key_for_metastack_root, resolve_source_root,
        write_json,
    };

    #[test]
    fn project_store_uses_git_common_dir_source_root_for_worktrees() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let worktree_root = temp.path().join("repo-worktree");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        std::process::Command::new("git")
            .args(["init", "-b", "main", repo_root.to_string_lossy().as_ref()])
            .status()?;
        std::process::Command::new("git")
            .args([
                "-C",
                repo_root.to_string_lossy().as_ref(),
                "config",
                "user.email",
                "test@example.com",
            ])
            .status()?;
        std::process::Command::new("git")
            .args([
                "-C",
                repo_root.to_string_lossy().as_ref(),
                "config",
                "user.name",
                "Meta Test",
            ])
            .status()?;
        fs::write(repo_root.join("README.md"), "repo\n")?;
        std::process::Command::new("git")
            .args(["-C", repo_root.to_string_lossy().as_ref(), "add", "."])
            .status()?;
        std::process::Command::new("git")
            .args([
                "-C",
                repo_root.to_string_lossy().as_ref(),
                "commit",
                "-m",
                "init",
            ])
            .status()?;
        std::process::Command::new("git")
            .args([
                "-C",
                repo_root.to_string_lossy().as_ref(),
                "worktree",
                "add",
                "-b",
                "feature/test",
                worktree_root.to_string_lossy().as_ref(),
                "main",
            ])
            .status()?;

        let source_root = resolve_source_root(&worktree_root)?;
        assert_eq!(source_root.canonicalize()?, repo_root.canonicalize()?);

        Ok(())
    }

    #[test]
    fn project_key_hash_is_stable_for_same_metastack_root() -> Result<()> {
        let temp = tempdir()?;
        let metastack_root = temp.path().join("repo").join(".metastack");
        fs::create_dir_all(&metastack_root)?;

        assert_eq!(
            project_key_for_metastack_root(&metastack_root, Some("MetaStack CLI")),
            project_key_for_metastack_root(&metastack_root, Some("metastack cli"))
        );
        assert_ne!(
            project_key_for_metastack_root(&metastack_root, Some("MetaStack CLI")),
            project_key_for_metastack_root(&metastack_root, Some("MetaStack API"))
        );
        assert_ne!(
            project_key_for_metastack_root(&metastack_root, Some("MetaStack CLI")),
            project_key_for_metastack_root(&metastack_root, None)
        );

        Ok(())
    }

    #[test]
    fn project_store_paths_use_global_data_root() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let config_path = temp.path().join("metastack.toml");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let data_root = data_root_from_config_path(&config_path)?;
        let store = ListenProjectStore::resolve_with_data_root(
            &repo_root,
            data_root.clone(),
            Some("MetaStack CLI"),
        )?;
        assert!(data_root.starts_with(temp.path()));
        assert!(store.paths().state_path.starts_with(data_root));
        Ok(())
    }

    fn default_session(
        issue_identifier: &str,
        phase: SessionPhase,
        updated_at: u64,
    ) -> AgentSession {
        AgentSession {
            issue_id: Some(format!("{issue_identifier}-id")),
            issue_identifier: issue_identifier.to_string(),
            issue_title: format!("{issue_identifier} title"),
            project_name: Some("MetaStack CLI".to_string()),
            team_key: "MET".to_string(),
            issue_url: format!("https://linear.app/metastack/{issue_identifier}"),
            phase,
            summary: format!("{issue_identifier} summary"),
            brief_path: Some(format!(".metastack/agents/briefs/{issue_identifier}.md")),
            backlog_issue_identifier: Some(format!("TECH-{issue_identifier}")),
            backlog_issue_title: Some(format!("Backlog for {issue_identifier}")),
            backlog_path: Some(format!(".metastack/backlog/{issue_identifier}")),
            workspace_path: Some(format!("/tmp/{issue_identifier}")),
            branch: Some(format!("branch-{issue_identifier}")),
            pull_request: Default::default(),
            workpad_comment_id: Some(format!("workpad-{issue_identifier}")),
            updated_at_epoch_seconds: updated_at,
            pid: None,
            session_id: Some(format!("session-{issue_identifier}")),
            latest_resume_handle: None,
            turns: Some(1),
            tokens: TokenUsage::default(),
            log_path: Some(format!("logs/{issue_identifier}.log")),
        }
    }

    fn seed_state(store: &ListenProjectStore, sessions: Vec<AgentSession>) -> Result<()> {
        store.ensure_layout()?;
        write_json(
            &store.paths().state_path,
            &ListenState::from_sessions(sessions),
        )
    }

    fn spawn_sleep_process() -> Result<Child> {
        Command::new("sleep")
            .arg("30")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("failed to spawn sleep process for listen store test")
    }

    #[test]
    fn clear_by_issue_identifier_preserves_other_sessions_and_project_files() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        store.ensure_layout()?;
        fs::write(store.paths().state_path.clone(), "{}")?;
        write_json(
            &store.paths().lock_path,
            &ActiveListenerLock {
                pid: 99_999,
                acquired_at_epoch_seconds: 0,
                source_root: repo_root.display().to_string(),
                metastack_root: repo_root.join(".metastack").display().to_string(),
            },
        )?;
        seed_state(
            &store,
            vec![
                default_session("ENG-10163", SessionPhase::Blocked, 100),
                default_session("ENG-10164", SessionPhase::Blocked, 200),
            ],
        )?;

        let outcome =
            store.clear_sessions(&SessionSelector::IssueIdentifier("ENG-10163".to_string()))?;
        let state = store.load_state()?;

        assert_eq!(outcome.cleared_sessions.len(), 1);
        assert_eq!(outcome.cleared_sessions[0].issue_identifier, "ENG-10163");
        assert_eq!(outcome.remaining_sessions, 1);
        assert!(store.paths().project_dir.is_dir());
        assert!(store.paths().project_metadata_path.is_file());
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].issue_identifier, "ENG-10164");
        assert!(
            store
                .paths()
                .lock_path
                .parent()
                .is_some_and(|parent| parent.is_dir())
        );
        Ok(())
    }

    #[test]
    fn clear_stale_removes_only_dead_pid_sessions() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;

        let mut stale = default_session("ENG-10163", SessionPhase::Blocked, 100);
        stale.pid = Some(99_999);
        let mut running = default_session("ENG-10164", SessionPhase::Running, 200);
        let mut child = spawn_sleep_process()?;
        running.pid = Some(child.id());
        seed_state(&store, vec![stale, running.clone()])?;

        let outcome = store.clear_sessions(&SessionSelector::Stale)?;
        let state = store.load_state()?;

        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(outcome.cleared_sessions.len(), 1);
        assert_eq!(outcome.cleared_sessions[0].issue_identifier, "ENG-10163");
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].issue_identifier, "ENG-10164");
        Ok(())
    }

    #[test]
    fn clear_refuses_live_targeted_sessions() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;

        let mut live = default_session("ENG-10163", SessionPhase::Running, 100);
        let mut child = spawn_sleep_process()?;
        live.pid = Some(child.id());
        seed_state(&store, vec![live])?;

        let error = store
            .clear_sessions(&SessionSelector::All)
            .expect_err("live session clear should fail");

        let _ = child.kill();
        let _ = child.wait();

        assert!(
            error
                .to_string()
                .contains("cannot clear live MetaListen session record(s)")
        );
        assert_eq!(store.load_state()?.sessions.len(), 1);
        Ok(())
    }

    #[test]
    fn load_state_prunes_only_completed_sessions_older_than_ttl() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        let now = super::now_epoch_seconds();
        let ttl = COMPLETED_SESSION_TTL_SECONDS;

        seed_state(
            &store,
            vec![
                default_session("ENG-10163", SessionPhase::Completed, now - ttl - 1),
                default_session("ENG-10164", SessionPhase::Completed, now - ttl + 5),
                default_session("ENG-10165", SessionPhase::Blocked, now - ttl - 1),
            ],
        )?;

        let state = store.load_state()?;

        assert_eq!(state.sessions.len(), 2);
        assert!(
            state
                .sessions
                .iter()
                .any(|session| session.issue_identifier == "ENG-10164")
        );
        assert!(
            state
                .sessions
                .iter()
                .any(|session| session.issue_identifier == "ENG-10165")
        );
        assert!(
            !state
                .sessions
                .iter()
                .any(|session| session.issue_identifier == "ENG-10163")
        );
        Ok(())
    }

    #[test]
    fn remove_ticket_artifacts_cleans_detail_and_log_files() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        store.ensure_layout()?;

        let issue_identifier = "ENG-10163";
        let mut session = default_session(issue_identifier, SessionPhase::Running, 100);
        session.log_path = Some(store.log_path(issue_identifier).display().to_string());
        fs::write(store.log_path(issue_identifier), "worker log line\n")
            .context("failed to seed session log for listen store test")?;
        store.save_state(&ListenState::from_sessions(vec![session]))?;

        assert!(store.detail_path(issue_identifier).is_file());
        assert!(store.log_path(issue_identifier).is_file());

        store.remove_ticket_artifacts(issue_identifier)?;

        assert!(store.load_state()?.sessions.is_empty());
        assert!(!store.detail_path(issue_identifier).exists());
        assert!(!store.log_path(issue_identifier).exists());
        Ok(())
    }

    #[test]
    fn remove_ticket_artifacts_cleans_orphaned_detail_without_session_state() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        store.ensure_layout()?;

        let issue_identifier = "ENG-10163";
        fs::write(
            store.detail_path(issue_identifier),
            serde_json::to_vec_pretty(&ListenSessionDetail {
                version: LISTEN_SESSION_DETAIL_VERSION,
                issue_identifier: issue_identifier.to_string(),
                issue_title: "orphan detail".to_string(),
                updated_at_epoch_seconds: 100,
                session_updated_at_epoch_seconds: 100,
                phase: SessionPhase::Completed,
                summary: "detail without state".to_string(),
                turns: Some(1),
                tokens: TokenUsage::default(),
                pull_request: PullRequestSummary::default(),
                references: SessionDetailReferences::default(),
                prompt_context: Vec::new(),
                milestones: Vec::new(),
                log_excerpts: Vec::new(),
            })?,
        )
        .context("failed to seed orphaned detail artifact for listen store test")?;
        fs::write(store.log_path(issue_identifier), "worker log line\n")
            .context("failed to seed orphaned log file for listen store test")?;

        assert!(store.load_state()?.sessions.is_empty());
        assert!(store.detail_path(issue_identifier).is_file());
        assert!(store.log_path(issue_identifier).is_file());

        store.remove_ticket_artifacts(issue_identifier)?;

        assert!(store.load_state()?.sessions.is_empty());
        assert!(!store.detail_path(issue_identifier).exists());
        assert!(!store.log_path(issue_identifier).exists());
        Ok(())
    }

    #[test]
    fn invalid_session_detail_is_treated_as_unavailable_and_rewritten() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        store.ensure_layout()?;

        let issue_identifier = "ENG-10163";
        fs::write(store.detail_path(issue_identifier), "{ not valid json")
            .context("failed to seed invalid detail artifact for listen store test")?;

        assert!(store.load_session_detail(issue_identifier)?.is_none());

        let mut session = default_session(issue_identifier, SessionPhase::Running, 100);
        session.log_path = Some(store.log_path(issue_identifier).display().to_string());
        store.save_state(&ListenState::from_sessions(vec![session]))?;

        let detail = store
            .load_session_detail(issue_identifier)?
            .context("expected save_state to rewrite the invalid detail artifact")?;
        assert_eq!(detail.issue_identifier, issue_identifier);
        assert_eq!(detail.summary, format!("{issue_identifier} summary"));
        Ok(())
    }

    #[test]
    fn list_projects_uses_pruned_state_for_latest_session() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        let now = super::now_epoch_seconds();

        seed_state(
            &store,
            vec![
                default_session(
                    "ENG-10163",
                    SessionPhase::Completed,
                    now - COMPLETED_SESSION_TTL_SECONDS - 1,
                ),
                default_session("ENG-10164", SessionPhase::Blocked, now),
            ],
        )?;

        let projects = ListenProjectStore::list_projects_with_data_root(temp.path().join("data"))?;
        let summary = projects
            .into_iter()
            .find(|project| project.metadata.project_key == store.identity().project_key)
            .expect("project summary should exist");

        assert_eq!(
            summary
                .latest_session
                .as_ref()
                .map(|session| session.issue_identifier.as_str()),
            Some("ENG-10164")
        );
        Ok(())
    }

    #[test]
    fn retry_blocked_session_resets_to_brief_ready() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        let now = super::now_epoch_seconds();

        seed_state(
            &store,
            vec![
                default_session("ENG-100", SessionPhase::Blocked, now),
                default_session("ENG-200", SessionPhase::Running, now),
            ],
        )?;

        assert!(store.retry_blocked_session("ENG-100")?);

        let state = store.load_state()?;
        let retried = state
            .sessions
            .iter()
            .find(|s| s.issue_identifier == "ENG-100")
            .expect("session should exist");
        assert_eq!(retried.phase, SessionPhase::BriefReady);
        assert!(retried.pid.is_none());
        assert_eq!(retried.summary, "Retrying from previous workspace state");

        let other = state
            .sessions
            .iter()
            .find(|s| s.issue_identifier == "ENG-200")
            .expect("other session should be untouched");
        assert_eq!(other.phase, SessionPhase::Running);

        Ok(())
    }

    #[test]
    fn retry_blocked_session_returns_false_for_non_blocked() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        let now = super::now_epoch_seconds();

        seed_state(
            &store,
            vec![default_session("ENG-300", SessionPhase::Running, now)],
        )?;

        assert!(!store.retry_blocked_session("ENG-300")?);
        assert!(!store.retry_blocked_session("ENG-999")?);

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn pause_running_session_marks_session_paused_and_keeps_pid() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        let now = super::now_epoch_seconds();
        let mut child = spawn_sleep_process()?;
        let pid = child.id();

        let mut session = default_session("ENG-400", SessionPhase::Running, now);
        session.pid = Some(pid);
        seed_state(&store, vec![session])?;

        assert!(store.pause_running_session("ENG-400")?);

        let state = store.load_state()?;
        let paused = state
            .sessions
            .iter()
            .find(|s| s.issue_identifier == "ENG-400")
            .expect("session should exist");
        assert_eq!(paused.phase, SessionPhase::Paused);
        assert_eq!(paused.pid, Some(pid));
        assert!(paused.summary.contains("Paused by operator"));
        assert!(super::pid_is_running(pid));

        let _ = child.kill();
        let _ = child.wait();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn resume_paused_session_marks_session_running_without_changing_pid() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        let now = super::now_epoch_seconds();
        let mut child = spawn_sleep_process()?;
        let pid = child.id();

        let mut session = default_session("ENG-401", SessionPhase::Running, now);
        session.pid = Some(pid);
        seed_state(&store, vec![session])?;
        assert!(store.pause_running_session("ENG-401")?);

        assert!(store.resume_paused_session("ENG-401")?);

        let state = store.load_state()?;
        let resumed = state
            .sessions
            .iter()
            .find(|s| s.issue_identifier == "ENG-401")
            .expect("session should exist");
        assert_eq!(resumed.phase, SessionPhase::Running);
        assert_eq!(resumed.pid, Some(pid));
        assert!(resumed.summary.contains("Resumed by operator"));
        assert!(super::pid_is_running(pid));

        let _ = child.kill();
        let _ = child.wait();
        Ok(())
    }

    #[test]
    fn pause_and_resume_return_false_when_session_cannot_transition() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root, None)?;
        let now = super::now_epoch_seconds();

        seed_state(
            &store,
            vec![
                default_session("ENG-500", SessionPhase::Blocked, now),
                default_session("ENG-501", SessionPhase::Paused, now),
                default_session("ENG-502", SessionPhase::Running, now),
            ],
        )?;

        assert!(!store.pause_running_session("ENG-500")?);
        assert!(!store.pause_running_session("ENG-502")?);
        assert!(!store.resume_paused_session("ENG-500")?);
        assert!(!store.resume_paused_session("ENG-501")?);
        assert!(!store.pause_running_session("ENG-999")?);
        assert!(!store.resume_paused_session("ENG-999")?);

        Ok(())
    }
}

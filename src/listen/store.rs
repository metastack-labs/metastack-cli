use std::ffi::OsStr;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::resolve_data_root;
use crate::fs::{canonicalize_existing_dir, ensure_dir};

use super::state::{AgentSession, COMPLETED_SESSION_TTL_SECONDS, ListenState, SessionPhase};

const LISTEN_STORE_VERSION: u8 = 1;

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
    pub(super) projects_root: PathBuf,
    pub(super) project_dir: PathBuf,
    pub(super) project_metadata_path: PathBuf,
    pub(super) state_path: PathBuf,
    pub(super) lock_path: PathBuf,
    pub(super) logs_dir: PathBuf,
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
        let paths = ListenProjectPaths {
            projects_root,
            project_dir: project_dir.clone(),
            project_metadata_path: project_dir.join("project.json"),
            state_path: project_dir.join("session.json"),
            lock_path: project_dir.join("active-listener.lock.json"),
            logs_dir: project_dir.join("logs"),
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
        let paths = ListenProjectPaths {
            projects_root: data_root.join("listen").join("projects"),
            project_dir: project_dir.clone(),
            project_metadata_path: metadata_path,
            state_path: project_dir.join("session.json"),
            lock_path: project_dir.join("active-listener.lock.json"),
            logs_dir: project_dir.join("logs"),
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
        write_json(&self.paths.state_path, state)
    }

    pub(super) fn upsert_session(&self, session: AgentSession) -> Result<()> {
        let mut state = self.load_state()?;
        state.upsert(session);
        self.save_state(&state)
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

    pub(super) fn acquire_listener_lock(&self, pid: u32) -> Result<ListenerLockGuard> {
        self.ensure_layout()?;

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

                match fs::remove_file(&self.paths.lock_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "failed to remove stale `{}`",
                                self.paths.lock_path.display()
                            )
                        });
                    }
                }
                continue;
            }

            let lock = ActiveListenerLock {
                pid,
                acquired_at_epoch_seconds: now_epoch_seconds(),
                source_root: self.identity.source_root.display().to_string(),
                metastack_root: self.identity.metastack_root.display().to_string(),
            };
            let contents = serde_json::to_vec_pretty(&lock)
                .context("failed to serialize the active listen lock")?;
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.paths.lock_path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(&contents).with_context(|| {
                        format!("failed to write `{}`", self.paths.lock_path.display())
                    })?;
                    return Ok(ListenerLockGuard {
                        lock_path: self.paths.lock_path.clone(),
                        pid,
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to create `{}`", self.paths.lock_path.display())
                    });
                }
            }
        }
    }

    pub(super) fn load_active_lock(&self) -> Result<Option<ActiveListenerLock>> {
        match fs::read_to_string(&self.paths.lock_path) {
            Ok(contents) => serde_json::from_str(&contents)
                .map(Some)
                .with_context(|| format!("failed to decode `{}`", self.paths.lock_path.display())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("failed to read `{}`", self.paths.lock_path.display())),
        }
    }

    /// Removes the stored session entry and per-ticket log file for one Linear ticket.
    ///
    /// Returns an error when the persisted state cannot be read or updated, or when the matching
    /// log file cannot be removed.
    pub(crate) fn remove_ticket_artifacts(&self, issue_identifier: &str) -> Result<()> {
        let mut state = self.load_state()?;
        if state.remove_issue(issue_identifier) {
            self.save_state(&state)?;
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
                    projects_root: projects_root.clone(),
                    project_dir: project_dir.clone(),
                    project_metadata_path: metadata_path.clone(),
                    state_path: state_path.clone(),
                    lock_path: lock_path.clone(),
                    logs_dir: logs_dir.clone(),
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
        match fs::read_to_string(&self.paths.state_path) {
            Ok(contents) => serde_json::from_str(&contents)
                .map(|state| (state, true))
                .with_context(|| format!("failed to decode `{}`", self.paths.state_path.display())),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                Ok((ListenState::default(), false))
            }
            Err(error) => Err(error)
                .with_context(|| format!("failed to read `{}`", self.paths.state_path.display())),
        }
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

fn write_json<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let contents = serde_json::to_string_pretty(value)
        .with_context(|| format!("failed to serialize `{}`", path.display()))?;
    fs::write(path, contents).with_context(|| format!("failed to write `{}`", path.display()))
}

fn read_json<T>(path: &Path) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    serde_json::from_str(&contents)
        .with_context(|| format!("failed to decode `{}`", path.display()))
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
    use crate::listen::TokenUsage;

    use super::{
        ActiveListenerLock, AgentSession, COMPLETED_SESSION_TTL_SECONDS, ListenProjectStore,
        ListenState, SessionPhase, SessionSelector, project_key_for_metastack_root,
        resolve_source_root, write_json,
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
            workpad_comment_id: Some(format!("workpad-{issue_identifier}")),
            updated_at_epoch_seconds: updated_at,
            pid: None,
            session_id: Some(format!("session-{issue_identifier}")),
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
                default_session("ENG-10164", SessionPhase::Completed, now - ttl),
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
}

use std::ffi::OsStr;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::resolve_data_root;
use crate::fs::{canonicalize_existing_dir, ensure_dir};

use super::state::ListenState;

const LISTEN_STORE_VERSION: u8 = 1;

#[derive(Debug, Clone)]
pub(super) struct ListenProjectStore {
    identity: ListenProjectIdentity,
    paths: ListenProjectPaths,
}

#[derive(Debug, Clone)]
pub(super) struct ListenProjectIdentity {
    pub(super) project_key: String,
    pub(super) source_root: PathBuf,
    pub(super) metastack_root: PathBuf,
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
    pub(super) project_label: String,
    pub(super) source_root: String,
    pub(super) metastack_root: String,
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
    pub(super) latest_session: Option<super::state::AgentSession>,
    pub(super) active_lock: Option<ActiveListenerLock>,
}

#[derive(Debug)]
pub(super) struct ListenerLockGuard {
    lock_path: PathBuf,
    pid: u32,
}

impl ListenProjectStore {
    pub(super) fn resolve(root: &Path) -> Result<Self> {
        let data_root = resolve_data_root()?;
        Self::resolve_with_data_root(root, data_root)
    }

    fn resolve_with_data_root(root: &Path, data_root: PathBuf) -> Result<Self> {
        let identity = resolve_project_identity(root)?;
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
        let identity = ListenProjectIdentity {
            project_key: metadata.project_key.clone(),
            source_root: PathBuf::from(&metadata.source_root),
            metastack_root: PathBuf::from(&metadata.metastack_root),
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
                project_label: self.identity.project_label.clone(),
                source_root: self.identity.source_root.display().to_string(),
                metastack_root: self.identity.metastack_root.display().to_string(),
            },
        )
    }

    pub(super) fn load_state(&self) -> Result<ListenState> {
        match fs::read_to_string(&self.paths.state_path) {
            Ok(contents) => serde_json::from_str(&contents)
                .with_context(|| format!("failed to decode `{}`", self.paths.state_path.display())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(ListenState::default()),
            Err(error) => Err(error)
                .with_context(|| format!("failed to read `{}`", self.paths.state_path.display())),
        }
    }

    pub(super) fn save_state(&self, state: &ListenState) -> Result<()> {
        self.ensure_layout()?;
        write_json(&self.paths.state_path, state)
    }

    pub(super) fn upsert_session(&self, session: super::state::AgentSession) -> Result<()> {
        let mut state = self.load_state()?;
        state.upsert(session);
        self.save_state(&state)
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

    pub(super) fn clear(&self) -> Result<()> {
        if let Some(active_lock) = self.load_active_lock()?
            && pid_is_running(active_lock.pid)
        {
            bail!(
                "cannot clear project `{}` while active listener pid {} still owns it",
                self.identity.project_label,
                active_lock.pid
            );
        }

        for attempt in 0..5 {
            match fs::remove_dir_all(&self.paths.project_dir) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
                Err(error) if is_directory_not_empty(&error) && attempt < 4 => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to remove `{}`", self.paths.project_dir.display())
                    });
                }
            }
        }

        Ok(())
    }

    pub(super) fn list_projects() -> Result<Vec<StoredListenProjectSummary>> {
        let data_root = resolve_data_root()?;
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
            let latest_session = match fs::read_to_string(&state_path) {
                Ok(contents) => serde_json::from_str::<ListenState>(&contents)
                    .ok()
                    .and_then(|state| state.latest_session()),
                Err(error) if error.kind() == ErrorKind::NotFound => None,
                Err(_) => None,
            };
            let active_lock = match fs::read_to_string(&lock_path) {
                Ok(contents) => serde_json::from_str::<ActiveListenerLock>(&contents).ok(),
                Err(error) if error.kind() == ErrorKind::NotFound => None,
                Err(_) => None,
            };

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

fn resolve_project_identity(root: &Path) -> Result<ListenProjectIdentity> {
    let requested_root = canonicalize_existing_dir(root)?;
    let source_root = resolve_source_root(&requested_root)?;
    let metastack_root = canonicalize_existing_dir(&source_root.join(".metastack"))?;
    let project_label = source_root
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("project")
        .to_string();

    Ok(ListenProjectIdentity {
        project_key: project_key_for_metastack_root(&metastack_root),
        source_root,
        metastack_root,
        project_label,
    })
}

pub(super) fn resolve_source_project_root(root: &Path) -> Result<PathBuf> {
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

fn project_key_for_metastack_root(metastack_root: &Path) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    metastack_root.display().to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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

fn pid_is_running(pid: u32) -> bool {
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

fn is_directory_not_empty(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::DirectoryNotEmpty | ErrorKind::Other
    ) && error
        .raw_os_error()
        .is_some_and(|code| matches!(code, 39 | 66))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use tempfile::tempdir;

    use crate::config::data_root_from_config_path;

    use super::{
        ActiveListenerLock, ListenProjectStore, project_key_for_metastack_root,
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
            project_key_for_metastack_root(&metastack_root),
            project_key_for_metastack_root(&metastack_root)
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
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root.clone())?;
        assert!(data_root.starts_with(temp.path()));
        assert!(store.paths().state_path.starts_with(data_root));
        Ok(())
    }

    #[test]
    fn clear_removes_project_dir_when_only_a_stale_lock_remains() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let data_root = temp.path().join("data");
        fs::create_dir_all(repo_root.join(".metastack"))?;
        let store = ListenProjectStore::resolve_with_data_root(&repo_root, data_root)?;
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

        store.clear()?;

        assert!(!store.paths().project_dir.exists());
        Ok(())
    }
}

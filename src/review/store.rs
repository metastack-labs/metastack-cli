use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::resolve_data_root;
use crate::fs::ensure_dir;

use super::state::{COMPLETED_SESSION_TTL_SECONDS, ReviewState};

const REVIEW_STORE_VERSION: u8 = 1;

/// Install-scoped store for review listener state.
#[derive(Debug, Clone)]
pub(crate) struct ReviewProjectStore {
    identity: ReviewProjectIdentity,
    paths: ReviewProjectPaths,
}

#[derive(Debug, Clone)]
pub(super) struct ReviewProjectIdentity {
    pub(super) project_key: String,
    pub(super) source_root: PathBuf,
    pub(super) source_label: String,
}

#[derive(Debug, Clone)]
pub(super) struct ReviewProjectPaths {
    pub(super) project_dir: PathBuf,
    pub(super) project_metadata_path: PathBuf,
    pub(super) state_path: PathBuf,
    pub(super) lock_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReviewProjectMetadata {
    version: u8,
    project_key: String,
    source_root: String,
    source_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ActiveReviewLock {
    pub(super) pid: u32,
    pub(super) acquired_at_epoch_seconds: u64,
    pub(super) source_root: String,
}

#[derive(Debug)]
pub(super) struct ReviewLockGuard {
    lock_path: PathBuf,
    pid: u32,
}

impl ReviewProjectStore {
    /// Resolve the install-scoped review store for the repository at `root`.
    ///
    /// Returns an error when the repository root or data root cannot be resolved.
    pub(crate) fn resolve(root: &Path) -> Result<Self> {
        let data_root = resolve_data_root()?;
        let source_root = crate::fs::canonicalize_existing_dir(root)?;
        let source_label = source_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("project")
            .to_string();
        let project_key = resolve_project_key(&source_root);
        let project_dir = data_root.join("review").join("projects").join(&project_key);
        let paths = ReviewProjectPaths {
            project_dir: project_dir.clone(),
            project_metadata_path: project_dir.join("project.json"),
            state_path: project_dir.join("session.json"),
            lock_path: project_dir.join("active-reviewer.lock.json"),
        };
        let identity = ReviewProjectIdentity {
            project_key,
            source_root,
            source_label,
        };
        Ok(Self { identity, paths })
    }

    pub(super) fn paths(&self) -> &ReviewProjectPaths {
        &self.paths
    }

    pub(super) fn ensure_layout(&self) -> Result<()> {
        ensure_dir(&self.paths.project_dir)?;
        self.save_metadata()
    }

    fn save_metadata(&self) -> Result<()> {
        write_json(
            &self.paths.project_metadata_path,
            &ReviewProjectMetadata {
                version: REVIEW_STORE_VERSION,
                project_key: self.identity.project_key.clone(),
                source_root: self.identity.source_root.display().to_string(),
                source_label: self.identity.source_label.clone(),
            },
        )
    }

    /// Load persisted review state, pruning old completed sessions.
    ///
    /// Returns an error on I/O or deserialization failures.
    pub(super) fn load_state(&self) -> Result<ReviewState> {
        let (mut state, existed) = self.load_state_from_disk()?;
        let pruned = state.prune_completed_sessions_older_than(
            now_epoch_seconds(),
            COMPLETED_SESSION_TTL_SECONDS,
        );
        if existed && !pruned.is_empty() {
            self.save_state(&state)?;
        }
        Ok(state)
    }

    /// Save review state to disk.
    ///
    /// Returns an error on I/O failures.
    pub(super) fn save_state(&self, state: &ReviewState) -> Result<()> {
        self.ensure_layout()?;
        write_json(&self.paths.state_path, state)
    }

    /// Acquire an exclusive listener lock for this project.
    ///
    /// Returns an error when a lock already exists for a running PID.
    pub(super) fn acquire_lock(&self) -> Result<ReviewLockGuard> {
        self.ensure_layout()?;
        let pid = std::process::id();
        if let Ok(existing) = read_json::<ActiveReviewLock>(&self.paths.lock_path) {
            if pid_is_running(existing.pid) {
                bail!(
                    "a review listener is already active for `{}` (pid {})",
                    self.identity.source_label,
                    existing.pid
                );
            }
        }
        let lock = ActiveReviewLock {
            pid,
            acquired_at_epoch_seconds: now_epoch_seconds(),
            source_root: self.identity.source_root.display().to_string(),
        };
        write_json(&self.paths.lock_path, &lock)?;
        Ok(ReviewLockGuard {
            lock_path: self.paths.lock_path.clone(),
            pid,
        })
    }

    fn load_state_from_disk(&self) -> Result<(ReviewState, bool)> {
        match fs::read_to_string(&self.paths.state_path) {
            Ok(content) => {
                let state = serde_json::from_str(&content).with_context(|| {
                    format!(
                        "invalid state JSON at `{}`",
                        self.paths.state_path.display()
                    )
                })?;
                Ok((state, true))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                Ok((ReviewState::default(), false))
            }
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to read review state from `{}`",
                    self.paths.state_path.display()
                )
            }),
        }
    }
}

impl Drop for ReviewLockGuard {
    fn drop(&mut self) {
        if let Ok(existing) = read_json::<ActiveReviewLock>(&self.lock_path) {
            if existing.pid == self.pid {
                let _ = fs::remove_file(&self.lock_path);
            }
        }
    }
}

/// Check whether a process with the given PID is currently running.
///
/// Returns false when the PID cannot be signalled.
pub(crate) fn pid_is_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn resolve_project_key(source_root: &Path) -> String {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&source_root.display().to_string(), &mut hasher);
    format!("{:016x}", hasher.finish())
}

fn now_epoch_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let content =
        serde_json::to_string_pretty(value).context("failed to serialize JSON for review store")?;
    fs::write(path, content)
        .with_context(|| format!("failed to write review store file `{}`", path.display()))
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read review store file `{}`", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse review store file `{}`", path.display()))
}

/// Resolve the repository origin remote URL for the current working tree.
///
/// Returns an error when `git` cannot determine the origin URL.
pub(super) fn resolve_origin_remote(root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()
        .context("failed to run `git remote get-url origin`")?;
    if !output.status.success() {
        bail!(
            "failed to resolve origin remote: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn store_round_trips_state() -> Result<()> {
        let temp = tempdir()?;
        let store = ReviewProjectStore {
            identity: ReviewProjectIdentity {
                project_key: "test-key".to_string(),
                source_root: temp.path().to_path_buf(),
                source_label: "test".to_string(),
            },
            paths: ReviewProjectPaths {
                project_dir: temp.path().to_path_buf(),
                project_metadata_path: temp.path().join("project.json"),
                state_path: temp.path().join("session.json"),
                lock_path: temp.path().join("lock.json"),
            },
        };
        store.ensure_layout()?;

        let session = super::super::state::ReviewSession {
            pr_number: 42,
            pr_title: "Test PR".to_string(),
            pr_url: None,
            pr_author: None,
            head_branch: None,
            base_branch: None,
            linear_identifier: None,
            phase: super::super::state::ReviewPhase::Claimed,
            summary: "Claimed".to_string(),
            updated_at_epoch_seconds: now_epoch_seconds(),
            remediation_required: None,
            remediation_pr_number: None,
            remediation_pr_url: None,
        };
        let mut state = store.load_state()?;
        state.upsert(session);
        store.save_state(&state)?;

        let state = store.load_state()?;
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].pr_number, 42);

        Ok(())
    }

    #[test]
    fn lock_rejects_second_process() -> Result<()> {
        let temp = tempdir()?;
        let store = ReviewProjectStore {
            identity: ReviewProjectIdentity {
                project_key: "test-key".to_string(),
                source_root: temp.path().to_path_buf(),
                source_label: "test".to_string(),
            },
            paths: ReviewProjectPaths {
                project_dir: temp.path().to_path_buf(),
                project_metadata_path: temp.path().join("project.json"),
                state_path: temp.path().join("session.json"),
                lock_path: temp.path().join("lock.json"),
            },
        };
        store.ensure_layout()?;

        let _guard = store.acquire_lock()?;
        let result = store.acquire_lock();
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("already active"));

        Ok(())
    }
}

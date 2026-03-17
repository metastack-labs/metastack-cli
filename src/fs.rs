use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone)]
pub struct PlanningPaths {
    pub metastack_dir: PathBuf,
    pub agent_dir: PathBuf,
    pub backlog_dir: PathBuf,
    pub merge_runs_dir: PathBuf,
    pub backlog_template_dir: PathBuf,
    pub agent_briefs_dir: PathBuf,
    pub agent_sessions_dir: PathBuf,
    pub codebase_dir: PathBuf,
    pub workflows_dir: PathBuf,
    pub cron_dir: PathBuf,
    pub cron_runtime_dir: PathBuf,
    pub cron_runtime_jobs_dir: PathBuf,
    pub cron_runtime_logs_dir: PathBuf,
}

impl PlanningPaths {
    pub fn new(root: &Path) -> Self {
        let metastack_dir = root.join(".metastack");
        let agent_dir = metastack_dir.join("agents");
        let backlog_dir = metastack_dir.join("backlog");
        let merge_runs_dir = metastack_dir.join("merge-runs");
        let backlog_template_dir = backlog_dir.join("_TEMPLATE");
        let agent_briefs_dir = agent_dir.join("briefs");
        let agent_sessions_dir = agent_dir.join("sessions");
        let codebase_dir = metastack_dir.join("codebase");
        let workflows_dir = metastack_dir.join("workflows");
        let cron_dir = metastack_dir.join("cron");
        let cron_runtime_dir = cron_dir.join(".runtime");
        let cron_runtime_jobs_dir = cron_runtime_dir.join("jobs");
        let cron_runtime_logs_dir = cron_runtime_dir.join("logs");

        Self {
            metastack_dir,
            agent_dir,
            backlog_dir,
            merge_runs_dir,
            backlog_template_dir,
            agent_briefs_dir,
            agent_sessions_dir,
            codebase_dir,
            workflows_dir,
            cron_dir,
            cron_runtime_dir,
            cron_runtime_jobs_dir,
            cron_runtime_logs_dir,
        }
    }

    pub fn scan_path(&self) -> PathBuf {
        self.codebase_dir.join("SCAN.md")
    }

    pub fn architecture_path(&self) -> PathBuf {
        self.codebase_dir.join("ARCHITECTURE.md")
    }

    pub fn concerns_path(&self) -> PathBuf {
        self.codebase_dir.join("CONCERNS.md")
    }

    pub fn conventions_path(&self) -> PathBuf {
        self.codebase_dir.join("CONVENTIONS.md")
    }

    pub fn integrations_path(&self) -> PathBuf {
        self.codebase_dir.join("INTEGRATIONS.md")
    }

    pub fn stack_path(&self) -> PathBuf {
        self.codebase_dir.join("STACK.md")
    }

    pub fn structure_path(&self) -> PathBuf {
        self.codebase_dir.join("STRUCTURE.md")
    }

    pub fn testing_path(&self) -> PathBuf {
        self.codebase_dir.join("TESTING.md")
    }

    pub fn scan_log_path(&self) -> PathBuf {
        self.agent_sessions_dir.join("scan.log")
    }

    pub fn legacy_scan_paths(&self) -> [PathBuf; 3] {
        [
            self.codebase_dir.join("overview.md"),
            self.codebase_dir.join("stack.md"),
            self.codebase_dir.join("details.md"),
        ]
    }

    pub fn meta_path(&self) -> PathBuf {
        self.metastack_dir.join("meta.json")
    }

    pub fn legacy_agent_dir(&self) -> PathBuf {
        self.metastack_dir.join("agent")
    }

    pub fn legacy_agent_briefs_dir(&self) -> PathBuf {
        self.legacy_agent_dir().join("agent-briefs")
    }

    pub fn legacy_agent_sessions_dir(&self) -> PathBuf {
        self.legacy_agent_dir().join("agent-sessions")
    }

    pub fn backlog_issue_dir(&self, identifier: &str) -> PathBuf {
        self.backlog_dir.join(identifier)
    }

    pub fn merge_run_dir(&self, run_id: &str) -> PathBuf {
        self.merge_runs_dir.join(run_id)
    }

    pub fn agent_issue_context_dir(&self, identifier: &str) -> PathBuf {
        self.agent_dir.join("issue-context").join(identifier)
    }

    pub fn agent_issue_context_manifest_path(&self, identifier: &str) -> PathBuf {
        self.agent_issue_context_dir(identifier).join("README.md")
    }

    pub fn cron_readme_path(&self) -> PathBuf {
        self.cron_dir.join("README.md")
    }

    pub fn cron_job_path(&self, name: &str) -> PathBuf {
        self.cron_dir.join(format!("{name}.md"))
    }

    pub fn cron_scheduler_pid_path(&self) -> PathBuf {
        self.cron_runtime_dir.join("scheduler.pid")
    }

    pub fn cron_scheduler_state_path(&self) -> PathBuf {
        self.cron_runtime_dir.join("scheduler.json")
    }

    pub fn cron_scheduler_log_path(&self) -> PathBuf {
        self.cron_runtime_dir.join("scheduler.log")
    }

    pub fn cron_job_state_path(&self, name: &str) -> PathBuf {
        self.cron_runtime_jobs_dir.join(format!("{name}.json"))
    }

    pub fn cron_job_log_path(&self, name: &str) -> PathBuf {
        self.cron_runtime_logs_dir.join(format!("{name}.log"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileWriteStatus {
    Created,
    Updated,
    Unchanged,
}

pub fn canonicalize_existing_dir(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("failed to resolve repository root `{}`", path.display()))
}

pub fn sibling_workspace_root(root: &Path) -> Result<PathBuf> {
    let repo_name = root
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("failed to resolve the repository directory name"))?;
    let parent = root
        .parent()
        .ok_or_else(|| anyhow!("failed to resolve the repository parent directory"))?;
    Ok(parent.join(format!("{repo_name}-workspace")))
}

pub fn ensure_workspace_path_is_safe(
    source_root: &Path,
    workspace_root: &Path,
    workspace_path: &Path,
) -> Result<()> {
    let source_root = source_root
        .canonicalize()
        .with_context(|| format!("failed to resolve `{}`", source_root.display()))?;
    let workspace_root = workspace_root
        .canonicalize()
        .with_context(|| format!("failed to resolve `{}`", workspace_root.display()))?;
    let workspace_path = workspace_path
        .canonicalize()
        .with_context(|| format!("failed to resolve `{}`", workspace_path.display()))?;

    if workspace_path == source_root || workspace_path.starts_with(&source_root) {
        bail!("refusing to use a workspace inside the source checkout");
    }
    if !workspace_path.starts_with(&workspace_root) {
        bail!(
            "refusing to use workspace outside the configured workspace root: `{}`",
            workspace_path.display()
        );
    }

    Ok(())
}

pub fn ensure_dir(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }

    fs::create_dir_all(path)
        .with_context(|| format!("failed to create directory `{}`", path.display()))?;
    Ok(true)
}

pub fn write_text_file(path: &Path, contents: &str, overwrite: bool) -> Result<FileWriteStatus> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    match fs::read_to_string(path) {
        Ok(existing) if existing == contents => Ok(FileWriteStatus::Unchanged),
        Ok(_) if !overwrite => Ok(FileWriteStatus::Unchanged),
        Ok(_) => {
            fs::write(path, contents)
                .with_context(|| format!("failed to write `{}`", path.display()))?;
            Ok(FileWriteStatus::Updated)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::write(path, contents)
                .with_context(|| format!("failed to create `{}`", path.display()))?;
            Ok(FileWriteStatus::Created)
        }
        Err(error) => {
            Err(error).with_context(|| format!("failed to read existing file `{}`", path.display()))
        }
    }
}

pub fn display_path(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use tempfile::tempdir;

    use super::{ensure_workspace_path_is_safe, sibling_workspace_root};

    #[test]
    fn sibling_workspace_root_uses_repo_parent_and_name() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("demo-repo");
        std::fs::create_dir_all(&repo_root)?;

        let workspace_root = sibling_workspace_root(&repo_root)?;

        assert_eq!(workspace_root, temp.path().join("demo-repo-workspace"));
        Ok(())
    }

    #[test]
    fn ensure_workspace_path_is_safe_accepts_sibling_workspace_paths() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let workspace_root = temp.path().join("repo-workspace");
        let workspace_path = workspace_root.join("merge-runs").join("run-1");
        std::fs::create_dir_all(&repo_root)?;
        std::fs::create_dir_all(&workspace_path)?;

        ensure_workspace_path_is_safe(&repo_root, &workspace_root, &workspace_path)?;
        Ok(())
    }

    #[test]
    fn ensure_workspace_path_is_safe_rejects_paths_inside_the_source_checkout() -> Result<()> {
        let temp = tempdir()?;
        let repo_root = temp.path().join("repo");
        let workspace_root = repo_root.join("nested-workspaces");
        let workspace_path = workspace_root.join("run-1");
        std::fs::create_dir_all(&workspace_path)?;

        let error = ensure_workspace_path_is_safe(&repo_root, &workspace_root, &workspace_path)
            .expect_err("workspace inside repo should be rejected");
        assert!(
            error
                .to_string()
                .contains("refusing to use a workspace inside the source checkout")
        );
        Ok(())
    }
}

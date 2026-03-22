use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

use crate::fs::{ensure_workspace_path_is_safe, sibling_workspace_root};

/// Result of provisioning an improve workspace from a source PR branch.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ImproveWorkspace {
    pub workspace_path: PathBuf,
    pub improve_branch: String,
    pub head_sha: String,
}

/// Provision an isolated workspace for an improve session.
///
/// Clones from the repository origin remote, checks out the source PR branch,
/// and creates a new `improve/<source_branch>` branch for the improvement work.
/// Returns an error if the clone, fetch, or branch operations fail.
#[allow(dead_code)]
pub fn ensure_improve_workspace(
    root: &Path,
    source_branch: &str,
    session_id: &str,
) -> Result<ImproveWorkspace> {
    let workspace_root = sibling_workspace_root(root)?;
    fs::create_dir_all(&workspace_root)
        .with_context(|| format!("failed to create `{}`", workspace_root.display()))?;

    let workspace_path = workspace_root.join(format!("improve-{session_id}"));
    let improve_branch = crate::improve::state::improve_branch_name(source_branch);

    if workspace_path.exists() {
        ensure_workspace_path_is_safe(root, &workspace_root, &workspace_path)?;
        refresh_improve_workspace(&workspace_path, source_branch, &improve_branch)?;
    } else {
        clone_improve_workspace(root, &workspace_path, source_branch, &improve_branch)?;
    }

    let head_sha = git_stdout(&workspace_path, &["rev-parse", "--short", "HEAD"])?;

    Ok(ImproveWorkspace {
        workspace_path,
        improve_branch,
        head_sha,
    })
}

fn clone_improve_workspace(
    root: &Path,
    workspace_path: &Path,
    source_branch: &str,
    improve_branch: &str,
) -> Result<()> {
    let remote_url = git_stdout(root, &["remote", "get-url", "origin"])
        .context("failed to resolve the repository origin remote")?;

    run_git(root, &["fetch", "origin", source_branch])
        .with_context(|| format!("failed to fetch source branch `{source_branch}`"))?;

    run_git(
        root,
        &[
            "clone",
            "--origin",
            "origin",
            &remote_url,
            workspace_path
                .to_str()
                .ok_or_else(|| anyhow!("workspace path is not valid utf-8"))?,
        ],
    )?;

    run_git(workspace_path, &["fetch", "origin", source_branch])?;
    run_git(
        workspace_path,
        &[
            "checkout",
            "-B",
            improve_branch,
            &format!("origin/{source_branch}"),
        ],
    )?;

    Ok(())
}

fn refresh_improve_workspace(
    workspace_path: &Path,
    source_branch: &str,
    improve_branch: &str,
) -> Result<()> {
    if !workspace_path.is_dir() {
        bail!(
            "existing workspace path `{}` is not a directory",
            workspace_path.display()
        );
    }

    run_git(workspace_path, &["fetch", "origin", source_branch])?;
    run_git(
        workspace_path,
        &[
            "checkout",
            "-B",
            improve_branch,
            &format!("origin/{source_branch}"),
        ],
    )?;
    run_git(
        workspace_path,
        &["reset", "--hard", &format!("origin/{source_branch}")],
    )?;
    run_git(workspace_path, &["clean", "-fd", "--exclude=.metastack/"])?;

    Ok(())
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

/// Push the improve branch to origin from the workspace.
///
/// Returns an error when `git push` fails.
#[allow(dead_code)]
pub fn push_improve_branch(workspace_path: &Path, improve_branch: &str) -> Result<()> {
    run_git(
        workspace_path,
        &[
            "push",
            "--set-upstream",
            "origin",
            improve_branch,
            "--force-with-lease",
        ],
    )
    .with_context(|| format!("failed to push improve branch `{improve_branch}`"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn improve_branch_derives_from_source() {
        assert_eq!(
            crate::improve::state::improve_branch_name("met-42-feature"),
            "improve/met-42-feature"
        );
    }
}

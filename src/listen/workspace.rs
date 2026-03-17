use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

use crate::config::ListenRefreshPolicy;
use crate::fs::{ensure_workspace_path_is_safe, sibling_workspace_root};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketWorkspaceProvisioning {
    Created,
    Refreshed,
    Recreated,
}

#[derive(Debug, Clone)]
pub struct TicketWorkspace {
    pub workspace_root: PathBuf,
    pub workspace_path: PathBuf,
    pub branch: String,
    pub base_ref: String,
    pub head_sha: String,
    pub provisioning: TicketWorkspaceProvisioning,
}

pub fn ensure_ticket_workspace(
    root: &Path,
    refresh_policy: ListenRefreshPolicy,
    identifier: &str,
    title: &str,
) -> Result<TicketWorkspace> {
    let workspace_root = ticket_workspace_root(root)?;
    fs::create_dir_all(&workspace_root)
        .with_context(|| format!("failed to create `{}`", workspace_root.display()))?;

    let workspace_path = workspace_root.join(identifier);
    let branch = build_branch_name(identifier, title);
    let base_ref = "origin/main".to_string();

    match refresh_policy {
        ListenRefreshPolicy::ReuseAndRefresh if workspace_path.exists() => refresh_workspace_clone(
            root,
            &workspace_root,
            &workspace_path,
            &branch,
            &base_ref,
            TicketWorkspaceProvisioning::Refreshed,
        ),
        ListenRefreshPolicy::RecreateFromOriginMain if workspace_path.exists() => {
            remove_existing_workspace(root, &workspace_root, &workspace_path)?;
            clone_workspace(
                root,
                &workspace_root,
                &workspace_path,
                &branch,
                &base_ref,
                true,
            )
        }
        _ => clone_workspace(
            root,
            &workspace_root,
            &workspace_path,
            &branch,
            &base_ref,
            false,
        ),
    }
}

fn ticket_workspace_root(root: &Path) -> Result<PathBuf> {
    sibling_workspace_root(root)
}

fn clone_workspace(
    root: &Path,
    workspace_root: &Path,
    workspace_path: &Path,
    branch: &str,
    base_ref: &str,
    recreated: bool,
) -> Result<TicketWorkspace> {
    let remote_url = git_stdout(root, &["remote", "get-url", "origin"])
        .context("failed to resolve the repository origin remote")?;
    run_git(root, &["fetch", "origin", "main"])?;
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

    refresh_workspace_clone(
        root,
        workspace_root,
        workspace_path,
        branch,
        base_ref,
        if recreated {
            TicketWorkspaceProvisioning::Recreated
        } else {
            TicketWorkspaceProvisioning::Created
        },
    )
}

fn refresh_workspace_clone(
    root: &Path,
    workspace_root: &Path,
    workspace_path: &Path,
    branch: &str,
    base_ref: &str,
    provisioning: TicketWorkspaceProvisioning,
) -> Result<TicketWorkspace> {
    if !workspace_path.is_dir() {
        bail!(
            "existing workspace path `{}` is not a directory",
            workspace_path.display()
        );
    }

    let current_branch = git_stdout(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if current_branch == "HEAD" {
        bail!(
            "existing workspace `{}` is detached; manual intervention is required before the listener can reuse it",
            workspace_path.display()
        );
    }

    let conflicted = git_stdout(workspace_path, &["diff", "--name-only", "--diff-filter=U"])?;
    if !conflicted.trim().is_empty() {
        bail!(
            "existing workspace `{}` has unresolved merge conflicts",
            workspace_path.display()
        );
    }

    run_git(workspace_path, &["fetch", "origin", "main"])?;
    run_git(workspace_path, &["checkout", "-B", branch, base_ref])?;
    run_git(workspace_path, &["reset", "--hard", base_ref])?;
    run_git(workspace_path, &["clean", "-fd", "--exclude=.metastack/"])?;
    let head_sha = git_stdout(workspace_path, &["rev-parse", "--short", "HEAD"])?;

    // Keep the source repo pointed at the latest upstream main as well.
    let _ = run_git(root, &["fetch", "origin", "main"]);

    Ok(TicketWorkspace {
        workspace_root: workspace_root.to_path_buf(),
        workspace_path: workspace_path.to_path_buf(),
        branch: branch.to_string(),
        base_ref: base_ref.to_string(),
        head_sha,
        provisioning,
    })
}

fn remove_existing_workspace(
    root: &Path,
    workspace_root: &Path,
    workspace_path: &Path,
) -> Result<()> {
    if !workspace_path.is_dir() {
        bail!(
            "existing workspace path `{}` is not a directory",
            workspace_path.display()
        );
    }
    ensure_workspace_path_is_safe(root, workspace_root, workspace_path)?;

    fs::remove_dir_all(workspace_path)
        .with_context(|| format!("failed to remove `{}`", workspace_path.display()))
}

fn build_branch_name(identifier: &str, title: &str) -> String {
    let mut slug = title
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    let suffix = if slug.is_empty() {
        identifier.to_ascii_lowercase()
    } else {
        format!("{}-{}", identifier.to_ascii_lowercase(), slug)
    };
    suffix.chars().take(72).collect()
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

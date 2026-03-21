use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PullRequestPublishMode {
    Ready,
    Draft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PullRequestLifecycleAction {
    CreatedReady,
    CreatedDraft,
    UpdatedExisting,
    PromotedToReady,
    AlreadyReady,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PullRequestLifecycleResult {
    pub(crate) number: u64,
    pub(crate) url: String,
    pub(crate) action: PullRequestLifecycleAction,
    pub(crate) is_draft: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct PullRequestPublishRequest<'a> {
    pub(crate) head_branch: &'a str,
    pub(crate) base_branch: &'a str,
    pub(crate) title: &'a str,
    pub(crate) body_path: &'a Path,
    pub(crate) mode: PullRequestPublishMode,
}

#[derive(Debug, Clone)]
pub(crate) struct GhCli;

#[derive(Debug, Clone, Deserialize)]
struct BranchPullRequest {
    number: u64,
    url: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
}

impl GhCli {
    /// Run `gh` and deserialize its JSON output.
    ///
    /// Returns an error when the command cannot be launched, exits unsuccessfully,
    /// or emits invalid JSON.
    pub(crate) fn run_json<T: DeserializeOwned>(&self, root: &Path, args: &[&str]) -> Result<T> {
        let output = Command::new("gh")
            .args(args)
            .current_dir(root)
            .output()
            .with_context(|| format!("failed to run `gh {}`", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "gh {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("failed to decode JSON from `gh {}`", args.join(" ")))
    }

    /// Run `gh` without expecting JSON output.
    ///
    /// Returns an error when the command cannot be launched or exits unsuccessfully.
    pub(crate) fn run_plain(&self, root: &Path, args: &[&str]) -> Result<()> {
        let output = Command::new("gh")
            .args(args)
            .current_dir(root)
            .output()
            .with_context(|| format!("failed to run `gh {}`", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "gh {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    /// Create a branch pull request or update the existing open PR for the same head/base pair.
    ///
    /// Returns an error when `gh` cannot inspect, create, or edit the pull request.
    pub(crate) fn publish_branch_pull_request(
        &self,
        workspace_path: &Path,
        request: PullRequestPublishRequest<'_>,
    ) -> Result<PullRequestLifecycleResult> {
        if let Some(existing) = self.find_open_branch_pull_request_raw(
            workspace_path,
            request.head_branch,
            request.base_branch,
        )? {
            self.run_plain(
                workspace_path,
                &[
                    "pr",
                    "edit",
                    &existing.number.to_string(),
                    "--title",
                    request.title,
                    "--body-file",
                    body_path_arg(request.body_path)?,
                ],
            )?;
            return Ok(PullRequestLifecycleResult {
                number: existing.number,
                url: existing.url,
                action: PullRequestLifecycleAction::UpdatedExisting,
                is_draft: existing.is_draft,
            });
        }

        let mut create_args = vec![
            "pr",
            "create",
            "--base",
            request.base_branch,
            "--head",
            request.head_branch,
            "--title",
            request.title,
            "--body-file",
            body_path_arg(request.body_path)?,
        ];
        if request.mode == PullRequestPublishMode::Draft {
            create_args.push("--draft");
        }
        let created = self
            .run_json::<BranchPullRequest>(
                workspace_path,
                &[&create_args[..], &["--json", "number,url,isDraft"]].concat(),
            )
            .or_else(|_| {
                self.run_plain(workspace_path, &create_args)?;
                self.find_open_branch_pull_request_raw(
                    workspace_path,
                    request.head_branch,
                    request.base_branch,
                )?
                .ok_or_else(|| {
                    anyhow!(
                        "gh created a pull request for `{}` but no open PR was returned",
                        request.head_branch
                    )
                })
            })?;

        Ok(PullRequestLifecycleResult {
            number: created.number,
            url: created.url,
            action: match request.mode {
                PullRequestPublishMode::Ready => PullRequestLifecycleAction::CreatedReady,
                PullRequestPublishMode::Draft => PullRequestLifecycleAction::CreatedDraft,
            },
            is_draft: created.is_draft,
        })
    }

    /// Promote the open branch PR for the provided head/base pair to ready for review.
    ///
    /// Returns an error when no matching open PR exists or when `gh` fails to promote it.
    pub(crate) fn promote_branch_pull_request_to_ready(
        &self,
        workspace_path: &Path,
        head_branch: &str,
        base_branch: &str,
    ) -> Result<PullRequestLifecycleResult> {
        let existing = self
            .find_open_branch_pull_request_raw(workspace_path, head_branch, base_branch)?
            .ok_or_else(|| {
                anyhow!(
                    "no open pull request exists for branch `{head_branch}` against `{base_branch}`"
                )
            })?;
        if !existing.is_draft {
            return Ok(PullRequestLifecycleResult {
                number: existing.number,
                url: existing.url,
                action: PullRequestLifecycleAction::AlreadyReady,
                is_draft: false,
            });
        }

        self.run_plain(
            workspace_path,
            &["pr", "ready", &existing.number.to_string()],
        )?;
        Ok(PullRequestLifecycleResult {
            number: existing.number,
            url: existing.url,
            action: PullRequestLifecycleAction::PromotedToReady,
            is_draft: false,
        })
    }

    /// Ensure the requested label exists in the repository.
    ///
    /// Returns an error when `gh` cannot create the label for reasons other than it already existing.
    pub(crate) fn ensure_label_exists(
        &self,
        workspace_path: &Path,
        label: &str,
        color: &str,
        description: &str,
    ) -> Result<()> {
        match self.run_plain(
            workspace_path,
            &[
                "label",
                "create",
                label,
                "--color",
                color,
                "--description",
                description,
            ],
        ) {
            Ok(()) => Ok(()),
            Err(error) if error.to_string().contains("already exists") => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Add a label to the provided pull request number.
    ///
    /// Returns an error when `gh` cannot edit the pull request.
    pub(crate) fn add_label_to_pull_request(
        &self,
        workspace_path: &Path,
        number: u64,
        label: &str,
    ) -> Result<()> {
        self.run_plain(
            workspace_path,
            &["pr", "edit", &number.to_string(), "--add-label", label],
        )
    }

    fn find_open_branch_pull_request_raw(
        &self,
        workspace_path: &Path,
        head_branch: &str,
        base_branch: &str,
    ) -> Result<Option<BranchPullRequest>> {
        let existing = self.run_json::<Vec<BranchPullRequest>>(
            workspace_path,
            &[
                "pr",
                "list",
                "--state",
                "open",
                "--head",
                head_branch,
                "--base",
                base_branch,
                "--json",
                "number,url,isDraft",
            ],
        )?;
        Ok(existing.into_iter().next())
    }
}

fn body_path_arg(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("invalid PR body path `{}`", path.display()))
}

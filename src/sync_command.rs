use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::backlog::{
    BacklogIssueMetadata, BacklogSyncStatus, INDEX_FILE_NAME, ManagedFileRecord, backlog_issue_dir,
    backlog_issue_index_path, backlog_issue_metadata_path, collect_local_sync_files,
    compute_local_sync_hash, compute_remote_sync_hash, load_issue_metadata,
    resolve_backlog_sync_status, save_issue_metadata, write_issue_attachment_file,
    write_issue_description,
};
use crate::cli::{LinearClientArgs, SyncPullArgs, SyncPushArgs};
use crate::config::load_required_planning_meta;
use crate::fs::{canonicalize_existing_dir, display_path, ensure_dir};
use crate::linear::{AttachmentCreateRequest, IssueEditSpec, IssueListFilters, IssueSummary};
use crate::scaffold::ensure_planning_layout;
use crate::sync_dashboard::{
    SyncDashboardData, SyncDashboardExit, SyncDashboardIssue, SyncDashboardOptions,
    SyncSelectionAction, run_sync_dashboard,
};
use crate::{LinearCommandContext, load_linear_command_context};

const MANAGED_ATTACHMENT_MARKER: &str = "metastack-cli";

pub async fn run_sync_dashboard_command(
    client_args: &LinearClientArgs,
    project_override: Option<&str>,
    options: SyncDashboardOptions,
) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "sync")?;
    let LinearCommandContext {
        service,
        default_team,
        default_project_id,
    } = load_linear_command_context(client_args, None)?;

    let (filter, title) = if let Some(project_name) = project_override {
        (
            IssueListFilters {
                team: default_team,
                project: Some(project_name.to_string()),
                limit: usize::MAX,
                ..IssueListFilters::default()
            },
            format!("meta backlog sync ({project_name})"),
        )
    } else {
        let project_id = default_project_id.ok_or_else(|| {
            anyhow!(
                "`meta backlog sync` requires a repo default project or `--project`. Run `meta runtime setup --root . --project <PROJECT>` or pass `--project \"Project Name\"`."
            )
        })?;
        (
            IssueListFilters {
                team: default_team,
                project_id: Some(project_id.clone()),
                limit: usize::MAX,
                ..IssueListFilters::default()
            },
            format!("meta backlog sync ({project_id})"),
        )
    };

    let issues = service.list_issues(filter).await?;

    match run_sync_dashboard(
        SyncDashboardData {
            title,
            issues: issues
                .into_iter()
                .map(|issue| {
                    let issue_dir = backlog_issue_dir(&root, &issue.identifier);
                    let metadata = load_issue_metadata_if_present(&issue_dir)?;
                    let resolution = resolve_backlog_sync_status(
                        metadata.as_ref(),
                        compute_local_sync_hash(&issue_dir)?,
                        Some(issue_remote_hash(&issue)),
                    );
                    Ok(SyncDashboardIssue {
                        issue,
                        local_status: resolution.status,
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        },
        options,
    )? {
        SyncDashboardExit::Snapshot(snapshot) => println!("{snapshot}"),
        SyncDashboardExit::Cancelled => println!("Sync canceled."),
        SyncDashboardExit::Selected(selection) => {
            let issue_args = SyncPullArgs {
                issue: selection.issue_identifier,
            };
            match selection.action {
                SyncSelectionAction::Pull => run_sync_pull(client_args, &issue_args).await?,
                SyncSelectionAction::Push => {
                    run_sync_push(
                        client_args,
                        &SyncPushArgs {
                            issue: issue_args.issue,
                            update_description: false,
                        },
                    )
                    .await?
                }
            }
        }
    }

    Ok(())
}

pub async fn run_sync_pull(client_args: &LinearClientArgs, args: &SyncPullArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "sync")?;
    ensure_planning_layout(&root, false)?;
    let LinearCommandContext { service, .. } = load_linear_command_context(client_args, None)?;
    let issue = service.load_issue(&args.issue).await?;
    let issue_dir = backlog_issue_dir(&root, &issue.identifier);
    ensure_dir(&issue_dir)?;
    let metadata = load_issue_metadata_if_present(&issue_dir)?;
    let remote_hash = issue_remote_hash(&issue);
    let resolution = resolve_backlog_sync_status(
        metadata.as_ref(),
        compute_local_sync_hash(&issue_dir)?,
        Some(remote_hash.clone()),
    );

    if needs_pull_overwrite_confirmation(resolution.status) {
        let local_description =
            read_optional_text_file(&backlog_issue_index_path(&root, &issue.identifier))?;
        let diff = render_sync_diff(
            &local_description,
            issue.description.as_deref().unwrap_or_default(),
        );

        if io::stdin().is_terminal() && io::stdout().is_terminal() {
            if !prompt_pull_overwrite(&issue.identifier, resolution.status, &diff)? {
                println!(
                    "Canceled pull for {}. Local backlog files and hash baselines were left unchanged.",
                    issue.identifier
                );
                return Ok(());
            }
        } else {
            bail!(
                "`meta backlog sync pull {}` refused to overwrite local backlog content because the sync state is `{}`; rerun in a TTY to review the diff and confirm the overwrite",
                issue.identifier,
                resolution.status.as_str(),
            );
        }
    }

    write_issue_description(
        &root,
        &issue.identifier,
        issue.description.as_deref().unwrap_or_default(),
    )?;

    let mut managed_files = Vec::new();
    for attachment in &issue.attachments {
        let Some(relative_path) = managed_attachment_path(&attachment.metadata) else {
            continue;
        };
        let contents = service
            .download_file(&attachment.url)
            .await
            .with_context(|| {
                format!(
                    "failed to restore managed attachment `{}` from `{}`",
                    attachment.title, attachment.url
                )
            })?;
        write_issue_attachment_file(&issue_dir, &relative_path, &contents)?;
        managed_files.push(ManagedFileRecord {
            path: relative_path,
            attachment_id: Some(attachment.id.clone()),
            url: Some(attachment.url.clone()),
        });
    }

    let local_hash = compute_local_sync_hash(&issue_dir)?.ok_or_else(|| {
        anyhow!(
            "backlog issue directory `{}` disappeared during sync",
            issue_dir.display()
        )
    })?;
    save_issue_metadata(
        &issue_dir,
        &build_issue_metadata(&issue, managed_files, Some(local_hash), Some(remote_hash)),
    )?;

    println!(
        "Pulled {} into {} (restored {} managed attachment file{}).",
        issue.identifier,
        display_path(&issue_dir, &root),
        issue
            .attachments
            .iter()
            .filter(|attachment| managed_attachment_path(&attachment.metadata).is_some())
            .count(),
        plural_suffix(
            issue
                .attachments
                .iter()
                .filter(|attachment| managed_attachment_path(&attachment.metadata).is_some())
                .count()
        ),
    );

    Ok(())
}

pub async fn run_sync_push(client_args: &LinearClientArgs, args: &SyncPushArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "sync")?;
    ensure_planning_layout(&root, false)?;
    if args.update_description {
        guard_listen_issue_description_sync(&args.issue)?;
    }
    let LinearCommandContext { service, .. } = load_linear_command_context(client_args, None)?;
    let issue = service.load_issue(&args.issue).await?;
    let issue_dir = backlog_issue_dir(&root, &issue.identifier);
    if !issue_dir.is_dir() {
        bail!(
            "backlog item `{}` was not found at `{}`; run `meta backlog sync pull {}` first",
            issue.identifier,
            issue_dir.display(),
            issue.identifier
        );
    }

    let index_path = backlog_issue_index_path(&root, &issue.identifier);
    let description = fs::read_to_string(&index_path).with_context(|| {
        format!(
            "failed to read `{}`; `meta backlog sync push` requires `{}`",
            index_path.display(),
            INDEX_FILE_NAME
        )
    })?;
    let metadata = load_issue_metadata_if_present(&issue_dir)?;
    let local_hash = compute_local_sync_hash(&issue_dir)?.ok_or_else(|| {
        anyhow!(
            "backlog issue directory `{}` disappeared before push",
            issue_dir.display()
        )
    })?;
    let resolution = resolve_backlog_sync_status(
        metadata.as_ref(),
        Some(local_hash.clone()),
        Some(issue_remote_hash(&issue)),
    );

    if args.update_description
        && matches!(
            resolution.status,
            BacklogSyncStatus::RemoteAhead | BacklogSyncStatus::Diverged
        )
    {
        bail!(
            "`meta backlog sync push {}` refused to update the Linear description because the sync state is `{}`; pull first or reconcile the local backlog before retrying with `--update-description`",
            issue.identifier,
            resolution.status.as_str(),
        );
    }
    if args.update_description {
        service
            .edit_issue(IssueEditSpec {
                identifier: issue.identifier.clone(),
                title: None,
                description: Some(description.clone()),
                project: None,
                state: None,
                priority: None,
            })
            .await?;
    }

    let local_files = collect_local_sync_files(&issue_dir)?
        .into_iter()
        .filter(|file| file.relative_path != INDEX_FILE_NAME)
        .collect::<Vec<_>>();
    let local_paths = local_files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let local_path_count = local_paths.len();
    let existing_managed = issue
        .attachments
        .iter()
        .filter_map(|attachment| {
            managed_attachment_path(&attachment.metadata).map(|path| (path, attachment.id.clone()))
        })
        .collect::<BTreeMap<_, _>>();

    for (path, attachment_id) in &existing_managed {
        if !local_paths.contains(path) {
            service.delete_attachment(attachment_id).await?;
        }
    }

    let mut managed_files = Vec::new();
    for file in local_files {
        if let Some(existing_id) = existing_managed.get(file.relative_path.as_str()) {
            service.delete_attachment(existing_id).await?;
        }

        let upload_name = upload_name(&file.relative_path);
        let asset_url = service
            .upload_file(&upload_name, &file.content_type, file.contents.clone())
            .await
            .with_context(|| {
                format!(
                    "failed to upload managed backlog file `{}`",
                    file.absolute_path.display()
                )
            })?;
        let attachment = service
            .create_attachment(AttachmentCreateRequest {
                issue_id: issue.id.clone(),
                title: file.title.clone(),
                url: asset_url,
                metadata: managed_attachment_metadata(&file.relative_path),
            })
            .await?;
        managed_files.push(ManagedFileRecord {
            path: file.relative_path,
            attachment_id: Some(attachment.id),
            url: Some(attachment.url),
        });
    }

    let remote_description = if args.update_description {
        description
    } else {
        issue.description.clone().unwrap_or_default()
    };
    let remote_hash = compute_remote_sync_hash(&remote_description, &managed_files);
    save_issue_metadata(
        &issue_dir,
        &build_issue_metadata(&issue, managed_files, Some(local_hash), Some(remote_hash)),
    )?;

    println!(
        "Pushed {} from {} (synced {} managed attachment file{}; {}).",
        issue.identifier,
        display_path(&issue_dir, &root),
        local_path_count,
        plural_suffix(local_path_count),
        if args.update_description {
            "updated Linear issue description from index.md"
        } else {
            "left the Linear issue description unchanged; pass --update-description to send index.md"
        },
    );

    Ok(())
}

fn guard_listen_issue_description_sync(identifier: &str) -> Result<()> {
    let unattended = std::env::var("METASTACK_LISTEN_UNATTENDED")
        .ok()
        .is_some_and(|value| value == "1");
    let active_issue = std::env::var("METASTACK_LINEAR_ISSUE_IDENTIFIER").ok();

    if unattended
        && active_issue
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case(identifier))
    {
        bail!(
            "`meta backlog sync push {identifier}` is disabled during `meta agents listen` because it would overwrite the primary Linear issue description; update the workpad comment instead"
        );
    }

    Ok(())
}

fn build_issue_metadata(
    issue: &IssueSummary,
    managed_files: Vec<ManagedFileRecord>,
    local_hash: Option<String>,
    remote_hash: Option<String>,
) -> BacklogIssueMetadata {
    BacklogIssueMetadata {
        issue_id: issue.id.clone(),
        identifier: issue.identifier.clone(),
        title: issue.title.clone(),
        url: issue.url.clone(),
        team_key: issue.team.key.clone(),
        project_id: issue.project.as_ref().map(|project| project.id.clone()),
        project_name: issue.project.as_ref().map(|project| project.name.clone()),
        parent_id: issue.parent.as_ref().map(|parent| parent.id.clone()),
        parent_identifier: issue
            .parent
            .as_ref()
            .map(|parent| parent.identifier.clone()),
        local_hash,
        remote_hash,
        managed_files,
    }
}

pub(crate) fn managed_attachment_metadata(relative_path: &str) -> Value {
    json!({
        "managedBy": MANAGED_ATTACHMENT_MARKER,
        "relativePath": relative_path,
    })
}

pub(crate) fn managed_attachment_path(metadata: &Value) -> Option<String> {
    let managed_by = metadata.get("managedBy")?.as_str()?;
    if managed_by != MANAGED_ATTACHMENT_MARKER {
        return None;
    }

    metadata
        .get("relativePath")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn upload_name(relative_path: &str) -> String {
    Path::new(relative_path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| relative_path.replace('/', "_"))
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn load_issue_metadata_if_present(issue_dir: &Path) -> Result<Option<BacklogIssueMetadata>> {
    let metadata_path = backlog_issue_metadata_path(issue_dir);
    if !metadata_path.is_file() {
        return Ok(None);
    }

    load_issue_metadata(issue_dir).map(Some)
}

fn issue_remote_hash(issue: &IssueSummary) -> String {
    compute_remote_sync_hash(
        issue.description.as_deref().unwrap_or_default(),
        &managed_file_records_from_issue(issue),
    )
}

fn managed_file_records_from_issue(issue: &IssueSummary) -> Vec<ManagedFileRecord> {
    let mut managed_files = issue
        .attachments
        .iter()
        .filter_map(|attachment| {
            managed_attachment_path(&attachment.metadata).map(|path| ManagedFileRecord {
                path,
                attachment_id: Some(attachment.id.clone()),
                url: Some(attachment.url.clone()),
            })
        })
        .collect::<Vec<_>>();
    managed_files.sort_by(|left, right| left.path.cmp(&right.path));
    managed_files
}

fn needs_pull_overwrite_confirmation(status: BacklogSyncStatus) -> bool {
    matches!(
        status,
        BacklogSyncStatus::RemoteAhead | BacklogSyncStatus::Diverged
    )
}

fn prompt_pull_overwrite(identifier: &str, status: BacklogSyncStatus, diff: &str) -> Result<bool> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    prompt_pull_overwrite_with_io(identifier, status, diff, &mut reader, &mut writer)
}

fn prompt_pull_overwrite_with_io(
    identifier: &str,
    status: BacklogSyncStatus,
    diff: &str,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<bool> {
    writeln!(
        writer,
        "`meta backlog sync pull {identifier}` detected `{}`. Review the incoming description diff before overwriting local backlog files:",
        status.as_str(),
    )?;
    writeln!(writer, "{diff}")?;
    writeln!(writer, "Choose [o]verwrite or [c]ancel:")?;
    writer.flush()?;

    let mut input = String::new();
    loop {
        input.clear();
        reader.read_line(&mut input)?;
        match input.trim().to_ascii_lowercase().as_str() {
            "o" | "overwrite" => return Ok(true),
            "c" | "cancel" => return Ok(false),
            _ => {
                writeln!(writer, "Enter `o` or `c`:")?;
                writer.flush()?;
            }
        }
    }
}

fn read_optional_text_file(path: &Path) -> Result<String> {
    if !path.is_file() {
        return Ok(String::new());
    }

    fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))
}

fn render_sync_diff(local_contents: &str, remote_contents: &str) -> String {
    let mut rows = Vec::new();
    rows.push("--- local/index.md".to_string());
    rows.push("+++ linear/description".to_string());
    rows.extend(diff_lines(local_contents, remote_contents));
    rows.join("\n")
}

fn diff_lines(left: &str, right: &str) -> Vec<String> {
    let left_lines = left.lines().map(str::to_string).collect::<Vec<_>>();
    let right_lines = right.lines().map(str::to_string).collect::<Vec<_>>();
    let mut table = vec![vec![0usize; right_lines.len() + 1]; left_lines.len() + 1];

    for left_index in (0..left_lines.len()).rev() {
        for right_index in (0..right_lines.len()).rev() {
            table[left_index][right_index] = if left_lines[left_index] == right_lines[right_index] {
                table[left_index + 1][right_index + 1] + 1
            } else {
                table[left_index + 1][right_index].max(table[left_index][right_index + 1])
            };
        }
    }

    let mut rendered = Vec::new();
    let mut left_index = 0usize;
    let mut right_index = 0usize;
    while left_index < left_lines.len() && right_index < right_lines.len() {
        if left_lines[left_index] == right_lines[right_index] {
            rendered.push(format!(" {}", left_lines[left_index]));
            left_index += 1;
            right_index += 1;
        } else if table[left_index + 1][right_index] >= table[left_index][right_index + 1] {
            rendered.push(format!("-{}", left_lines[left_index]));
            left_index += 1;
        } else {
            rendered.push(format!("+{}", right_lines[right_index]));
            right_index += 1;
        }
    }
    while left_index < left_lines.len() {
        rendered.push(format!("-{}", left_lines[left_index]));
        left_index += 1;
    }
    while right_index < right_lines.len() {
        rendered.push(format!("+{}", right_lines[right_index]));
        right_index += 1;
    }

    if rendered.is_empty() {
        rendered.push(" (no description changes)".to_string());
    }
    rendered
}

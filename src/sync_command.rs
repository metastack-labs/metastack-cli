use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::backlog::{
    BacklogIssueMetadata, INDEX_FILE_NAME, ManagedFileRecord, backlog_issue_dir,
    backlog_issue_index_path, collect_local_sync_files, save_issue_metadata,
    write_issue_attachment_file, write_issue_description,
};
use crate::cli::{LinearClientArgs, SyncIssueArgs};
use crate::config::load_required_planning_meta;
use crate::fs::{canonicalize_existing_dir, display_path, ensure_dir};
use crate::linear::{
    AttachmentCreateRequest, DashboardData, IssueEditSpec, IssueListFilters, IssueSummary,
};
use crate::scaffold::ensure_planning_layout;
use crate::sync_dashboard::{
    SyncDashboardExit, SyncDashboardOptions, SyncSelectionAction, run_sync_dashboard,
};
use crate::{LinearCommandContext, load_linear_command_context};

const MANAGED_ATTACHMENT_MARKER: &str = "metastack-cli";

pub async fn run_sync_dashboard_command(
    client_args: &LinearClientArgs,
    options: SyncDashboardOptions,
) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "sync")?;
    let LinearCommandContext {
        service,
        default_team,
        default_project_id,
    } = load_linear_command_context(client_args, None)?;
    let project_id = default_project_id.ok_or_else(|| {
        anyhow!(
            "`meta backlog sync` requires a repo default project. Run `meta runtime setup --root . --project <PROJECT>` and rerun."
        )
    })?;
    let issues = service
        .list_issues(IssueListFilters {
            team: default_team,
            project_id: Some(project_id.clone()),
            limit: usize::MAX,
            ..IssueListFilters::default()
        })
        .await?;

    match run_sync_dashboard(
        DashboardData {
            title: format!("meta backlog sync ({project_id})"),
            issues,
        },
        options,
    )? {
        SyncDashboardExit::Snapshot(snapshot) => println!("{snapshot}"),
        SyncDashboardExit::Cancelled => println!("Sync canceled."),
        SyncDashboardExit::Selected(selection) => {
            let issue_args = SyncIssueArgs {
                issue: selection.issue_identifier,
            };
            match selection.action {
                SyncSelectionAction::Pull => run_sync_pull(client_args, &issue_args).await?,
                SyncSelectionAction::Push => run_sync_push(client_args, &issue_args).await?,
            }
        }
    }

    Ok(())
}

pub async fn run_sync_pull(client_args: &LinearClientArgs, args: &SyncIssueArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "sync")?;
    ensure_planning_layout(&root, false)?;
    let LinearCommandContext { service, .. } = load_linear_command_context(client_args, None)?;
    let issue = service.load_issue(&args.issue).await?;
    let issue_dir = backlog_issue_dir(&root, &issue.identifier);
    ensure_dir(&issue_dir)?;

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

    save_issue_metadata(&issue_dir, &build_issue_metadata(&issue, managed_files))?;

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

pub async fn run_sync_push(client_args: &LinearClientArgs, args: &SyncIssueArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "sync")?;
    ensure_planning_layout(&root, false)?;
    guard_listen_issue_description_sync(&args.issue)?;
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
    service
        .edit_issue(IssueEditSpec {
            identifier: issue.identifier.clone(),
            title: None,
            description: Some(description),
            project: None,
            state: None,
            priority: None,
        })
        .await?;

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

    save_issue_metadata(&issue_dir, &build_issue_metadata(&issue, managed_files))?;

    println!(
        "Pushed {} from {} (synced {} managed attachment file{}).",
        issue.identifier,
        display_path(&issue_dir, &root),
        local_path_count,
        plural_suffix(local_path_count),
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

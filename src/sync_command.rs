use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use serde_json::{Value, json};

use crate::backlog::{
    BacklogIssueMetadata, BacklogSyncStatus, INDEX_FILE_NAME, ManagedFileRecord, backlog_issue_dir,
    backlog_issue_metadata_path, collect_remote_managed_sync_files, compute_local_sync_hash,
    compute_remote_sync_hash, load_issue_metadata, resolve_backlog_sync_status,
    save_issue_metadata, write_issue_attachment_file,
};
use crate::cli::{LinearClientArgs, SyncLinkArgs, SyncPullArgs, SyncPushArgs, SyncStatusArgs};
use crate::config::load_required_planning_meta;
use crate::fs::{
    PlanningPaths, canonicalize_existing_dir, display_path, ensure_dir, write_text_file,
};
use crate::linear::{
    AttachmentCreateRequest, IssueEditSpec, IssueListFilters, IssueSummary, LinearService,
    ProjectRef, ReqwestLinearClient, TeamRef, TicketDiscussionBudgets, WorkflowState,
    materialize_issue_context, prepare_issue_context,
};
use crate::scaffold::ensure_planning_layout;
use crate::sync_dashboard::{
    SyncDashboardData, SyncDashboardExit, SyncDashboardIssue, SyncDashboardOptions,
    SyncSelectionAction, run_sync_dashboard,
};
use crate::text_diff::render_text_diff;
use crate::{LinearCommandContext, load_linear_command_context};

const MANAGED_ATTACHMENT_MARKER: &str = "metastack-cli";
const HARNESS_SYNC_MARKER: &str = "[harness-sync]";

#[derive(Debug, Clone)]
struct BacklogSyncEntry {
    slug: String,
    issue_dir: PathBuf,
    metadata: Option<BacklogIssueMetadata>,
    local_hash: Option<String>,
    title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncExecutionOutcome {
    Synced,
    Skipped,
}

#[derive(Debug, Default)]
struct BatchSyncSummary {
    synced: usize,
    skipped: usize,
    errors: usize,
}

#[derive(Debug, Clone)]
struct ChecklistProgressSummary {
    milestones: Vec<ChecklistMilestoneProgress>,
    completed: usize,
    total: usize,
}

#[derive(Debug, Clone)]
struct ChecklistMilestoneProgress {
    title: String,
    completed: usize,
    total: usize,
}

fn resolve_ticket_discussion_budgets(
    planning_meta: &crate::config::PlanningMeta,
) -> TicketDiscussionBudgets {
    TicketDiscussionBudgets {
        prompt_chars: planning_meta
            .linear
            .ticket_context
            .discussion_prompt_chars
            .unwrap_or_else(|| planning_meta.sync.discussion_prompt_char_limit()),
        persisted_chars: planning_meta
            .linear
            .ticket_context
            .discussion_persisted_chars
            .unwrap_or_else(|| planning_meta.sync.discussion_file_char_limit()),
    }
}

/// Launch the interactive sync dashboard using local backlog entries as the selection source.
///
/// Returns an error when planning metadata is missing, backlog discovery fails, or
/// Linear-backed dashboard actions for linked entries cannot be completed.
pub async fn run_sync_dashboard_command(
    client_args: &LinearClientArgs,
    project_override: Option<&str>,
    options: SyncDashboardOptions,
) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "sync")?;
    let entries = discover_backlog_entries(&root)?;
    let LinearCommandContext {
        service,
        default_project_id,
        ..
    } = load_linear_command_context(client_args, None)?;
    let title = sync_dashboard_title(project_override, default_project_id.as_deref());

    match run_sync_dashboard(
        SyncDashboardData {
            title,
            issues: load_sync_dashboard_issues(&service, &entries).await?,
        },
        options,
    )? {
        SyncDashboardExit::Snapshot(snapshot) => println!("{snapshot}"),
        SyncDashboardExit::Cancelled => println!("Sync canceled."),
        SyncDashboardExit::Selected(selection) => match selection.action {
            SyncSelectionAction::Pull => {
                run_sync_pull(
                    client_args,
                    &SyncPullArgs {
                        issue: Some(selection.issue_identifier),
                        all: false,
                    },
                )
                .await?
            }
            SyncSelectionAction::Push => {
                run_sync_push(
                    client_args,
                    &SyncPushArgs {
                        issue: Some(selection.issue_identifier),
                        all: false,
                        update_description: false,
                    },
                )
                .await?
            }
        },
    }

    Ok(())
}

async fn load_sync_dashboard_issues(
    service: &LinearService<ReqwestLinearClient>,
    entries: &[BacklogSyncEntry],
) -> Result<Vec<SyncDashboardIssue>> {
    let mut dashboard_issues = Vec::with_capacity(entries.len());
    for entry in entries {
        let metadata = entry.metadata.as_ref();
        if metadata.is_none() {
            dashboard_issues.push(build_unlinked_sync_dashboard_issue(entry));
            continue;
        }
        let remote_issue = if let Some(metadata) = metadata {
            service.load_issue(&metadata.identifier).await.ok()
        } else {
            None
        };
        let remote_hash = remote_issue
            .as_ref()
            .map(issue_remote_hash)
            .or_else(|| metadata.and_then(|metadata| metadata.remote_hash.clone()));
        let resolution =
            resolve_backlog_sync_status(metadata, entry.local_hash.clone(), remote_hash);
        let issue = match remote_issue {
            Some(issue) => issue,
            None => build_local_dashboard_issue(entry)?,
        };

        dashboard_issues.push(SyncDashboardIssue {
            entry_slug: entry.slug.clone(),
            issue,
            linked_issue_identifier: metadata.map(|metadata| metadata.identifier.clone()),
            local_status: resolution.status,
        });
    }

    Ok(dashboard_issues)
}

fn build_unlinked_sync_dashboard_issue(entry: &BacklogSyncEntry) -> SyncDashboardIssue {
    let slug = entry.slug.clone();
    SyncDashboardIssue {
        entry_slug: slug.clone(),
        issue: IssueSummary {
            id: format!("local-backlog:{slug}"),
            identifier: slug.clone(),
            title: entry.title.clone(),
            description: Some(format!(
                "Local backlog entry under `.metastack/backlog/{slug}`. Link it with `meta backlog sync link <ISSUE> --entry {slug}` to enable pull and push."
            )),
            url: format!(".metastack/backlog/{slug}"),
            priority: None,
            estimate: None,
            updated_at: "local-only".to_string(),
            team: TeamRef {
                id: "local-backlog".to_string(),
                key: "LOCAL".to_string(),
                name: "Local backlog".to_string(),
            },
            project: Some(ProjectRef {
                id: "local-backlog".to_string(),
                name: "Local backlog".to_string(),
            }),
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: Some(WorkflowState {
                id: "local-unlinked".to_string(),
                name: "Unlinked".to_string(),
                kind: None,
            }),
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        },
        linked_issue_identifier: None,
        local_status: BacklogSyncStatus::Unlinked,
    }
}

fn build_local_dashboard_issue(entry: &BacklogSyncEntry) -> Result<IssueSummary> {
    let description = read_optional_text_file(&entry.issue_dir.join(INDEX_FILE_NAME))?;
    let metadata = entry.metadata.as_ref();
    let project = metadata.and_then(|metadata| {
        let name = metadata
            .project_name
            .clone()
            .or_else(|| metadata.project_id.clone())?;
        Some(ProjectRef {
            id: metadata.project_id.clone().unwrap_or_else(|| name.clone()),
            name,
        })
    });
    let team_key = metadata
        .map(|metadata| metadata.team_key.clone())
        .unwrap_or_else(|| "LOCAL".to_string());

    Ok(IssueSummary {
        id: metadata
            .map(|metadata| metadata.issue_id.clone())
            .unwrap_or_else(|| format!("local-{}", entry.slug)),
        identifier: metadata
            .map(|metadata| metadata.identifier.clone())
            .unwrap_or_else(|| entry.slug.clone()),
        title: entry.title.clone(),
        description: (!description.trim().is_empty()).then_some(description),
        url: metadata
            .map(|metadata| metadata.url.clone())
            .unwrap_or_else(|| entry.issue_dir.display().to_string()),
        priority: None,
        estimate: None,
        updated_at: metadata
            .and_then(|metadata| metadata.last_sync_at.clone())
            .unwrap_or_else(|| "-".to_string()),
        team: TeamRef {
            id: format!("team-{team_key}"),
            key: team_key.clone(),
            name: if team_key == "LOCAL" {
                "Local backlog".to_string()
            } else {
                team_key.clone()
            },
        },
        project,
        assignee: None,
        labels: Vec::new(),
        comments: Vec::new(),
        state: None,
        attachments: Vec::new(),
        parent: None,
        children: Vec::new(),
    })
}
/// Link an existing backlog entry to a Linear issue and optionally pull the remote packet.
///
/// Returns an error when planning metadata is missing, the requested issue or backlog entry
/// cannot be resolved, interactive selection is required without a TTY, or metadata cannot be
/// written to disk.
pub async fn run_sync_link(
    client_args: &LinearClientArgs,
    project_override: Option<&str>,
    no_interactive: bool,
    args: &SyncLinkArgs,
) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let planning_meta = load_required_planning_meta(&root, "sync")?;
    let discussion_budgets = resolve_ticket_discussion_budgets(&planning_meta);
    ensure_planning_layout(&root, false)?;
    let entries = discover_backlog_entries(&root)?;
    let LinearCommandContext { service, .. } = load_linear_command_context(client_args, None)?;

    let issue = match &args.issue {
        Some(identifier) => service.load_issue(identifier).await?,
        None => {
            if no_interactive || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
                bail!(
                    "`meta backlog sync link` requires <ISSUE> when `--no-interactive` is used or when the command runs without a TTY"
                );
            }
            let (_service, issues, _) =
                load_sync_project_issues(client_args, project_override).await?;
            let selected_index = prompt_select(
                "Choose a Linear issue to link:",
                &issues
                    .iter()
                    .map(|issue| format!("{}  {}", issue.identifier, issue.title))
                    .collect::<Vec<_>>(),
            )?;
            issues
                .into_iter()
                .nth(selected_index)
                .ok_or_else(|| anyhow!("selected issue index was out of range"))?
        }
    };

    let issue_dir = resolve_link_issue_dir(
        &root,
        &entries,
        &issue,
        args.entry.as_deref(),
        no_interactive,
    )?;

    if let Some(metadata) = load_issue_metadata_if_present(&issue_dir)?
        && metadata.identifier.eq_ignore_ascii_case(&issue.identifier)
    {
        if args.pull {
            let _ = sync_pull_issue(
                &root,
                &service,
                &issue,
                &issue_dir,
                discussion_budgets,
                false,
            )
            .await?;
        } else {
            println!(
                "{} is already linked to {} at {}.",
                display_path(&issue_dir, &root),
                issue.identifier,
                display_path(&issue_dir, &root),
            );
        }
        return Ok(());
    }

    save_issue_metadata(
        &issue_dir,
        &build_issue_metadata(&issue, Vec::new(), None, None, None, Vec::new()),
    )?;

    if args.pull {
        let _ = sync_pull_issue(
            &root,
            &service,
            &issue,
            &issue_dir,
            discussion_budgets,
            false,
        )
        .await?;
    } else {
        println!(
            "Linked {} to {}.",
            display_path(&issue_dir, &root),
            issue.identifier,
        );
    }

    Ok(())
}

/// Show the current sync state for every backlog entry under `.metastack/backlog/`.
///
/// Returns an error when planning metadata is missing, backlog entries cannot be scanned, or
/// `--fetch` is used and Linear issue state cannot be loaded.
pub async fn run_sync_status(client_args: &LinearClientArgs, args: &SyncStatusArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "sync")?;
    let entries = discover_backlog_entries(&root)?;

    if entries.is_empty() {
        println!("No backlog entries found under .metastack/backlog/.");
        return Ok(());
    }

    let maybe_service = if args.fetch {
        Some(load_linear_command_context(client_args, None)?.service)
    } else {
        None
    };

    let mut rows = Vec::new();
    for entry in entries {
        let (identifier, title, status) = if let Some(metadata) = entry.metadata.as_ref() {
            if let Some(service) = maybe_service.as_ref() {
                let issue = service
                    .load_issue(&metadata.identifier)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to fetch sync status for linked backlog entry `{}`",
                            entry.slug
                        )
                    })?;
                let resolution = resolve_backlog_sync_status(
                    entry.metadata.as_ref(),
                    entry.local_hash.clone(),
                    Some(issue_remote_hash(&issue)),
                );
                (
                    issue.identifier,
                    issue.title,
                    resolution.status.as_str().to_string(),
                )
            } else {
                let resolution = resolve_backlog_sync_status(
                    entry.metadata.as_ref(),
                    entry.local_hash.clone(),
                    metadata.remote_hash.clone(),
                );
                (
                    metadata.identifier.clone(),
                    entry.title.clone(),
                    resolution.status.as_str().to_string(),
                )
            }
        } else {
            (
                entry.slug.clone(),
                entry.title.clone(),
                BacklogSyncStatus::Unlinked.as_str().to_string(),
            )
        };

        rows.push(vec![
            identifier,
            title,
            status,
            entry
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.last_sync_at.clone())
                .unwrap_or_else(|| "-".to_string()),
        ]);
    }

    println!(
        "{}",
        render_table(&["Identifier", "Title", "Status", "Last Sync"], &rows)
    );
    Ok(())
}

/// Pull one Linear issue, or every linked backlog entry with `--all`, into local backlog files.
///
/// Returns an error when planning metadata is missing, the requested issue cannot be resolved,
/// local backlog paths cannot be prepared, or overwrite safeguards block the pull.
pub async fn run_sync_pull(client_args: &LinearClientArgs, args: &SyncPullArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let planning_meta = load_required_planning_meta(&root, "sync")?;
    let discussion_budgets = resolve_ticket_discussion_budgets(&planning_meta);
    ensure_planning_layout(&root, false)?;
    let LinearCommandContext { service, .. } = load_linear_command_context(client_args, None)?;

    if args.all {
        return run_sync_pull_all(&root, &service, discussion_budgets).await;
    }

    let issue_identifier = args
        .issue
        .as_deref()
        .ok_or_else(|| anyhow!("missing required issue identifier for sync pull"))?;
    let issue = service.load_issue(issue_identifier).await?;
    let entries = discover_backlog_entries(&root)?;
    let issue_dir = resolve_issue_dir_for_identifier(&root, &entries, &issue.identifier)?;
    ensure_dir(&issue_dir)?;
    let _ = sync_pull_issue(
        &root,
        &service,
        &issue,
        &issue_dir,
        discussion_budgets,
        false,
    )
    .await?;
    Ok(())
}

/// Push managed backlog files for one issue, or every linked backlog entry with `--all`, to Linear.
///
/// Returns an error when planning metadata is missing, the requested issue cannot be resolved,
/// required local files are missing, or the description overwrite safeguards reject the push.
pub async fn run_sync_push(client_args: &LinearClientArgs, args: &SyncPushArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "sync")?;
    ensure_planning_layout(&root, false)?;
    let LinearCommandContext { service, .. } = load_linear_command_context(client_args, None)?;

    if args.all {
        return run_sync_push_all(&root, &service, args.update_description).await;
    }

    let issue_identifier = args
        .issue
        .as_deref()
        .ok_or_else(|| anyhow!("missing required issue identifier for sync push"))?;
    if args.update_description {
        guard_listen_issue_description_sync(issue_identifier)?;
    }
    let issue = service.load_issue(issue_identifier).await?;
    let entries = discover_backlog_entries(&root)?;
    let issue_dir = resolve_issue_dir_for_identifier(&root, &entries, &issue.identifier)?;
    let _ = sync_push_issue(
        &root,
        &service,
        &issue,
        &issue_dir,
        args.update_description,
        false,
    )
    .await?;
    Ok(())
}

/// Push CLI-managed backlog files for a preloaded Linear issue without reloading it from Linear.
///
/// Returns an error when the backlog directory is missing, required managed files cannot be read,
/// or attachment synchronization to Linear fails.
pub(crate) async fn run_sync_push_for_issue(
    root: &Path,
    service: &LinearService<ReqwestLinearClient>,
    issue: &IssueSummary,
    issue_dir: &Path,
) -> Result<()> {
    let _ = sync_push_issue(root, service, issue, issue_dir, false, false).await?;
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

async fn run_sync_pull_all(
    root: &Path,
    service: &LinearService<ReqwestLinearClient>,
    discussion_budgets: TicketDiscussionBudgets,
) -> Result<()> {
    let entries = discover_backlog_entries(root)?;
    let linked_entries = entries
        .into_iter()
        .filter(|entry| entry.metadata.is_some())
        .collect::<Vec<_>>();
    if linked_entries.is_empty() {
        println!("No linked backlog entries found under .metastack/backlog/.");
        return Ok(());
    }

    let mut summary = BatchSyncSummary::default();
    for entry in linked_entries {
        let metadata = entry
            .metadata
            .as_ref()
            .ok_or_else(|| anyhow!("linked backlog entry metadata unexpectedly missing"))?;
        match service.load_issue(&metadata.identifier).await {
            Ok(issue) => match sync_pull_issue(
                root,
                service,
                &issue,
                &entry.issue_dir,
                discussion_budgets,
                true,
            )
            .await
            {
                Ok(SyncExecutionOutcome::Synced) => summary.synced += 1,
                Ok(SyncExecutionOutcome::Skipped) => summary.skipped += 1,
                Err(error) => {
                    summary.errors += 1;
                    eprintln!(
                        "Failed to pull {} from {}: {error:#}",
                        metadata.identifier,
                        display_path(&entry.issue_dir, root),
                    );
                }
            },
            Err(error) => {
                summary.errors += 1;
                eprintln!(
                    "Failed to load {} for {}: {error:#}",
                    metadata.identifier,
                    display_path(&entry.issue_dir, root),
                );
            }
        }
    }

    println!(
        "Pull summary: {} synced, {} skipped, {} errors.",
        summary.synced, summary.skipped, summary.errors
    );

    if summary.errors > 0 {
        bail!(
            "`meta backlog sync pull --all` completed with {} error{}",
            summary.errors,
            plural_suffix(summary.errors),
        );
    }

    Ok(())
}

async fn run_sync_push_all(
    root: &Path,
    service: &LinearService<ReqwestLinearClient>,
    update_description: bool,
) -> Result<()> {
    let entries = discover_backlog_entries(root)?;
    let linked_entries = entries
        .into_iter()
        .filter(|entry| entry.metadata.is_some())
        .collect::<Vec<_>>();
    if linked_entries.is_empty() {
        println!("No linked backlog entries found under .metastack/backlog/.");
        return Ok(());
    }

    let mut summary = BatchSyncSummary::default();
    for entry in linked_entries {
        let metadata = entry
            .metadata
            .as_ref()
            .ok_or_else(|| anyhow!("linked backlog entry metadata unexpectedly missing"))?;
        if update_description
            && let Err(error) = guard_listen_issue_description_sync(&metadata.identifier)
        {
            summary.errors += 1;
            eprintln!(
                "Failed to push {} from {}: {error:#}",
                metadata.identifier,
                display_path(&entry.issue_dir, root),
            );
            continue;
        }

        match service.load_issue(&metadata.identifier).await {
            Ok(issue) => {
                match sync_push_issue(
                    root,
                    service,
                    &issue,
                    &entry.issue_dir,
                    update_description,
                    true,
                )
                .await
                {
                    Ok(SyncExecutionOutcome::Synced) => summary.synced += 1,
                    Ok(SyncExecutionOutcome::Skipped) => summary.skipped += 1,
                    Err(error) => {
                        summary.errors += 1;
                        eprintln!(
                            "Failed to push {} from {}: {error:#}",
                            metadata.identifier,
                            display_path(&entry.issue_dir, root),
                        );
                    }
                }
            }
            Err(error) => {
                summary.errors += 1;
                eprintln!(
                    "Failed to load {} for {}: {error:#}",
                    metadata.identifier,
                    display_path(&entry.issue_dir, root),
                );
            }
        }
    }

    println!(
        "Push summary: {} synced, {} skipped, {} errors.",
        summary.synced, summary.skipped, summary.errors
    );

    if summary.errors > 0 {
        bail!(
            "`meta backlog sync push --all` completed with {} error{}",
            summary.errors,
            plural_suffix(summary.errors),
        );
    }

    Ok(())
}

async fn sync_pull_issue(
    root: &Path,
    service: &LinearService<ReqwestLinearClient>,
    issue: &IssueSummary,
    issue_dir: &Path,
    discussion_budgets: TicketDiscussionBudgets,
    skip_if_synced: bool,
) -> Result<SyncExecutionOutcome> {
    ensure_dir(issue_dir)?;
    let issue_for_pull = issue_with_sync_visible_comments(issue);
    let pulled_comment_ids = issue_for_pull
        .comments
        .iter()
        .map(|comment| comment.id.clone())
        .collect::<Vec<_>>();
    let prepared_context = prepare_issue_context(&issue_for_pull, discussion_budgets);
    let metadata = load_issue_metadata_if_present(issue_dir)?;
    let remote_hash = issue_remote_hash(issue);
    let resolution = resolve_backlog_sync_status(
        metadata.as_ref(),
        compute_local_sync_hash(issue_dir)?,
        Some(remote_hash.clone()),
    );

    if skip_if_synced
        && resolution.status == BacklogSyncStatus::Synced
        && metadata
            .as_ref()
            .map(|metadata| metadata.last_pulled_comment_ids.as_slice())
            == Some(pulled_comment_ids.as_slice())
    {
        return Ok(SyncExecutionOutcome::Skipped);
    }

    if needs_pull_overwrite_confirmation(resolution.status) {
        let local_description = read_optional_text_file(&issue_dir.join(INDEX_FILE_NAME))?;
        let diff = render_sync_diff(
            &local_description,
            prepared_context
                .issue
                .description
                .as_deref()
                .unwrap_or_default(),
        );

        if io::stdin().is_terminal() && io::stdout().is_terminal() {
            if !prompt_pull_overwrite(&issue.identifier, resolution.status, &diff)? {
                println!(
                    "Canceled pull for {}. Local backlog files and hash baselines were left unchanged.",
                    issue.identifier
                );
                return Ok(SyncExecutionOutcome::Skipped);
            }
        } else {
            bail!(
                "`meta backlog sync pull {}` refused to overwrite local backlog content because the sync state is `{}`; rerun in a TTY to review the diff and confirm the overwrite",
                issue.identifier,
                resolution.status.as_str(),
            );
        }
    }

    write_issue_index_file(
        issue_dir,
        prepared_context
            .issue
            .description
            .as_deref()
            .unwrap_or_default(),
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
        write_issue_attachment_file(issue_dir, &relative_path, &contents)?;
        managed_files.push(ManagedFileRecord {
            path: relative_path,
            attachment_id: Some(attachment.id.clone()),
            url: Some(attachment.url.clone()),
        });
    }

    let download_failures =
        materialize_issue_context(service, issue_dir, &prepared_context).await?;
    for failure in download_failures {
        eprintln!(
            "warning: failed to localize ticket image for {}: {} from {} ({})",
            issue.identifier, failure.filename, failure.source_label, failure.error
        );
    }
    let local_hash = compute_local_sync_hash(issue_dir)?.ok_or_else(|| {
        anyhow!(
            "backlog issue directory `{}` disappeared during sync",
            issue_dir.display()
        )
    })?;
    save_issue_metadata(
        issue_dir,
        &build_issue_metadata(
            issue,
            managed_files,
            Some(local_hash),
            Some(remote_hash),
            Some(sync_timestamp()),
            pulled_comment_ids,
        ),
    )?;

    println!(
        "Pulled {} into {} (restored {} managed attachment file{}; rebuilt discussion context with {} comment{} and {} image{}).",
        issue.identifier,
        display_path(issue_dir, root),
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
        issue_for_pull.comments.len(),
        plural_suffix(issue_for_pull.comments.len()),
        prepared_context.images.len(),
        plural_suffix(prepared_context.images.len()),
    );

    Ok(SyncExecutionOutcome::Synced)
}

async fn sync_push_issue(
    root: &Path,
    service: &LinearService<ReqwestLinearClient>,
    issue: &IssueSummary,
    issue_dir: &Path,
    update_description: bool,
    skip_if_synced: bool,
) -> Result<SyncExecutionOutcome> {
    if !issue_dir.is_dir() {
        bail!(
            "backlog item `{}` was not found at `{}`; run `meta backlog sync pull {}` first",
            issue.identifier,
            issue_dir.display(),
            issue.identifier
        );
    }

    let index_path = issue_dir.join(INDEX_FILE_NAME);
    let description = fs::read_to_string(&index_path).with_context(|| {
        format!(
            "failed to read `{}`; `meta backlog sync push` requires `{}`",
            index_path.display(),
            INDEX_FILE_NAME
        )
    })?;
    let metadata = load_issue_metadata_if_present(issue_dir)?;
    let local_hash = compute_local_sync_hash(issue_dir)?.ok_or_else(|| {
        anyhow!(
            "backlog issue directory `{}` disappeared before push",
            issue_dir.display()
        )
    })?;
    let resolution = resolve_backlog_sync_status(
        metadata.as_ref(),
        Some(local_hash.clone()),
        Some(issue_remote_hash(issue)),
    );

    if skip_if_synced && resolution.status == BacklogSyncStatus::Synced {
        return Ok(SyncExecutionOutcome::Skipped);
    }

    if update_description
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
    if update_description {
        service
            .edit_issue(IssueEditSpec {
                identifier: issue.identifier.clone(),
                title: None,
                description: Some(description.clone()),
                project: None,
                state: None,
                priority: None,
                estimate: None,
                labels: None,
                parent_identifier: None,
            })
            .await?;
    }

    let local_files = collect_remote_managed_sync_files(issue_dir)?
        .into_iter()
        .filter(|file| file.relative_path != INDEX_FILE_NAME)
        .collect::<Vec<_>>();
    let local_paths = local_files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect::<BTreeSet<_>>();
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

    let checklist_path = issue_dir.join("checklist.md");
    let progress_comment_status = if checklist_path.is_file() {
        let checklist_contents = fs::read_to_string(&checklist_path)
            .with_context(|| format!("failed to read `{}`", checklist_path.display()))?;
        let progress = parse_checklist_progress_summary(&checklist_contents);
        service
            .upsert_comment_with_marker(
                issue,
                HARNESS_SYNC_MARKER,
                render_harness_sync_comment(issue, &progress),
            )
            .await?;
        Some("updated [harness-sync] progress comment")
    } else {
        None
    };

    let remote_description = if update_description {
        description
    } else {
        issue.description.clone().unwrap_or_default()
    };
    let remote_hash = compute_remote_sync_hash(&remote_description, &managed_files);
    let mut updated_metadata = build_issue_metadata(
        issue,
        managed_files,
        Some(local_hash),
        Some(remote_hash),
        Some(sync_timestamp()),
        metadata
            .as_ref()
            .map(|metadata| metadata.last_pulled_comment_ids.clone())
            .unwrap_or_default(),
    );
    if updated_metadata.parent_id.is_none() {
        updated_metadata.parent_id = metadata
            .as_ref()
            .and_then(|metadata| metadata.parent_id.clone());
    }
    if updated_metadata.parent_identifier.is_none() {
        updated_metadata.parent_identifier = metadata
            .as_ref()
            .and_then(|metadata| metadata.parent_identifier.clone());
    }
    save_issue_metadata(issue_dir, &updated_metadata)?;

    println!(
        "Pushed {} from {} (synced {} managed attachment file{}; {}; {}).",
        issue.identifier,
        display_path(issue_dir, root),
        local_path_count,
        plural_suffix(local_path_count),
        if update_description {
            "updated Linear issue description from index.md"
        } else {
            "left the Linear issue description unchanged; pass --update-description to send index.md"
        },
        progress_comment_status
            .unwrap_or("skipped [harness-sync] progress comment because checklist.md is missing"),
    );

    Ok(SyncExecutionOutcome::Synced)
}

fn build_issue_metadata(
    issue: &IssueSummary,
    managed_files: Vec<ManagedFileRecord>,
    local_hash: Option<String>,
    remote_hash: Option<String>,
    last_sync_at: Option<String>,
    last_pulled_comment_ids: Vec<String>,
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
        last_sync_at,
        last_pulled_comment_ids,
        managed_files,
    }
}

fn normalized_issue_description(issue: &IssueSummary) -> &str {
    issue.description.as_deref().unwrap_or_default()
}

fn issue_with_sync_visible_comments(issue: &IssueSummary) -> IssueSummary {
    let mut filtered = issue.clone();
    filtered
        .comments
        .retain(|comment| !is_generated_sync_comment(comment.body.as_str()));
    filtered.comments.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    filtered
}

fn is_generated_sync_comment(body: &str) -> bool {
    body.contains("## Codex Workpad") || body.contains(HARNESS_SYNC_MARKER)
}

fn parse_checklist_progress_summary(contents: &str) -> ChecklistProgressSummary {
    let mut milestones = Vec::new();
    let mut current_title = "Checklist".to_string();
    let mut current_completed = 0usize;
    let mut current_total = 0usize;

    for line in contents.lines() {
        let trimmed = line.trim_start();
        if let Some(title) = checklist_heading_title(trimmed) {
            if current_total > 0 {
                milestones.push(ChecklistMilestoneProgress {
                    title: current_title,
                    completed: current_completed,
                    total: current_total,
                });
            }
            current_title = title.trim().to_string();
            current_completed = 0;
            current_total = 0;
            continue;
        }

        if trimmed.starts_with("- [x] ")
            || trimmed.starts_with("- [X] ")
            || trimmed.starts_with("* [x] ")
            || trimmed.starts_with("* [X] ")
        {
            current_completed += 1;
            current_total += 1;
        } else if trimmed.starts_with("- [ ] ") || trimmed.starts_with("* [ ] ") {
            current_total += 1;
        }
    }

    if current_total > 0 {
        milestones.push(ChecklistMilestoneProgress {
            title: current_title,
            completed: current_completed,
            total: current_total,
        });
    }

    ChecklistProgressSummary {
        completed: milestones.iter().map(|milestone| milestone.completed).sum(),
        total: milestones.iter().map(|milestone| milestone.total).sum(),
        milestones,
    }
}

fn checklist_heading_title(line: &str) -> Option<&str> {
    let heading_level = line
        .as_bytes()
        .iter()
        .take_while(|byte| **byte == b'#')
        .count();
    if heading_level < 2 {
        return None;
    }

    let remainder = line[heading_level..].trim_start();
    if remainder.is_empty() {
        return None;
    }

    Some(remainder.trim())
}

fn render_harness_sync_comment(
    issue: &IssueSummary,
    progress: &ChecklistProgressSummary,
) -> String {
    let mut lines = vec![
        HARNESS_SYNC_MARKER.to_string(),
        format!("Issue: `{}`", issue.identifier),
        format!("Updated: {}", sync_timestamp()),
        String::new(),
    ];
    if progress.milestones.is_empty() {
        lines.push("No checklist milestones were found in `checklist.md`.".to_string());
    } else {
        for milestone in &progress.milestones {
            lines.push(format!(
                "- {} -- {}/{} complete",
                milestone.title, milestone.completed, milestone.total
            ));
        }
        lines.push(String::new());
        lines.push(format!(
            "Overall: {} ({}/{})",
            completion_percentage(progress.completed, progress.total),
            progress.completed,
            progress.total
        ));
    }
    lines.join("\n")
}

fn completion_percentage(completed: usize, total: usize) -> String {
    if total == 0 {
        return "0%".to_string();
    }
    format!("{}%", (completed * 100) / total)
}

async fn load_sync_project_issues(
    client_args: &LinearClientArgs,
    project_override: Option<&str>,
) -> Result<(
    LinearService<ReqwestLinearClient>,
    Vec<IssueSummary>,
    String,
)> {
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
            sync_dashboard_title(Some(project_name), default_project_id.as_deref()),
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
            sync_dashboard_title(None, Some(project_id.as_str())),
        )
    };

    let issues = service.list_issues(filter).await?;
    Ok((service, issues, title))
}

fn sync_dashboard_title(
    project_override: Option<&str>,
    default_project_id: Option<&str>,
) -> String {
    if let Some(project_name) = project_override {
        return format!("meta backlog sync ({project_name})");
    }

    if let Some(project_id) = default_project_id {
        format!("meta backlog sync ({project_id})")
    } else {
        "meta backlog sync".to_string()
    }
}

fn discover_backlog_entries(root: &Path) -> Result<Vec<BacklogSyncEntry>> {
    let paths = PlanningPaths::new(root);
    if !paths.backlog_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut entries = fs::read_dir(&paths.backlog_dir)
        .with_context(|| format!("failed to read `{}`", paths.backlog_dir.display()))?
        .map(|entry| entry.map_err(anyhow::Error::from))
        .map(|entry| -> Result<Option<BacklogSyncEntry>> {
            let entry = entry
                .with_context(|| format!("failed to traverse `{}`", paths.backlog_dir.display()))?;
            if !entry
                .file_type()
                .with_context(|| format!("failed to read `{}`", entry.path().display()))?
                .is_dir()
            {
                return Ok(None);
            }

            let slug = entry.file_name().to_string_lossy().to_string();
            if slug == "_TEMPLATE" {
                return Ok(None);
            }

            let issue_dir = entry.path();
            let metadata = load_issue_metadata_if_present(&issue_dir)?;
            let title = metadata
                .as_ref()
                .map(|metadata| metadata.title.trim().to_string())
                .filter(|title| !title.is_empty())
                .or_else(|| read_entry_title(&issue_dir).ok().flatten())
                .unwrap_or_else(|| slug.clone());

            Ok(Some(BacklogSyncEntry {
                slug,
                local_hash: compute_local_sync_hash(&issue_dir)?,
                issue_dir,
                metadata,
                title,
            }))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| left.slug.cmp(&right.slug));
    Ok(entries)
}

fn resolve_link_issue_dir(
    root: &Path,
    entries: &[BacklogSyncEntry],
    issue: &IssueSummary,
    entry_slug: Option<&str>,
    no_interactive: bool,
) -> Result<PathBuf> {
    if let Some(existing) = linked_entry_for_issue(entries, &issue.identifier)? {
        if let Some(entry_slug) = entry_slug
            && existing.slug != entry_slug
        {
            bail!(
                "Linear issue `{}` is already linked to `{}`",
                issue.identifier,
                display_path(&existing.issue_dir, root),
            );
        }
        return Ok(existing.issue_dir.clone());
    }

    let target = if let Some(entry_slug) = entry_slug {
        resolve_entry_by_slug(root, entries, entry_slug)?
    } else {
        if no_interactive || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            bail!(
                "`meta backlog sync link {}` requires `--entry <SLUG>` when `--no-interactive` is used or when the command runs without a TTY",
                issue.identifier,
            );
        }
        let unlinked = entries
            .iter()
            .filter(|entry| entry.metadata.is_none())
            .collect::<Vec<_>>();
        if unlinked.is_empty() {
            bail!("no unlinked backlog entries are available to link");
        }

        let selected_index = prompt_select(
            "Choose a backlog entry to link:",
            &unlinked
                .iter()
                .map(|entry| format!("{}  {}", entry.slug, entry.title))
                .collect::<Vec<_>>(),
        )?;
        unlinked
            .get(selected_index)
            .ok_or_else(|| anyhow!("selected backlog entry index was out of range"))?
            .issue_dir
            .clone()
    };

    if let Some(metadata) = load_issue_metadata_if_present(&target)?
        && !metadata.identifier.eq_ignore_ascii_case(&issue.identifier)
    {
        bail!(
            "backlog entry `{}` is already linked to `{}`",
            display_path(&target, root),
            metadata.identifier,
        );
    }

    Ok(target)
}

fn resolve_issue_dir_for_identifier(
    root: &Path,
    entries: &[BacklogSyncEntry],
    identifier: &str,
) -> Result<PathBuf> {
    if let Some(entry) = linked_entry_for_issue(entries, identifier)? {
        return Ok(entry.issue_dir.clone());
    }

    Ok(backlog_issue_dir(root, identifier))
}

fn linked_entry_for_issue<'a>(
    entries: &'a [BacklogSyncEntry],
    identifier: &str,
) -> Result<Option<&'a BacklogSyncEntry>> {
    let matches = entries
        .iter()
        .filter(|entry| {
            entry
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.identifier.eq_ignore_ascii_case(identifier))
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => Ok(None),
        [entry] => Ok(Some(*entry)),
        _ => bail!(
            "multiple backlog entries are linked to `{identifier}`: {}",
            matches
                .iter()
                .map(|entry| entry.slug.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ),
    }
}

fn resolve_entry_by_slug(root: &Path, entries: &[BacklogSyncEntry], slug: &str) -> Result<PathBuf> {
    let entry = entries
        .iter()
        .find(|entry| entry.slug == slug)
        .ok_or_else(|| {
            anyhow!("backlog entry `{slug}` was not found under `.metastack/backlog/`")
        })?;
    if !entry
        .issue_dir
        .starts_with(PlanningPaths::new(root).backlog_dir)
    {
        bail!("refusing to use backlog entry outside `.metastack/backlog/`");
    }
    Ok(entry.issue_dir.clone())
}

fn read_entry_title(issue_dir: &Path) -> Result<Option<String>> {
    let index_path = issue_dir.join(INDEX_FILE_NAME);
    if !index_path.is_file() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&index_path)
        .with_context(|| format!("failed to read `{}`", index_path.display()))?;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(stripped) = trimmed.strip_prefix('#') {
            let heading = stripped.trim();
            if !heading.is_empty() {
                return Ok(Some(heading.to_string()));
            }
        }
        return Ok(Some(trimmed.to_string()));
    }

    Ok(None)
}

fn write_issue_index_file(issue_dir: &Path, contents: &str) -> Result<()> {
    let path = issue_dir.join(INDEX_FILE_NAME);
    let _ = write_text_file(&path, contents, true)?;
    Ok(())
}

fn sync_timestamp() -> String {
    Utc::now().to_rfc3339()
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
        normalized_issue_description(issue),
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

fn prompt_select(prompt: &str, options: &[String]) -> Result<usize> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    prompt_select_with_io(prompt, options, &mut reader, &mut writer)
}

fn prompt_select_with_io(
    prompt: &str,
    options: &[String],
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<usize> {
    if options.is_empty() {
        bail!("interactive selection requires at least one option");
    }

    writeln!(writer, "{prompt}")?;
    for (index, option) in options.iter().enumerate() {
        writeln!(writer, "  {}. {option}", index + 1)?;
    }
    writeln!(writer, "Enter a number:")?;
    writer.flush()?;

    let mut input = String::new();
    loop {
        input.clear();
        reader.read_line(&mut input)?;
        let trimmed = input.trim();
        if let Ok(choice) = trimmed.parse::<usize>()
            && (1..=options.len()).contains(&choice)
        {
            return Ok(choice - 1);
        }
        writeln!(writer, "Enter a number between 1 and {}:", options.len())?;
        writer.flush()?;
    }
}

fn read_optional_text_file(path: &Path) -> Result<String> {
    if !path.is_file() {
        return Ok(String::new());
    }

    fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))
}

fn render_sync_diff(local_contents: &str, remote_contents: &str) -> String {
    render_text_diff(
        "local/index.md",
        "linear/description",
        local_contents,
        remote_contents,
    )
}

fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.len());
        }
    }

    let mut lines = Vec::new();
    lines.push(render_row(
        &headers
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>(),
        &widths,
    ));
    lines.push(render_row(
        &widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>(),
        &widths,
    ));
    for row in rows {
        lines.push(render_row(row, &widths));
    }

    lines.join("\n")
}

fn render_row(row: &[String], widths: &[usize]) -> String {
    row.iter()
        .enumerate()
        .map(|(index, value)| format!("{value:width$}", width = widths[index]))
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
mod tests {
    use super::{prompt_pull_overwrite_with_io, prompt_select_with_io};
    use crate::backlog::BacklogSyncStatus;
    use anyhow::Result;
    use std::io::Cursor;

    #[test]
    fn prompt_select_retries_until_it_gets_a_valid_choice() -> Result<()> {
        let mut reader = Cursor::new("0\n2\n");
        let mut writer = Vec::new();
        let options = vec!["alpha".to_string(), "beta".to_string()];

        let choice = prompt_select_with_io("Choose:", &options, &mut reader, &mut writer)?;

        assert_eq!(choice, 1);
        let output = String::from_utf8(writer)?;
        assert!(output.contains("Enter a number between 1 and 2:"));
        Ok(())
    }

    #[test]
    fn prompt_pull_overwrite_accepts_cancel() -> Result<()> {
        let mut reader = Cursor::new("cancel\n");
        let mut writer = Vec::new();

        let accepted = prompt_pull_overwrite_with_io(
            "MET-35",
            BacklogSyncStatus::RemoteAhead,
            "diff",
            &mut reader,
            &mut writer,
        )?;

        assert!(!accepted);
        Ok(())
    }
}

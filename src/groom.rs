use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use time::OffsetDateTime;

use crate::backlog::{
    BacklogIssueMetadata, BacklogSyncStatus, ManagedFileRecord, compute_local_sync_hash,
    compute_remote_sync_hash, ensure_no_unresolved_placeholders, load_issue_metadata,
    render_template_files, resolve_backlog_sync_status, save_issue_metadata,
    write_issue_description,
};
use crate::cli::GroomArgs;
use crate::config::load_required_planning_meta;
use crate::fs::{
    FileWriteStatus, PlanningPaths, canonicalize_existing_dir, display_path, ensure_dir,
    write_text_file,
};
use crate::linear::{
    IssueListFilters, IssueSummary, LinearService, ReqwestLinearClient, WorkflowState,
};
use crate::load_linear_command_context;
use crate::scaffold::ensure_planning_layout;

const REFINE_LABEL: &str = "backlog-refine";
const MERGE_LABEL: &str = "backlog-merge";
const SPLIT_LABEL: &str = "backlog-split";
const RESCAN_LABEL: &str = "backlog-rescan";
const ARCHIVE_STATE_CANDIDATES: &[&str] = &["Archive", "Archived", "Canceled", "Cancelled"];
const STALE_ARCHIVE_DAYS: i64 = 90;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum GroomCategory {
    Archive,
    Merge,
    Refine,
    RescanRequired,
    Split,
}

impl GroomCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Merge => "merge",
            Self::Refine => "refine",
            Self::RescanRequired => "rescan-required",
            Self::Split => "split",
        }
    }

    fn label_name(self) -> Option<&'static str> {
        match self {
            Self::Archive => None,
            Self::Merge => Some(MERGE_LABEL),
            Self::Refine => Some(REFINE_LABEL),
            Self::RescanRequired => Some(RESCAN_LABEL),
            Self::Split => Some(SPLIT_LABEL),
        }
    }
}

#[derive(Debug, Clone)]
struct GroomFinding {
    category: GroomCategory,
    summary: String,
}

#[derive(Debug, Clone)]
struct IssueGroomResult {
    issue: IssueSummary,
    findings: Vec<GroomFinding>,
    packet_created: bool,
}

#[derive(Debug, Clone)]
struct MutationRecord {
    issue_identifier: String,
    fields: Vec<String>,
}

/// Run repo-scoped backlog grooming for the active repository.
///
/// Returns an error when repo planning metadata cannot be loaded, Linear issues cannot be listed,
/// local backlog packets cannot be prepared, or apply-mode Linear mutations fail.
pub async fn run_backlog_groom(args: &GroomArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.client.root)?;
    let planning_meta = load_required_planning_meta(&root, "backlog groom")?;
    ensure_planning_layout(&root, false)?;

    let command_context = load_linear_command_context(&args.client, None)?;
    let issues = command_context
        .service
        .list_issues(IssueListFilters {
            team: command_context
                .default_team
                .clone()
                .or(planning_meta.linear.team.clone()),
            project_id: command_context.default_project_id.clone(),
            limit: args.limit,
            ..IssueListFilters::default()
        })
        .await
        .context("failed to load repo-scoped backlog issues from Linear")?;

    if issues.is_empty() {
        println!("Backlog grooming report\n\nNo repo-scoped backlog issues found.");
        return Ok(());
    }

    let duplicate_map = duplicate_theme_map(&issues);
    let mut results = Vec::with_capacity(issues.len());
    let mut created_packets = 0usize;

    for issue in issues {
        let packet_created = ensure_issue_packet(&root, &issue)?;
        if packet_created {
            created_packets += 1;
        }
        let findings = analyze_issue(&root, &issue, duplicate_map.get(&issue.identifier));
        results.push(IssueGroomResult {
            issue,
            findings,
            packet_created,
        });
    }

    let mutations = if args.apply {
        apply_groom_actions(&command_context.service, &root, &results).await?
    } else {
        Vec::new()
    };

    println!(
        "{}",
        render_groom_report(&root, &results, created_packets, &mutations, args.apply)
    );
    Ok(())
}

fn analyze_issue(
    root: &Path,
    issue: &IssueSummary,
    duplicate_titles: Option<&Vec<String>>,
) -> Vec<GroomFinding> {
    let mut findings = Vec::new();
    let description = issue.description.as_deref().unwrap_or_default();
    let issue_dir = PlanningPaths::new(root).backlog_issue_dir(&issue.identifier);
    let metadata = load_issue_metadata(&issue_dir).ok();
    let local_hash = compute_local_sync_hash(&issue_dir).ok().flatten();
    let remote_hash = Some(compute_remote_sync_hash(
        description,
        &managed_files_from(&metadata),
    ));
    let sync_resolution = resolve_backlog_sync_status(metadata.as_ref(), local_hash, remote_hash);
    let linear_updated_at = parse_rfc3339(&issue.updated_at).ok();
    let packet_updated_at = latest_packet_update(&issue_dir);
    let scan_updated_at = latest_scan_update(root);

    if missing_acceptance_criteria(description) || description_is_weak(description) {
        findings.push(GroomFinding {
            category: GroomCategory::Refine,
            summary: "description is missing acceptance criteria or enough execution detail"
                .to_string(),
        });
    }

    if ticket_needs_split(issue, description) {
        findings.push(GroomFinding {
            category: GroomCategory::Split,
            summary: "scope looks broad enough to split into smaller tickets".to_string(),
        });
    }

    if let Some(duplicates) = duplicate_titles
        && !duplicates.is_empty()
    {
        findings.push(GroomFinding {
            category: GroomCategory::Merge,
            summary: format!("theme overlaps with {}", duplicates.join(", ")),
        });
    }

    if needs_archive(issue, description) {
        findings.push(GroomFinding {
            category: GroomCategory::Archive,
            summary: format!(
                "ticket is stale and low-signal (>{STALE_ARCHIVE_DAYS} days old in backlog/todo)"
            ),
        });
    }

    if sync_resolution.status != BacklogSyncStatus::Synced {
        findings.push(GroomFinding {
            category: GroomCategory::RescanRequired,
            summary: format!(
                "local packet is {} relative to Linear",
                sync_resolution.status.as_str()
            ),
        });
    }

    if let (Some(packet_updated_at), Some(linear_updated_at)) =
        (packet_updated_at, linear_updated_at)
        && packet_updated_at < linear_updated_at
    {
        findings.push(GroomFinding {
            category: GroomCategory::RescanRequired,
            summary: "local packet is older than the Linear issue update timestamp".to_string(),
        });
    }

    if let (Some(scan_updated_at), Some(linear_updated_at)) = (scan_updated_at, linear_updated_at)
        && scan_updated_at < linear_updated_at
    {
        findings.push(GroomFinding {
            category: GroomCategory::RescanRequired,
            summary: "repo scan context predates the latest Linear issue update".to_string(),
        });
    }

    dedupe_findings(findings)
}

async fn apply_groom_actions(
    service: &LinearService<ReqwestLinearClient>,
    root: &Path,
    results: &[IssueGroomResult],
) -> Result<Vec<MutationRecord>> {
    let mut mutations = Vec::new();

    for result in results {
        let issue_dir = PlanningPaths::new(root).backlog_issue_dir(&result.issue.identifier);
        let local_index = fs::read_to_string(issue_dir.join("index.md")).ok();
        let labels_to_add = result
            .findings
            .iter()
            .filter_map(|finding| finding.category.label_name().map(ToString::to_string))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        let description_update = if result
            .findings
            .iter()
            .any(|finding| finding.category == GroomCategory::Refine)
        {
            local_index.as_ref().and_then(|contents| {
                let remote = result.issue.description.as_deref().unwrap_or_default();
                (contents.trim() != remote.trim()).then(|| contents.clone())
            })
        } else {
            None
        };

        let state_update = if result
            .findings
            .iter()
            .any(|finding| finding.category == GroomCategory::Archive)
        {
            let context = service
                .load_issue_edit_context(&result.issue.identifier)
                .await?;
            choose_archive_state_name(&context.team.states).map(ToString::to_string)
        } else {
            None
        };

        if description_update.is_none() && state_update.is_none() && labels_to_add.is_empty() {
            continue;
        }

        let mut fields = Vec::new();
        if description_update.is_some() {
            fields.push("description".to_string());
        }
        if !labels_to_add.is_empty() {
            fields.push("labels".to_string());
        }
        if state_update.is_some() {
            fields.push("state".to_string());
        }

        service
            .update_issue_fields(
                &result.issue.identifier,
                description_update,
                state_update,
                &labels_to_add,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to apply grooming mutations for `{}`",
                    result.issue.identifier
                )
            })?;

        if fields.iter().any(|field| field == "description")
            && let Some(local_index) = local_index.as_deref()
        {
            let _ = write_issue_description(root, &result.issue.identifier, local_index)?;
        }

        mutations.push(MutationRecord {
            issue_identifier: result.issue.identifier.clone(),
            fields,
        });
    }

    Ok(mutations)
}

fn render_groom_report(
    root: &Path,
    results: &[IssueGroomResult],
    created_packets: usize,
    mutations: &[MutationRecord],
    apply_mode: bool,
) -> String {
    let mut lines = vec![
        "Backlog grooming report".to_string(),
        String::new(),
        format!(
            "Scanned {} issue(s); created {} missing local packet(s); mode: {}",
            results.len(),
            created_packets,
            if apply_mode { "apply" } else { "report" }
        ),
    ];

    for result in results {
        lines.push(String::new());
        lines.push(format!(
            "{}  {}",
            result.issue.identifier, result.issue.title
        ));
        if result.packet_created {
            lines.push(format!(
                "  created local packet at {}",
                display_path(
                    &PlanningPaths::new(root).backlog_issue_dir(&result.issue.identifier),
                    root
                )
            ));
        }
        if result.findings.is_empty() {
            lines.push("  no grooming findings".to_string());
            continue;
        }
        for finding in &result.findings {
            lines.push(format!(
                "  [{}] {}",
                finding.category.as_str(),
                finding.summary
            ));
        }
    }

    lines.push(String::new());
    lines.push("Mutations".to_string());
    if mutations.is_empty() {
        lines.push("  none".to_string());
    } else {
        for mutation in mutations {
            lines.push(format!(
                "  {}: {}",
                mutation.issue_identifier,
                mutation.fields.join(", ")
            ));
        }
    }

    lines.join("\n")
}

fn ensure_issue_packet(root: &Path, issue: &IssueSummary) -> Result<bool> {
    let paths = PlanningPaths::new(root);
    let issue_dir = paths.backlog_issue_dir(&issue.identifier);
    let mut created = ensure_dir(&issue_dir)?;
    let rendered = render_template_files(
        root,
        &crate::backlog::TemplateContext {
            issue_identifier: Some(issue.identifier.clone()),
            issue_title: Some(issue.title.clone()),
            issue_url: Some(issue.url.clone()),
            parent_identifier: issue
                .parent
                .as_ref()
                .map(|parent| parent.identifier.clone()),
            parent_title: issue.parent.as_ref().map(|parent| parent.title.clone()),
            parent_url: issue.parent.as_ref().map(|parent| parent.url.clone()),
            parent_description: issue.description.clone(),
            ..crate::backlog::TemplateContext::default()
        },
    )?;
    ensure_no_unresolved_placeholders(&rendered)?;

    for file in &rendered {
        let status = write_text_file(&issue_dir.join(&file.relative_path), &file.contents, false)?;
        created |= matches!(status, FileWriteStatus::Created);
    }

    save_issue_metadata(&issue_dir, &build_issue_metadata(issue))?;
    Ok(created)
}

fn build_issue_metadata(issue: &IssueSummary) -> BacklogIssueMetadata {
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
        local_hash: None,
        remote_hash: None,
        last_sync_at: None,
        managed_files: Vec::<ManagedFileRecord>::new(),
    }
}

fn duplicate_theme_map(issues: &[IssueSummary]) -> BTreeMap<String, Vec<String>> {
    let mut duplicates = BTreeMap::<String, Vec<String>>::new();
    for (index, left) in issues.iter().enumerate() {
        let left_tokens = title_tokens(&left.title);
        if left_tokens.is_empty() {
            continue;
        }
        for right in issues.iter().skip(index + 1) {
            let right_tokens = title_tokens(&right.title);
            if right_tokens.is_empty() {
                continue;
            }
            if jaccard_similarity(&left_tokens, &right_tokens) >= 0.6 {
                duplicates
                    .entry(left.identifier.clone())
                    .or_default()
                    .push(right.identifier.clone());
                duplicates
                    .entry(right.identifier.clone())
                    .or_default()
                    .push(left.identifier.clone());
            }
        }
    }
    duplicates
}

fn title_tokens(value: &str) -> BTreeSet<String> {
    value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter_map(|token| {
            let token = token.trim().to_ascii_lowercase();
            (!token.is_empty()
                && !matches!(
                    token.as_str(),
                    "the" | "a" | "an" | "and" | "for" | "with" | "meta" | "backlog"
                ))
            .then_some(token)
        })
        .collect()
}

fn jaccard_similarity(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f32 {
    let intersection = left.intersection(right).count() as f32;
    let union = left.union(right).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn missing_acceptance_criteria(description: &str) -> bool {
    let lower = description.to_ascii_lowercase();
    let has_heading = lower.contains("acceptance criteria");
    let checklist_items = description
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("- [ ]")
                || trimmed.starts_with("- [x]")
                || trimmed.starts_with("* ")
                || trimmed.starts_with("- ")
        })
        .count();
    !has_heading && checklist_items < 2
}

fn description_is_weak(description: &str) -> bool {
    let non_empty_lines = description
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let heading_count = description
        .lines()
        .filter(|line| line.trim_start().starts_with('#'))
        .count();
    description.trim().len() < 120 || non_empty_lines < 4 || heading_count == 0
}

fn ticket_needs_split(issue: &IssueSummary, description: &str) -> bool {
    let title_word_count = issue.title.split_whitespace().count();
    let checklist_items = description
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            trimmed.starts_with("- [ ]") || trimmed.starts_with("- ")
        })
        .count();
    (issue.title.contains(" and ") || issue.title.contains('/'))
        && (title_word_count >= 8 || description.len() >= 900 || checklist_items >= 6)
}

fn needs_archive(issue: &IssueSummary, description: &str) -> bool {
    let Some(updated_at) = parse_rfc3339(&issue.updated_at).ok() else {
        return false;
    };
    let age_days = (OffsetDateTime::now_utc() - updated_at).whole_days();
    let state_kind = issue
        .state
        .as_ref()
        .and_then(|state| state.kind.as_deref())
        .unwrap_or_default();

    age_days >= STALE_ARCHIVE_DAYS
        && matches!(state_kind, "backlog" | "unstarted")
        && description_is_weak(description)
}

fn latest_packet_update(issue_dir: &Path) -> Option<OffsetDateTime> {
    let mut latest = None;
    let entries = fs::read_dir(issue_dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        let metadata = fs::metadata(&path).ok()?;
        let modified = OffsetDateTime::from(metadata.modified().ok()?);
        latest = Some(latest.map_or(modified, |current: OffsetDateTime| current.max(modified)));
    }
    latest
}

fn latest_scan_update(root: &Path) -> Option<OffsetDateTime> {
    let modified = fs::metadata(PlanningPaths::new(root).scan_path())
        .ok()?
        .modified()
        .ok()?;
    Some(OffsetDateTime::from(modified))
}

fn parse_rfc3339(value: &str) -> Result<OffsetDateTime> {
    let parsed =
        DateTime::parse_from_rfc3339(value).context("failed to parse RFC3339 timestamp")?;
    OffsetDateTime::from_unix_timestamp(parsed.with_timezone(&Utc).timestamp())
        .context("failed to convert parsed timestamp")
}

fn managed_files_from(metadata: &Option<BacklogIssueMetadata>) -> Vec<ManagedFileRecord> {
    metadata
        .as_ref()
        .map(|metadata| metadata.managed_files.clone())
        .unwrap_or_default()
}

fn choose_archive_state_name(states: &[WorkflowState]) -> Option<&str> {
    ARCHIVE_STATE_CANDIDATES
        .iter()
        .find(|candidate| {
            states
                .iter()
                .any(|state| state.name.eq_ignore_ascii_case(candidate))
        })
        .copied()
}

fn dedupe_findings(findings: Vec<GroomFinding>) -> Vec<GroomFinding> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for finding in findings {
        let key = format!("{}:{}", finding.category.as_str(), finding.summary);
        if seen.insert(key) {
            deduped.push(finding);
        }
    }
    deduped.sort_by(|left, right| {
        left.category
            .cmp(&right.category)
            .then_with(|| left.summary.cmp(&right.summary))
    });
    deduped
}

#[cfg(test)]
mod tests {
    use super::{description_is_weak, duplicate_theme_map, missing_acceptance_criteria};
    use crate::linear::{IssueSummary, ProjectRef, TeamRef, WorkflowState};

    fn issue(identifier: &str, title: &str) -> IssueSummary {
        IssueSummary {
            id: format!("issue-{identifier}"),
            identifier: identifier.to_string(),
            title: title.to_string(),
            description: Some("Description".to_string()),
            url: format!("https://linear.app/issues/{identifier}"),
            priority: Some(2),
            estimate: None,
            updated_at: "2026-03-14T16:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "MET".to_string(),
                name: "Metastack".to_string(),
            },
            project: Some(ProjectRef {
                id: "project-1".to_string(),
                name: "MetaStack CLI".to_string(),
            }),
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: Some(WorkflowState {
                id: "state-1".to_string(),
                name: "Todo".to_string(),
                kind: Some("unstarted".to_string()),
            }),
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        }
    }

    #[test]
    fn weak_description_requires_enough_lines_and_structure() {
        assert!(description_is_weak("Short note"));
        assert!(!description_is_weak(
            "# Context\n\nDescribe the work with enough detail for execution, risks, and validation.\n\n# Validation\n\n- cargo test\n- cargo clippy --all-targets --all-features -- -D warnings\n"
        ));
    }

    #[test]
    fn missing_acceptance_requires_heading_or_checklist_density() {
        assert!(missing_acceptance_criteria("Plain paragraph only."));
        assert!(!missing_acceptance_criteria(
            "# Acceptance Criteria\n\n- [ ] prove it\n- [ ] ship it\n"
        ));
    }

    #[test]
    fn duplicate_themes_pair_similar_titles() {
        let first = issue("MET-1", "Improve backlog sync status output");
        let second = issue("MET-2", "Improve backlog sync status outputs");
        let duplicates = duplicate_theme_map(&[first, second]);
        assert_eq!(
            duplicates.get("MET-1").cloned(),
            Some(vec!["MET-2".to_string()])
        );
    }
}

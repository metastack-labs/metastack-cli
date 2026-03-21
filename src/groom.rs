use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Duration, Utc};
use walkdir::WalkDir;

use crate::backlog::{
    BacklogIssueMetadata, TemplateContext, compute_local_sync_hash, load_issue_metadata,
    render_template_files, save_issue_metadata, write_issue_description,
    write_rendered_backlog_item,
};
use crate::cli::GroomArgs;
use crate::config::load_required_planning_meta;
use crate::fs::{PlanningPaths, canonicalize_existing_dir, ensure_dir};
use crate::linear::{IssueListFilters, IssueSummary, load_linear_command_context};
use crate::scaffold::ensure_planning_layout;

const REFINE_LABEL: &str = "refine";
const MERGE_LABEL: &str = "merge";
const SPLIT_LABEL: &str = "split";
const ARCHIVE_LABEL: &str = "archive";
const RESCAN_REQUIRED_LABEL: &str = "rescan-required";
const WEAK_DESCRIPTION_MIN_LENGTH: usize = 180;
const SPLIT_ACCEPTANCE_LIMIT: usize = 6;
const ARCHIVE_STALE_DAYS: i64 = 120;

/// Run repo-scoped backlog grooming against the configured Linear project.
///
/// Returns an error when the repository root cannot be resolved, repo setup is missing, Linear
/// connectivity fails, or local packet scaffolding/report generation does not succeed.
pub async fn run_backlog_groom(args: &GroomArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.client.root)?;
    let _planning_meta = load_required_planning_meta(&root, "groom")?;
    ensure_planning_layout(&root, false)?;

    let context = load_linear_command_context(&args.client, None)?;
    let project_id = context.default_project_id.clone().ok_or_else(|| {
        anyhow!(
            "`meta backlog groom` requires a repo default project. Run `meta runtime setup --root . --project <PROJECT>` first."
        )
    })?;

    let issues = context
        .service
        .list_issues(IssueListFilters {
            team: context.default_team.clone(),
            project_id: Some(project_id.clone()),
            limit: args.limit.max(1),
            ..IssueListFilters::default()
        })
        .await?;

    if issues.is_empty() {
        println!(
            "Backlog groom report for `{project_id}`\nMode: {}\nIssues scanned: 0\nLocal packets created: 0\nReport mode created any missing local packets but did not mutate Linear.",
            if args.apply { "apply" } else { "report" }
        );
        return Ok(());
    }

    let local_packets = inspect_local_packets(&root, &issues)?;
    let duplicate_groups = duplicate_groups(&issues);
    let mut reports = Vec::with_capacity(issues.len());
    for issue in &issues {
        let local = local_packets
            .get(issue.identifier.as_str())
            .ok_or_else(|| anyhow!("missing local packet state for `{}`", issue.identifier))?;
        reports.push(analyze_issue(
            issue,
            local,
            duplicate_groups.get(issue.identifier.as_str()),
        )?);
    }

    let created_packets = ensure_missing_packets(&root, &issues, &reports)?;
    for report in &mut reports {
        if created_packets.contains(report.issue.identifier.as_str()) {
            report.packet_created = true;
            report.findings.push(GroomFinding {
                category: GroomCategory::RescanRequired,
                detail: "local packet was missing and was scaffolded during this run".to_string(),
            });
            report.sort_and_dedup_findings();
        }
    }

    let mut applied_mutations = Vec::new();
    if args.apply {
        for report in &reports {
            if report
                .findings
                .iter()
                .any(|finding| finding.category == GroomCategory::RescanRequired)
            {
                continue;
            }

            let description = refined_description(report);
            let state = if report
                .findings
                .iter()
                .any(|finding| finding.category == GroomCategory::Archive)
            {
                let edit_context = context
                    .service
                    .load_issue_edit_context(&report.issue.identifier)
                    .await?;
                resolve_archive_state_name(&edit_context.team.states)
            } else {
                None
            };
            let labels_to_add = labels_to_add(report);

            if description.is_none() && state.is_none() && labels_to_add.is_empty() {
                continue;
            }

            let updated = context
                .service
                .update_issue_fields(
                    &report.issue.identifier,
                    description.clone(),
                    state.clone(),
                    &labels_to_add,
                )
                .await?;

            if description.is_some() {
                applied_mutations.push(format!("{} description", updated.identifier));
            }
            if !labels_to_add.is_empty() {
                applied_mutations.push(format!(
                    "{} labels [{}]",
                    updated.identifier,
                    labels_to_add.join(", ")
                ));
            }
            if let Some(state_name) = state {
                applied_mutations.push(format!("{} state {}", updated.identifier, state_name));
            }
        }
    }

    println!(
        "{}",
        render_report(&project_id, args.apply, &reports, &applied_mutations)
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum GroomCategory {
    Refine,
    Merge,
    Split,
    Archive,
    RescanRequired,
}

impl GroomCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Refine => REFINE_LABEL,
            Self::Merge => MERGE_LABEL,
            Self::Split => SPLIT_LABEL,
            Self::Archive => ARCHIVE_LABEL,
            Self::RescanRequired => RESCAN_REQUIRED_LABEL,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroomFinding {
    category: GroomCategory,
    detail: String,
}

#[derive(Debug, Clone)]
struct GroomIssueReport {
    issue: IssueSummary,
    findings: Vec<GroomFinding>,
    local: LocalPacketState,
    packet_created: bool,
}

impl GroomIssueReport {
    fn sort_and_dedup_findings(&mut self) {
        self.findings.sort();
        self.findings.dedup();
    }
}

#[derive(Debug, Clone)]
struct LocalPacketState {
    issue_dir: PathBuf,
    existed_before: bool,
    metadata: Option<BacklogIssueMetadata>,
    latest_packet_update: Option<DateTime<Utc>>,
    local_hash: Option<String>,
}

fn inspect_local_packets(
    root: &Path,
    issues: &[IssueSummary],
) -> Result<BTreeMap<String, LocalPacketState>> {
    let paths = PlanningPaths::new(root);
    let mut packets = BTreeMap::new();

    for issue in issues {
        let issue_dir = paths.backlog_issue_dir(&issue.identifier);
        let metadata = if issue_dir.join(".linear.json").is_file() {
            Some(load_issue_metadata(&issue_dir)?)
        } else {
            None
        };
        packets.insert(
            issue.identifier.clone(),
            LocalPacketState {
                latest_packet_update: latest_packet_update(&issue_dir)?,
                local_hash: compute_local_sync_hash(&issue_dir)?,
                issue_dir: issue_dir.clone(),
                existed_before: issue_dir.is_dir(),
                metadata,
            },
        );
    }

    Ok(packets)
}

fn analyze_issue(
    issue: &IssueSummary,
    local: &LocalPacketState,
    duplicate_group: Option<&Vec<String>>,
) -> Result<GroomIssueReport> {
    let mut findings = Vec::new();
    let description = issue_description(issue);
    let acceptance_count = acceptance_criteria_count(description);
    let issue_updated_at = parse_rfc3339(&issue.updated_at)?;

    if !local.existed_before {
        findings.push(GroomFinding {
            category: GroomCategory::RescanRequired,
            detail: "local backlog packet is missing".to_string(),
        });
    }
    if local.metadata.is_none() {
        findings.push(GroomFinding {
            category: GroomCategory::RescanRequired,
            detail: "local packet is missing `.linear.json` metadata".to_string(),
        });
    }
    if let Some(metadata) = local.metadata.as_ref() {
        if !metadata.identifier.eq_ignore_ascii_case(&issue.identifier) {
            findings.push(GroomFinding {
                category: GroomCategory::RescanRequired,
                detail: format!(
                    "local packet metadata points at `{}` instead of `{}`",
                    metadata.identifier, issue.identifier
                ),
            });
        }
        if metadata.last_sync_at.is_none() {
            findings.push(GroomFinding {
                category: GroomCategory::RescanRequired,
                detail: "local packet has no recorded sync baseline".to_string(),
            });
        }
        let local_changed_since_sync =
            metadata.local_hash.is_some() && metadata.local_hash != local.local_hash;
        let remote_changed_since_sync = metadata
            .last_sync_at
            .as_deref()
            .map(parse_rfc3339)
            .transpose()?
            .is_some_and(|timestamp| issue_updated_at > timestamp);
        if local_changed_since_sync && remote_changed_since_sync {
            findings.push(GroomFinding {
                category: GroomCategory::RescanRequired,
                detail: "local packet and Linear issue both changed since the last sync"
                    .to_string(),
            });
        }
    }
    if local
        .latest_packet_update
        .is_some_and(|timestamp| issue_updated_at > timestamp)
    {
        findings.push(GroomFinding {
            category: GroomCategory::RescanRequired,
            detail: format!(
                "Linear issue updated at {} after the local packet timestamp",
                issue.updated_at
            ),
        });
    }

    let mut refine_reasons = Vec::new();
    if acceptance_count == 0 {
        refine_reasons.push("missing acceptance criteria".to_string());
    }
    if description_is_weak(description) {
        refine_reasons.push("description lacks enough implementation context".to_string());
    }
    if !refine_reasons.is_empty() {
        findings.push(GroomFinding {
            category: GroomCategory::Refine,
            detail: refine_reasons.join("; "),
        });
    }

    if acceptance_count >= SPLIT_ACCEPTANCE_LIMIT
        || should_split(issue, description, acceptance_count)
    {
        findings.push(GroomFinding {
            category: GroomCategory::Split,
            detail: format!(
                "ticket spans {} acceptance items or multiple workstreams",
                acceptance_count.max(2)
            ),
        });
    }

    if let Some(group) = duplicate_group
        && group.len() > 1
    {
        findings.push(GroomFinding {
            category: GroomCategory::Merge,
            detail: format!("duplicate theme overlaps {}", group.join(", ")),
        });
    }

    if should_archive(issue, description, issue_updated_at) {
        findings.push(GroomFinding {
            category: GroomCategory::Archive,
            detail: format!(
                "stale backlog item with weak context and no current assignee (>{ARCHIVE_STALE_DAYS} days old)"
            ),
        });
    }

    let mut report = GroomIssueReport {
        issue: issue.clone(),
        findings,
        local: local.clone(),
        packet_created: false,
    };
    report.sort_and_dedup_findings();
    Ok(report)
}

fn labels_to_add(report: &GroomIssueReport) -> Vec<String> {
    let existing = report
        .issue
        .labels
        .iter()
        .map(|label| label.name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();

    report
        .findings
        .iter()
        .filter_map(|finding| {
            let label = finding.category.as_str().to_string();
            (!existing.contains(&label)).then_some(label)
        })
        .filter(|label| label != RESCAN_REQUIRED_LABEL)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn refined_description(report: &GroomIssueReport) -> Option<String> {
    if !report
        .findings
        .iter()
        .any(|finding| finding.category == GroomCategory::Refine)
    {
        return None;
    }

    let mut description = issue_description(&report.issue).trim_end().to_string();
    let mut changed = false;
    if acceptance_criteria_count(&description) == 0 {
        if !description.is_empty() {
            description.push_str("\n\n");
        }
        description
            .push_str("## Acceptance Criteria\n- [ ] Add explicit, testable acceptance criteria for this ticket.");
        changed = true;
    }
    if description_is_weak(&description) && !description.to_ascii_lowercase().contains("## context")
    {
        if !description.is_empty() {
            description.push_str("\n\n");
        }
        description.push_str(
            "## Context\n- Repository scope:\n- Constraints and dependencies:\n- Validation proof required:\n",
        );
        changed = true;
    }

    changed.then_some(description)
}

fn resolve_archive_state_name(states: &[crate::linear::WorkflowState]) -> Option<String> {
    states
        .iter()
        .find(|state| {
            state
                .kind
                .as_deref()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("canceled"))
                || state.name.eq_ignore_ascii_case("archive")
                || state.name.eq_ignore_ascii_case("archived")
        })
        .map(|state| state.name.clone())
}

fn ensure_missing_packets(
    root: &Path,
    issues: &[IssueSummary],
    reports: &[GroomIssueReport],
) -> Result<BTreeSet<String>> {
    let mut created = BTreeSet::new();
    for (issue, report) in issues.iter().zip(reports.iter()) {
        if report.local.existed_before {
            continue;
        }
        scaffold_issue_packet(root, issue)?;
        created.insert(issue.identifier.clone());
    }
    Ok(created)
}

fn scaffold_issue_packet(root: &Path, issue: &IssueSummary) -> Result<()> {
    let issue_dir = PlanningPaths::new(root).backlog_issue_dir(&issue.identifier);
    ensure_dir(&issue_dir)?;
    let rendered = render_template_files(
        root,
        &TemplateContext {
            issue_identifier: Some(issue.identifier.clone()),
            issue_title: Some(issue.title.clone()),
            issue_url: Some(issue.url.clone()),
            ..TemplateContext::default()
        },
    )?;
    let _ = write_rendered_backlog_item(root, &issue.identifier, &rendered)?;
    write_issue_description(root, &issue.identifier, issue_description(issue))?;
    save_issue_metadata(&issue_dir, &issue_metadata(issue))?;
    Ok(())
}

fn issue_metadata(issue: &IssueSummary) -> BacklogIssueMetadata {
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
        managed_files: Vec::new(),
    }
}

fn latest_packet_update(issue_dir: &Path) -> Result<Option<DateTime<Utc>>> {
    if !issue_dir.is_dir() {
        return Ok(None);
    }

    let mut latest = None;
    for entry in WalkDir::new(issue_dir) {
        let entry =
            entry.with_context(|| format!("failed to traverse `{}`", issue_dir.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        if entry
            .path()
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }
        let modified = entry
            .metadata()
            .with_context(|| format!("failed to read `{}`", entry.path().display()))?
            .modified()
            .with_context(|| format!("failed to read mtime for `{}`", entry.path().display()))?;
        let modified = DateTime::<Utc>::from(modified);
        latest = Some(latest.map_or(modified, |current: DateTime<Utc>| current.max(modified)));
    }

    Ok(latest)
}

fn duplicate_groups(issues: &[IssueSummary]) -> BTreeMap<String, Vec<String>> {
    let mut groups = BTreeMap::<String, Vec<String>>::new();
    for issue in issues {
        let key = duplicate_theme_key(&issue.title);
        if key.is_empty() {
            continue;
        }
        groups
            .entry(key)
            .or_default()
            .push(issue.identifier.clone());
    }

    let mut duplicates = BTreeMap::new();
    for identifiers in groups.into_values().filter(|ids| ids.len() > 1) {
        for identifier in &identifiers {
            duplicates.insert(identifier.clone(), identifiers.clone());
        }
    }
    duplicates
}

fn duplicate_theme_key(title: &str) -> String {
    title
        .to_ascii_lowercase()
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| {
            !token.is_empty()
                && !matches!(
                    *token,
                    "a" | "an" | "and" | "for" | "in" | "meta" | "of" | "the" | "to"
                )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_rfc3339(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .with_context(|| format!("failed to parse RFC3339 timestamp `{value}`"))
}

fn issue_description(issue: &IssueSummary) -> &str {
    issue.description.as_deref().unwrap_or_default()
}

fn acceptance_criteria_count(description: &str) -> usize {
    let mut in_section = false;
    let mut count = 0;
    for line in description.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            in_section = trimmed.to_ascii_lowercase().contains("acceptance criteria");
            continue;
        }
        if in_section
            && (trimmed.starts_with("- ")
                || trimmed.starts_with("* ")
                || trimmed.starts_with("- [ ]")
                || trimmed.starts_with("- [x]"))
        {
            count += 1;
        }
    }

    if count == 0
        && description
            .to_ascii_lowercase()
            .contains("acceptance criteria")
    {
        1
    } else {
        count
    }
}

fn description_is_weak(description: &str) -> bool {
    let trimmed = description.trim();
    if trimmed.is_empty() {
        return true;
    }

    let nonempty_lines = trimmed
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    trimmed.len() < WEAK_DESCRIPTION_MIN_LENGTH || nonempty_lines < 3
}

fn should_split(issue: &IssueSummary, description: &str, acceptance_count: usize) -> bool {
    issue.title.to_ascii_lowercase().matches(" and ").count() > 0
        && (acceptance_count >= 4 || description.lines().count() >= 10)
}

fn should_archive(issue: &IssueSummary, description: &str, updated_at: DateTime<Utc>) -> bool {
    let stale = Utc::now() - updated_at >= Duration::days(ARCHIVE_STALE_DAYS);
    let state_kind = issue.state.as_ref().and_then(|state| state.kind.as_deref());
    stale
        && issue.assignee.is_none()
        && description_is_weak(description)
        && state_kind.is_some_and(|kind| {
            kind.eq_ignore_ascii_case("backlog") || kind.eq_ignore_ascii_case("unstarted")
        })
}

fn render_report(
    project_id: &str,
    apply: bool,
    reports: &[GroomIssueReport],
    applied_mutations: &[String],
) -> String {
    let mut category_counts = BTreeMap::<&str, usize>::new();
    let mut created_packets = 0;
    for report in reports {
        if report.packet_created {
            created_packets += 1;
        }
        for finding in &report.findings {
            *category_counts
                .entry(finding.category.as_str())
                .or_default() += 1;
        }
    }

    let mut lines = vec![
        format!("Backlog groom report for `{project_id}`"),
        format!("Mode: {}", if apply { "apply" } else { "report" }),
        format!("Issues scanned: {}", reports.len()),
        format!("Local packets created: {created_packets}"),
        "Category totals:".to_string(),
    ];
    for category in [
        REFINE_LABEL,
        MERGE_LABEL,
        SPLIT_LABEL,
        ARCHIVE_LABEL,
        RESCAN_REQUIRED_LABEL,
    ] {
        lines.push(format!(
            "- {category}: {}",
            category_counts.get(category).copied().unwrap_or_default()
        ));
    }
    lines.push(String::new());

    for report in reports {
        lines.push(format!(
            "{}  {}",
            report.issue.identifier, report.issue.title
        ));
        lines.push(format!(
            "  packet: {}",
            display_packet_path(&report.local.issue_dir)
        ));
        if report.findings.is_empty() {
            lines.push("  - clean: no grooming findings".to_string());
            continue;
        }
        for finding in &report.findings {
            lines.push(format!(
                "  - {}: {}",
                finding.category.as_str(),
                finding.detail
            ));
        }
    }

    lines.push(String::new());
    if apply {
        if applied_mutations.is_empty() {
            lines.push("Applied mutations: none".to_string());
        } else {
            lines.push("Applied mutations:".to_string());
            for mutation in applied_mutations {
                lines.push(format!("- {mutation}"));
            }
        }
    } else {
        lines.push(
            "Report mode created any missing local packets but did not mutate Linear.".to_string(),
        );
    }

    lines.join("\n")
}

fn display_packet_path(path: &Path) -> String {
    path.components()
        .rev()
        .take(3)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::{
        GroomCategory, acceptance_criteria_count, description_is_weak, duplicate_theme_key,
    };

    #[test]
    fn acceptance_criteria_counter_tracks_markdown_items() {
        let description = "\
## Summary
Implement the workflow.

## Acceptance Criteria
- [ ] proof one
- [ ] proof two
";

        assert_eq!(acceptance_criteria_count(description), 2);
    }

    #[test]
    fn duplicate_theme_key_ignores_common_noise_words() {
        assert_eq!(
            duplicate_theme_key("Add the meta backlog groom command"),
            duplicate_theme_key("Add backlog groom command"),
        );
    }

    #[test]
    fn weak_description_requires_more_than_a_stub() {
        assert!(description_is_weak("short stub"));
        assert!(!description_is_weak(
            "## Summary\nDetailed repository context with enough implementation detail to make the ticket executable without guessing, including the affected command path, the expected repository-scoped behavior, the drift signals that must be surfaced, and the exact validation proof that reviewers should expect.\n\n## Acceptance Criteria\n- one\n- two\n",
        ));
    }

    #[test]
    fn category_strings_remain_stable() {
        assert_eq!(GroomCategory::Refine.as_str(), "refine");
        assert_eq!(GroomCategory::RescanRequired.as_str(), "rescan-required");
    }
}

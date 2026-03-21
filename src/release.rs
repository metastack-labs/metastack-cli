use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;

use crate::backlog::{BacklogIssueMetadata, METADATA_FILE_NAME};
use crate::cli::ReleaseArgs;
use crate::fs::{PlanningPaths, canonicalize_existing_dir, ensure_dir, write_text_file};
use crate::linear::{IssueEditSpec, IssueListFilters, IssueSummary, load_linear_command_context};
use crate::scaffold::ensure_planning_layout;

const RELEASE_INDEX_FILE: &str = "index.md";
const RELEASE_JSON_FILE: &str = "plan.json";
const BACKLOG_SCOPE_LIMIT: usize = 250;
const DEPENDENCY_HINTS: &[&str] = &[
    "blocked by",
    "depends on",
    "dependency",
    "requires",
    "prerequisite",
    "after ",
];

#[derive(Debug, Clone)]
struct LocalBacklogEntry {
    identifier: String,
    local_path: PathBuf,
    metadata: BacklogIssueMetadata,
    notes: Vec<BacklogNote>,
}

#[derive(Debug, Clone)]
struct BacklogNote {
    contents: String,
}

#[derive(Debug, Clone, Serialize)]
struct ReleaseIssue {
    identifier: String,
    title: String,
    url: String,
    local_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    estimate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    must_have: bool,
    above_cut_line: bool,
    dependency_signals: Vec<String>,
    rationale: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ReleasePacket {
    name: String,
    generated_at: String,
    root: String,
    batch_size: usize,
    selected_issue_count: usize,
    live_linear_data: bool,
    included: Vec<ReleaseIssue>,
    deferred: Vec<ReleaseIssue>,
    ordering: Vec<String>,
    risks: Vec<String>,
    notes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    apply: Option<AppliedMetadata>,
}

#[derive(Debug, Clone, Serialize)]
struct AppliedMetadata {
    project: Option<String>,
    state: Option<String>,
    updated_issues: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ReleaseReport {
    packet_dir: PathBuf,
    packet: ReleasePacket,
}

impl ReleaseReport {
    pub(crate) fn render(&self, json: bool) -> String {
        if json {
            return serde_json::to_string_pretty(&self.packet)
                .unwrap_or_else(|_| "{\"error\":\"failed to encode release packet\"}".to_string());
        }

        let included = if self.packet.included.is_empty() {
            "none".to_string()
        } else {
            self.packet
                .included
                .iter()
                .map(|issue| issue.identifier.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let deferred = if self.packet.deferred.is_empty() {
            "none".to_string()
        } else {
            self.packet
                .deferred
                .iter()
                .map(|issue| issue.identifier.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };

        let mut lines = vec![
            format!(
                "Release plan `{}` written to {}.",
                self.packet.name,
                self.packet_dir.display()
            ),
            format!(
                "Included above cut line ({}): {}",
                self.packet.included.len(),
                included
            ),
            format!(
                "Deferred below cut line ({}): {}",
                self.packet.deferred.len(),
                deferred
            ),
        ];

        if let Some(apply) = &self.packet.apply {
            lines.push(format!(
                "Applied Linear metadata to {} issue(s).",
                apply.updated_issues.len()
            ));
        }

        if !self.packet.risks.is_empty() {
            lines.push(format!("Risks: {}", self.packet.risks.join(" | ")));
        }

        lines.join("\n")
    }
}

/// Build a local release packet from repo-scoped backlog items and optionally apply existing
/// Linear issue metadata updates for the issues above the cut line.
///
/// Returns an error when the repository root cannot be resolved, the local backlog is malformed,
/// the selected batch is too small to plan, or an explicit apply request cannot be satisfied.
pub async fn run_release(args: &ReleaseArgs) -> Result<ReleaseReport> {
    let root = canonicalize_existing_dir(&args.client.root)?;
    ensure_planning_layout(&root, false)?;
    if args.batch_size == 0 {
        bail!("`meta backlog release --batch-size` must be at least 1");
    }

    if args.apply && args.project.is_none() && args.state.is_none() {
        bail!("`meta backlog release --apply` requires `--project`, `--state`, or both");
    }

    let paths = PlanningPaths::new(&root);
    let backlog = load_local_backlog(&root, &paths, &args.issues)?;
    if backlog.is_empty() {
        bail!(
            "no local backlog items were found under `{}`; pull or create backlog items first",
            paths.backlog_dir.display()
        );
    }
    if backlog.len() < 2 {
        bail!(
            "not enough backlog items to build a release packet; found {} item",
            backlog.len()
        );
    }

    let mut linear_context = None;
    let mut live_issues = BTreeMap::new();
    match load_linear_command_context(&args.client, None) {
        Ok(context) => {
            live_issues = load_live_issue_map(&context, &backlog).await?;
            linear_context = Some(context);
        }
        Err(error) if args.apply => return Err(error),
        Err(_) => {}
    }

    let packet = build_release_packet(&root, args, &backlog, &live_issues)?;
    let packet_dir = write_release_packet(&root, &args.name, &packet)?;

    let applied = if args.apply {
        let context = linear_context
            .as_ref()
            .ok_or_else(|| anyhow!("Linear access is required when `--apply` is enabled"))?;
        Some(apply_release_metadata(context, &packet, args).await?)
    } else {
        None
    };

    let packet = ReleasePacket {
        apply: applied,
        ..packet
    };
    write_release_packet(&root, &args.name, &packet)?;

    Ok(ReleaseReport { packet_dir, packet })
}

async fn apply_release_metadata(
    context: &crate::LinearCommandContext,
    packet: &ReleasePacket,
    args: &ReleaseArgs,
) -> Result<AppliedMetadata> {
    let mut updated = Vec::with_capacity(packet.included.len());

    for issue in &packet.included {
        context
            .service
            .edit_issue(IssueEditSpec {
                identifier: issue.identifier.clone(),
                title: None,
                description: None,
                project: args.project.clone(),
                state: args.state.clone(),
                priority: None,
            })
            .await
            .with_context(|| {
                format!("failed to apply Linear metadata to `{}`", issue.identifier)
            })?;
        updated.push(issue.identifier.clone());
    }

    Ok(AppliedMetadata {
        project: args.project.clone(),
        state: args.state.clone(),
        updated_issues: updated,
    })
}

async fn load_live_issue_map(
    context: &crate::LinearCommandContext,
    backlog: &[LocalBacklogEntry],
) -> Result<BTreeMap<String, IssueSummary>> {
    let identifiers = backlog
        .iter()
        .map(|entry| entry.identifier.clone())
        .collect::<BTreeSet<_>>();
    let mut issues = context
        .service
        .list_issues(IssueListFilters {
            team: context.default_team.clone(),
            project_id: context.default_project_id.clone(),
            limit: BACKLOG_SCOPE_LIMIT.max(identifiers.len()),
            ..IssueListFilters::default()
        })
        .await
        .context("failed to load repo-scoped backlog issues from Linear")?
        .into_iter()
        .filter(|issue| identifiers.contains(&issue.identifier))
        .map(|issue| (issue.identifier.clone(), issue))
        .collect::<BTreeMap<_, _>>();

    let missing = identifiers
        .into_iter()
        .filter(|identifier| !issues.contains_key(identifier))
        .collect::<Vec<_>>();
    for identifier in missing {
        if let Ok(issue) = context.service.load_issue(&identifier).await {
            issues.insert(identifier, issue);
        }
    }

    Ok(issues)
}

fn build_release_packet(
    root: &Path,
    args: &ReleaseArgs,
    backlog: &[LocalBacklogEntry],
    live_issues: &BTreeMap<String, IssueSummary>,
) -> Result<ReleasePacket> {
    let identifiers = backlog
        .iter()
        .map(|entry| entry.identifier.clone())
        .collect::<BTreeSet<_>>();
    let dependency_map = infer_dependencies(backlog, live_issues, &identifiers);
    let dependents = build_dependents(&dependency_map);
    let ordered = topological_order(backlog, live_issues, &dependency_map);
    let must_have = infer_must_have(&ordered, live_issues, &dependency_map);
    let included = ordered
        .iter()
        .take(args.batch_size)
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut issues = Vec::with_capacity(ordered.len());
    let mut risks = Vec::new();
    let mut notes = Vec::new();

    if live_issues.is_empty() {
        risks.push(
            "Live Linear issue data was unavailable, so ordering fell back to local backlog metadata and identifier order."
                .to_string(),
        );
    }

    for entry in backlog {
        let live = live_issues.get(&entry.identifier);
        if live.is_none() {
            notes.push(format!(
                "{} used local `.linear.json` metadata because the Linear issue could not be loaded.",
                entry.identifier
            ));
        }
        if live.and_then(|issue| issue.priority).is_none() {
            risks.push(format!(
                "{} has no live priority, so it may be cut later than intended.",
                entry.identifier
            ));
        }
    }

    for identifier in &ordered {
        let entry = backlog
            .iter()
            .find(|entry| &entry.identifier == identifier)
            .ok_or_else(|| anyhow!("failed to resolve selected backlog item `{identifier}`"))?;
        let live = live_issues.get(identifier);
        let dependencies = dependency_map
            .get(identifier)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        let priority = live.and_then(|issue| issue.priority);
        let above_cut_line = included.contains(identifier);
        let must = must_have.contains(identifier);
        let rationale = issue_rationale(identifier, priority, must, above_cut_line, &dependencies);

        if must && !above_cut_line {
            risks.push(format!(
                "{} is must-have but falls below the recommended cut line; either raise batch size or defer dependent work.",
                identifier
            ));
        }
        if dependencies
            .iter()
            .any(|dependency| !identifiers.contains(dependency))
        {
            risks.push(format!(
                "{} references dependency signals outside the selected backlog scope.",
                identifier
            ));
        }

        issues.push(ReleaseIssue {
            identifier: identifier.clone(),
            title: live
                .map(|issue| issue.title.clone())
                .unwrap_or_else(|| entry.metadata.title.clone()),
            url: live
                .map(|issue| issue.url.clone())
                .unwrap_or_else(|| entry.metadata.url.clone()),
            local_path: entry
                .local_path
                .strip_prefix(root)
                .unwrap_or(&entry.local_path)
                .display()
                .to_string(),
            priority,
            estimate: live.and_then(|issue| issue.estimate),
            state: live.and_then(|issue| issue.state.as_ref().map(|state| state.name.clone())),
            project: live
                .and_then(|issue| issue.project.as_ref().map(|project| project.name.clone())),
            must_have: must,
            above_cut_line,
            dependency_signals: dependencies,
            rationale,
        });
    }

    let mut included_issues = Vec::new();
    let mut deferred_issues = Vec::new();
    for issue in issues {
        if issue.above_cut_line {
            included_issues.push(issue);
        } else {
            deferred_issues.push(issue);
        }
    }

    for issue in &included_issues {
        if let Some(dependents) = dependents.get(&issue.identifier)
            && dependents
                .iter()
                .any(|dependent| !included.contains(dependent))
        {
            notes.push(format!(
                "{} lands above the cut line while at least one dependent issue stays deferred.",
                issue.identifier
            ));
        }
    }

    risks.sort();
    risks.dedup();
    notes.sort();
    notes.dedup();

    Ok(ReleasePacket {
        name: args.name.clone(),
        generated_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .context("failed to format release packet timestamp")?,
        root: root.display().to_string(),
        batch_size: args.batch_size,
        selected_issue_count: backlog.len(),
        live_linear_data: !live_issues.is_empty(),
        included: included_issues,
        deferred: deferred_issues,
        ordering: ordered,
        risks,
        notes,
        apply: None,
    })
}

fn issue_rationale(
    identifier: &str,
    priority: Option<u8>,
    must_have: bool,
    above_cut_line: bool,
    dependencies: &[String],
) -> Vec<String> {
    let mut rationale = Vec::new();
    match priority {
        Some(1) => rationale.push("Urgent priority keeps this issue near the front of the batch.".to_string()),
        Some(2) => rationale.push("High priority marks this issue as milestone-critical in the first cut.".to_string()),
        Some(3) => rationale.push("Medium priority keeps this issue in scope when capacity remains after must-have work.".to_string()),
        Some(4) => rationale.push("Low priority pushes this issue below the cut line unless spare capacity remains.".to_string()),
        _ => rationale.push("Missing live priority forces a conservative fallback to dependency and identifier ordering.".to_string()),
    }
    if must_have {
        rationale.push(
            "This item stays in the must-have lane because of its priority and prerequisite role."
                .to_string(),
        );
    } else {
        rationale.push("This item is deferrable once the must-have slice is covered.".to_string());
    }
    if above_cut_line {
        rationale.push(format!(
            "{identifier} stays above the recommended cut line for the next execution batch."
        ));
    } else {
        rationale.push(format!(
            "{identifier} lands below the recommended cut line and can move to the next milestone."
        ));
    }
    if !dependencies.is_empty() {
        rationale.push(format!(
            "Dependency signals keep it sequenced after {}.",
            dependencies.join(", ")
        ));
    }
    rationale
}

fn build_dependents(
    dependency_map: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut dependents = BTreeMap::<String, BTreeSet<String>>::new();
    for (issue, dependencies) in dependency_map {
        for dependency in dependencies {
            dependents
                .entry(dependency.clone())
                .or_default()
                .insert(issue.clone());
        }
    }
    dependents
}

fn infer_must_have(
    ordered: &[String],
    live_issues: &BTreeMap<String, IssueSummary>,
    dependency_map: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut must_have = ordered
        .iter()
        .filter(|identifier| {
            matches!(
                live_issues
                    .get(*identifier)
                    .and_then(|issue| issue.priority),
                Some(1 | 2)
            )
        })
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut queue = must_have.iter().cloned().collect::<VecDeque<_>>();
    while let Some(identifier) = queue.pop_front() {
        for dependency in dependency_map.get(&identifier).into_iter().flatten() {
            if must_have.insert(dependency.clone()) {
                queue.push_back(dependency.clone());
            }
        }
    }

    must_have
}

fn topological_order(
    backlog: &[LocalBacklogEntry],
    live_issues: &BTreeMap<String, IssueSummary>,
    dependency_map: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<String> {
    let mut indegree = BTreeMap::<String, usize>::new();
    let mut dependents = BTreeMap::<String, Vec<String>>::new();
    let own_rank = backlog
        .iter()
        .map(|entry| {
            (
                entry.identifier.clone(),
                priority_rank(
                    live_issues
                        .get(&entry.identifier)
                        .and_then(|issue| issue.priority),
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();

    for entry in backlog {
        indegree.entry(entry.identifier.clone()).or_insert(0);
    }
    for (issue, dependencies) in dependency_map {
        indegree.insert(issue.clone(), dependencies.len());
        for dependency in dependencies {
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(issue.clone());
        }
    }
    let mut effective_rank = BTreeMap::new();
    for entry in backlog {
        let mut visiting = BTreeSet::new();
        let rank = descendant_priority_rank(
            &entry.identifier,
            &dependents,
            &own_rank,
            &mut effective_rank,
            &mut visiting,
        );
        effective_rank.insert(entry.identifier.clone(), rank);
    }

    let mut ready = indegree
        .iter()
        .filter(|(_, count)| **count == 0)
        .map(|(identifier, _)| identifier.clone())
        .collect::<Vec<_>>();
    ready.sort_by_key(|identifier| {
        (
            effective_rank.get(identifier).copied().unwrap_or(99),
            own_rank.get(identifier).copied().unwrap_or(99),
            identifier.clone(),
        )
    });

    let mut ordered = Vec::with_capacity(backlog.len());
    while let Some(identifier) = ready.first().cloned() {
        ready.remove(0);
        ordered.push(identifier.clone());
        if let Some(children) = dependents.get(&identifier) {
            for child in children {
                if let Some(count) = indegree.get_mut(child) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        ready.push(child.clone());
                    }
                }
            }
            ready.sort_by_key(|identifier| {
                (
                    effective_rank.get(identifier).copied().unwrap_or(99),
                    own_rank.get(identifier).copied().unwrap_or(99),
                    identifier.clone(),
                )
            });
        }
    }

    if ordered.len() < backlog.len() {
        let ordered_set = ordered.iter().cloned().collect::<BTreeSet<_>>();
        let mut cycle_tail = backlog
            .iter()
            .filter(|entry| !ordered_set.contains(&entry.identifier))
            .map(|entry| entry.identifier.clone())
            .collect::<Vec<_>>();
        cycle_tail.sort_by_key(|identifier| {
            (
                effective_rank.get(identifier).copied().unwrap_or(99),
                own_rank.get(identifier).copied().unwrap_or(99),
                identifier.clone(),
            )
        });
        ordered.extend(cycle_tail);
    }

    ordered
}

fn descendant_priority_rank(
    identifier: &str,
    dependents: &BTreeMap<String, Vec<String>>,
    own_rank: &BTreeMap<String, u8>,
    cache: &mut BTreeMap<String, u8>,
    visiting: &mut BTreeSet<String>,
) -> u8 {
    if let Some(rank) = cache.get(identifier) {
        return *rank;
    }
    if !visiting.insert(identifier.to_string()) {
        return own_rank.get(identifier).copied().unwrap_or(99);
    }

    let mut rank = own_rank.get(identifier).copied().unwrap_or(99);
    if let Some(children) = dependents.get(identifier) {
        for child in children {
            rank = rank.min(descendant_priority_rank(
                child, dependents, own_rank, cache, visiting,
            ));
        }
    }
    visiting.remove(identifier);
    cache.insert(identifier.to_string(), rank);
    rank
}

fn priority_rank(priority: Option<u8>) -> u8 {
    match priority {
        Some(1) => 1,
        Some(2) => 2,
        Some(3) => 3,
        Some(4) => 4,
        Some(0) => 5,
        _ => 6,
    }
}

fn infer_dependencies(
    backlog: &[LocalBacklogEntry],
    live_issues: &BTreeMap<String, IssueSummary>,
    identifiers: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut dependency_map = BTreeMap::<String, BTreeSet<String>>::new();

    for entry in backlog {
        let mut dependencies = BTreeSet::new();

        if let Some(issue) = live_issues.get(&entry.identifier) {
            if let Some(parent) = &issue.parent
                && identifiers.contains(&parent.identifier)
            {
                dependencies.insert(parent.identifier.clone());
            }
        }

        for note in &entry.notes {
            collect_dependency_mentions(
                &note.contents,
                &entry.identifier,
                identifiers,
                &mut dependencies,
            );
        }
        if let Some(issue) = live_issues.get(&entry.identifier)
            && let Some(description) = issue.description.as_deref()
        {
            collect_dependency_mentions(
                description,
                &entry.identifier,
                identifiers,
                &mut dependencies,
            );
        }

        dependency_map.insert(entry.identifier.clone(), dependencies);
    }

    for issue in live_issues.values() {
        for child in &issue.children {
            if identifiers.contains(&child.identifier) && identifiers.contains(&issue.identifier) {
                dependency_map
                    .entry(child.identifier.clone())
                    .or_default()
                    .insert(issue.identifier.clone());
            }
        }
    }

    dependency_map
}

fn collect_dependency_mentions(
    contents: &str,
    current_identifier: &str,
    identifiers: &BTreeSet<String>,
    dependencies: &mut BTreeSet<String>,
) {
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        let lowered = line.to_ascii_lowercase();
        if !DEPENDENCY_HINTS.iter().any(|hint| lowered.contains(hint)) {
            continue;
        }

        for identifier in identifiers {
            if identifier == current_identifier {
                continue;
            }
            if line.contains(identifier) {
                dependencies.insert(identifier.clone());
            }
        }
    }
}

fn write_release_packet(root: &Path, name: &str, packet: &ReleasePacket) -> Result<PathBuf> {
    let paths = PlanningPaths::new(root);
    ensure_dir(&paths.releases_dir)?;
    let packet_dir = paths.release_plan_dir(name);
    ensure_dir(&packet_dir)?;

    write_text_file(
        &packet_dir.join(RELEASE_INDEX_FILE),
        &render_release_markdown(packet),
        true,
    )?;
    write_text_file(
        &packet_dir.join(RELEASE_JSON_FILE),
        &serde_json::to_string_pretty(packet).context("failed to serialize release packet")?,
        true,
    )?;

    Ok(packet_dir)
}

fn render_release_markdown(packet: &ReleasePacket) -> String {
    let mut lines = vec![
        format!("# Release Plan: {}", packet.name),
        String::new(),
        format!("- Generated: `{}`", packet.generated_at),
        format!("- Root: `{}`", packet.root),
        format!("- Batch size: `{}`", packet.batch_size),
        format!(
            "- Selected backlog issues: `{}`",
            packet.selected_issue_count
        ),
        format!(
            "- Live Linear data: `{}`",
            if packet.live_linear_data { "yes" } else { "no" }
        ),
        String::new(),
        "## Recommended Batch".to_string(),
        String::new(),
    ];

    for issue in &packet.included {
        lines.push(format!(
            "- `{}` {}",
            issue.identifier,
            markdown_issue_line(issue)
        ));
    }
    if packet.included.is_empty() {
        lines.push("- No issues landed above the recommended cut line.".to_string());
    }

    lines.extend([
        String::new(),
        "## Deferred After Cut Line".to_string(),
        String::new(),
    ]);
    for issue in &packet.deferred {
        lines.push(format!(
            "- `{}` {}",
            issue.identifier,
            markdown_issue_line(issue)
        ));
    }
    if packet.deferred.is_empty() {
        lines.push("- No additional issues remain below the cut line.".to_string());
    }

    lines.extend([String::new(), "## Ordering".to_string(), String::new()]);
    for (index, identifier) in packet.ordering.iter().enumerate() {
        lines.push(format!("{}. `{}`", index + 1, identifier));
    }

    lines.extend([String::new(), "## Risks".to_string(), String::new()]);
    if packet.risks.is_empty() {
        lines.push(
            "- No material planning risks were detected from the selected scope.".to_string(),
        );
    } else {
        for risk in &packet.risks {
            lines.push(format!("- {risk}"));
        }
    }

    lines.extend([String::new(), "## Notes".to_string(), String::new()]);
    if packet.notes.is_empty() {
        lines.push("- No extra planning notes.".to_string());
    } else {
        for note in &packet.notes {
            lines.push(format!("- {note}"));
        }
    }

    if let Some(apply) = &packet.apply {
        lines.extend([
            String::new(),
            "## Applied Linear Metadata".to_string(),
            String::new(),
        ]);
        if let Some(project) = &apply.project {
            lines.push(format!("- Project: `{project}`"));
        }
        if let Some(state) = &apply.state {
            lines.push(format!("- State: `{state}`"));
        }
        lines.push(format!(
            "- Updated issues: {}",
            apply.updated_issues.join(", ")
        ));
    }

    lines.join("\n")
}

fn markdown_issue_line(issue: &ReleaseIssue) -> String {
    let mut segments = vec![issue.title.clone()];
    if let Some(priority) = issue.priority {
        segments.push(format!("priority {}", priority));
    }
    if let Some(project) = &issue.project {
        segments.push(format!("project `{project}`"));
    }
    if !issue.dependency_signals.is_empty() {
        segments.push(format!("after {}", issue.dependency_signals.join(", ")));
    }
    segments.join(" | ")
}

fn load_local_backlog(
    root: &Path,
    paths: &PlanningPaths,
    selected_issues: &[String],
) -> Result<Vec<LocalBacklogEntry>> {
    if !paths.backlog_dir.is_dir() {
        return Ok(Vec::new());
    }

    let selected = selected_issues
        .iter()
        .map(|identifier| identifier.trim().to_string())
        .filter(|identifier| !identifier.is_empty())
        .collect::<BTreeSet<_>>();

    let mut entries = Vec::new();
    for dir_entry in fs::read_dir(&paths.backlog_dir)
        .with_context(|| format!("failed to read `{}`", paths.backlog_dir.display()))?
    {
        let dir_entry = dir_entry
            .with_context(|| format!("failed to read `{}`", paths.backlog_dir.display()))?;
        if !dir_entry
            .file_type()
            .with_context(|| format!("failed to inspect `{}`", dir_entry.path().display()))?
            .is_dir()
        {
            continue;
        }

        let file_name = dir_entry.file_name();
        let identifier = file_name.to_string_lossy().to_string();
        if identifier == "_TEMPLATE" || (!selected.is_empty() && !selected.contains(&identifier)) {
            continue;
        }

        let local_path = dir_entry.path();
        let metadata_path = local_path.join(METADATA_FILE_NAME);
        if !metadata_path.is_file() {
            continue;
        }

        let metadata = serde_json::from_str::<BacklogIssueMetadata>(
            &fs::read_to_string(&metadata_path)
                .with_context(|| format!("failed to read `{}`", metadata_path.display()))?,
        )
        .with_context(|| format!("failed to parse `{}`", metadata_path.display()))?;

        let notes = load_backlog_notes(&local_path)?;
        entries.push(LocalBacklogEntry {
            identifier: metadata.identifier.clone(),
            local_path: local_path
                .strip_prefix(root)
                .unwrap_or(&local_path)
                .to_path_buf(),
            metadata,
            notes,
        });
    }

    entries.sort_by(|left, right| left.identifier.cmp(&right.identifier));

    if !selected.is_empty() {
        let discovered = entries
            .iter()
            .map(|entry| entry.identifier.clone())
            .collect::<BTreeSet<_>>();
        let missing = selected
            .into_iter()
            .filter(|identifier| !discovered.contains(identifier))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            bail!(
                "backlog items {} were not found under `{}`",
                missing
                    .into_iter()
                    .map(|identifier| format!("`{identifier}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                paths.backlog_dir.display()
            );
        }
    }

    Ok(entries)
}

fn load_backlog_notes(issue_dir: &Path) -> Result<Vec<BacklogNote>> {
    let mut notes = Vec::new();
    for file_name in [
        "index.md",
        "implementation.md",
        "specification.md",
        "decisions.md",
        "risks.md",
        "validation.md",
    ] {
        let path = issue_dir.join(file_name);
        if !path.is_file() {
            continue;
        }
        notes.push(BacklogNote {
            contents: fs::read_to_string(&path)
                .with_context(|| format!("failed to read `{}`", path.display()))?,
        });
    }
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_entry(identifier: &str) -> LocalBacklogEntry {
        LocalBacklogEntry {
            identifier: identifier.to_string(),
            local_path: PathBuf::from(format!(".metastack/backlog/{identifier}")),
            metadata: BacklogIssueMetadata {
                issue_id: format!("issue-{identifier}"),
                identifier: identifier.to_string(),
                title: format!("Issue {identifier}"),
                url: format!("https://linear.app/{identifier}"),
                team_key: "MET".to_string(),
                ..BacklogIssueMetadata::default()
            },
            notes: Vec::new(),
        }
    }

    fn live_issue(identifier: &str, priority: Option<u8>) -> IssueSummary {
        IssueSummary {
            id: format!("id-{identifier}"),
            identifier: identifier.to_string(),
            title: format!("Issue {identifier}"),
            description: None,
            url: format!("https://linear.app/{identifier}"),
            priority,
            estimate: None,
            updated_at: "2026-03-20T00:00:00Z".to_string(),
            team: crate::linear::TeamRef {
                id: "team-1".to_string(),
                key: "MET".to_string(),
                name: "Metastack".to_string(),
            },
            project: None,
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: None,
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        }
    }

    #[test]
    fn dependency_mentions_are_inferred_from_local_notes() {
        let mut entry = local_entry("MET-11");
        entry.notes.push(BacklogNote {
            contents: "This rollout depends on MET-10 before MET-11 can ship.".to_string(),
        });
        let backlog = vec![local_entry("MET-10"), entry];
        let map = infer_dependencies(
            &backlog,
            &BTreeMap::new(),
            &backlog
                .iter()
                .map(|entry| entry.identifier.clone())
                .collect::<BTreeSet<_>>(),
        );

        assert_eq!(
            map.get("MET-11").cloned().unwrap_or_default(),
            BTreeSet::from(["MET-10".to_string()])
        );
    }

    #[test]
    fn topological_order_prefers_dependencies_before_priority() {
        let mut second = local_entry("MET-11");
        second.notes.push(BacklogNote {
            contents: "blocked by MET-10".to_string(),
        });
        let backlog = vec![second, local_entry("MET-10"), local_entry("MET-12")];
        let identifiers = backlog
            .iter()
            .map(|entry| entry.identifier.clone())
            .collect::<BTreeSet<_>>();
        let deps = infer_dependencies(&backlog, &BTreeMap::new(), &identifiers);
        let issues = BTreeMap::from([
            ("MET-10".to_string(), live_issue("MET-10", Some(3))),
            ("MET-11".to_string(), live_issue("MET-11", Some(1))),
            ("MET-12".to_string(), live_issue("MET-12", Some(2))),
        ]);

        assert_eq!(
            topological_order(&backlog, &issues, &deps),
            vec![
                "MET-10".to_string(),
                "MET-11".to_string(),
                "MET-12".to_string()
            ]
        );
    }

    #[test]
    fn must_have_closure_promotes_dependencies_of_high_priority_work() {
        let ordered = vec!["MET-10".to_string(), "MET-11".to_string()];
        let issues = BTreeMap::from([
            ("MET-10".to_string(), live_issue("MET-10", Some(4))),
            ("MET-11".to_string(), live_issue("MET-11", Some(2))),
        ]);
        let deps = BTreeMap::from([("MET-11".to_string(), BTreeSet::from(["MET-10".to_string()]))]);

        assert_eq!(
            infer_must_have(&ordered, &issues, &deps),
            BTreeSet::from(["MET-10".to_string(), "MET-11".to_string()])
        );
    }
}

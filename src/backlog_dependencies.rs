use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;

use crate::backlog::{BacklogIssueMetadata, INDEX_FILE_NAME, load_issue_metadata};
use crate::cli::{BacklogDependenciesArgs, LinearClientArgs};
use crate::config::load_required_planning_meta;
use crate::fs::{PlanningPaths, canonicalize_existing_dir, display_path};
use crate::linear::{
    IssueDependencySnapshot, IssueEditSpec, IssueRelationCreateRequest, IssueRelationType,
    IssueRelationUpdateRequest,
};
use crate::output::render_json_success;
use crate::{LinearCommandContext, load_linear_command_context};

#[derive(Debug, Clone)]
struct BacklogDependencyItem {
    slug: String,
    issue_dir: PathBuf,
    identifier: String,
    title: String,
    metadata: Option<BacklogIssueMetadata>,
    parent_identifier: Option<String>,
    content_lines: Vec<String>,
    remote: Option<IssueDependencySnapshot>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
enum RelationshipKind {
    Parent,
    BlockedBy,
    Related,
    SoftSequence,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum ChangeAction {
    Create,
    Update,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct RelationshipProposal {
    kind: RelationshipKind,
    issue: String,
    related_issue: String,
    reason: String,
    source: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
struct RelationshipChange {
    action: ChangeAction,
    kind: RelationshipKind,
    issue: String,
    related_issue: String,
    reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ParallelWorkstream {
    wave: usize,
    group: String,
    issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DependencyAnalysisResult {
    root: String,
    fetched_linear_metadata: bool,
    items: Vec<DependencyItemSummary>,
    proposals: Vec<RelationshipProposal>,
    changes: Vec<RelationshipChange>,
    warnings: Vec<String>,
    rollout_order: Vec<Vec<String>>,
    parallel_workstreams: Vec<ParallelWorkstream>,
}

#[derive(Debug, Clone, Serialize)]
struct DependencyItemSummary {
    slug: String,
    identifier: String,
    title: String,
    backlog_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<String>,
}

/// Analyze repo-scoped backlog packets and optionally apply proposed dependency relationships.
///
/// Returns an error when the repository root or local backlog packets cannot be read, when apply
/// mode is requested without the required Linear metadata, or when confirmation is required in a
/// non-interactive session without `--yes`.
pub async fn run_backlog_dependencies(args: &BacklogDependenciesArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.client.root)?;
    let _planning_meta = load_required_planning_meta(&root, "backlog dependencies")?;
    let remote_enabled = args.fetch || args.apply;
    let mut items = load_backlog_dependency_items(&root)?;
    if remote_enabled {
        enrich_dependency_items(&args.client, &mut items).await?;
    }

    let analysis = analyze_dependencies(&root, &items, remote_enabled);
    if args.apply {
        let preview = render_apply_preview(&analysis);
        if !analysis.warnings.is_empty() {
            bail!(
                "refusing to apply `meta backlog dependencies` while warnings remain:\n{}",
                analysis
                    .warnings
                    .iter()
                    .map(|warning| format!("- {warning}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
        if analysis.changes.is_empty() {
            if args.json {
                println!(
                    "{}",
                    render_json_success("backlog.dependencies", &analysis)?
                );
            } else {
                println!("{preview}");
                println!("No relationship changes need to be applied.");
            }
            return Ok(());
        }
        if !args.yes && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
            bail!("`meta backlog dependencies --apply` requires confirmation in a TTY or `--yes`");
        }
        if !args.yes && !confirm_apply(&preview)? {
            if args.json {
                println!(
                    "{}",
                    render_json_success("backlog.dependencies", &analysis)?
                );
            } else {
                println!("Apply canceled.");
            }
            return Ok(());
        }
        apply_changes(&args.client, &items, &analysis.changes).await?;
    }

    if args.json {
        println!(
            "{}",
            render_json_success("backlog.dependencies", &analysis)?
        );
    } else {
        println!("{}", render_analysis(&analysis, args.apply));
    }

    Ok(())
}

fn load_backlog_dependency_items(root: &Path) -> Result<Vec<BacklogDependencyItem>> {
    let paths = PlanningPaths::new(root);
    if !paths.backlog_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();
    for entry in fs::read_dir(&paths.backlog_dir)
        .with_context(|| format!("failed to read `{}`", paths.backlog_dir.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to traverse `{}`", paths.backlog_dir.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to read `{}`", entry.path().display()))?;
        if !file_type.is_dir() {
            continue;
        }

        let slug = entry.file_name().to_string_lossy().to_string();
        if slug == "_TEMPLATE" {
            continue;
        }

        let issue_dir = entry.path();
        let metadata = load_issue_metadata_if_present(&issue_dir)?;
        let index_path = issue_dir.join(INDEX_FILE_NAME);
        let index_text = read_optional_text_file(&index_path)?;
        let title = metadata
            .as_ref()
            .map(|entry| entry.title.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| first_heading(&index_text))
            .unwrap_or_else(|| slug.clone());
        let identifier = metadata
            .as_ref()
            .map(|entry| entry.identifier.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| slug.clone());
        let parent_identifier = metadata
            .as_ref()
            .and_then(|entry| entry.parent_identifier.clone())
            .or_else(|| parent_identifier_from_text(&index_text));
        let content_lines = collect_markdown_lines(&issue_dir)?;

        items.push(BacklogDependencyItem {
            slug,
            issue_dir,
            identifier,
            title,
            metadata,
            parent_identifier,
            content_lines,
            remote: None,
        });
    }

    items.sort_by(|left, right| left.identifier.cmp(&right.identifier));
    Ok(items)
}

async fn enrich_dependency_items(
    client_args: &LinearClientArgs,
    items: &mut [BacklogDependencyItem],
) -> Result<()> {
    let LinearCommandContext { service, .. } = load_linear_command_context(client_args, None)?;
    for item in items.iter_mut() {
        let Some(metadata) = item.metadata.as_ref() else {
            continue;
        };
        let snapshot = service
            .load_issue_dependency_snapshot(&metadata.identifier)
            .await
            .with_context(|| format!("failed to load Linear issue `{}`", metadata.identifier))?;
        item.parent_identifier = snapshot
            .issue
            .parent
            .as_ref()
            .map(|parent| parent.identifier.clone())
            .or_else(|| item.parent_identifier.clone());
        item.remote = Some(snapshot);
    }
    Ok(())
}

fn analyze_dependencies(
    root: &Path,
    items: &[BacklogDependencyItem],
    fetched_linear_metadata: bool,
) -> DependencyAnalysisResult {
    let item_map = items
        .iter()
        .map(|item| (item.identifier.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let mut proposals = BTreeSet::new();
    let mut warnings = BTreeSet::new();
    let mut hard_edges = BTreeSet::new();
    let mut soft_edges = BTreeSet::new();

    for item in items {
        if let Some(parent_identifier) = item.parent_identifier.as_ref() {
            if parent_identifier.eq_ignore_ascii_case(&item.identifier) {
                warnings.insert(format!(
                    "parent cycle: {} cannot be its own parent",
                    item.identifier
                ));
            } else if item_map.contains_key(parent_identifier) {
                proposals.insert(RelationshipProposal {
                    kind: RelationshipKind::Parent,
                    issue: item.identifier.clone(),
                    related_issue: parent_identifier.clone(),
                    reason: "existing backlog packet parent relationship".to_string(),
                    source: "local backlog metadata".to_string(),
                });
            } else {
                warnings.insert(format!(
                    "parent reference `{parent_identifier}` from `{}` does not exist in the local backlog set",
                    item.identifier
                ));
            }
        }

        for line in &item.content_lines {
            let references = extract_issue_identifiers(line)
                .into_iter()
                .filter(|identifier| !identifier.eq_ignore_ascii_case(&item.identifier))
                .collect::<BTreeSet<_>>();
            if references.is_empty() {
                continue;
            }

            let lower = line.to_ascii_lowercase();
            let relationship = classify_line_relationship(&lower);
            for reference in references {
                if !item_map.contains_key(&reference) {
                    warnings.insert(format!(
                        "reference `{reference}` from `{}` is outside the local backlog set",
                        item.identifier
                    ));
                    continue;
                }
                match relationship {
                    Some(RelationshipKind::BlockedBy) => {
                        proposals.insert(RelationshipProposal {
                            kind: RelationshipKind::BlockedBy,
                            issue: item.identifier.clone(),
                            related_issue: reference.clone(),
                            reason: line.trim().to_string(),
                            source: "local backlog text".to_string(),
                        });
                        hard_edges.insert((reference, item.identifier.clone()));
                    }
                    Some(RelationshipKind::Related) => {
                        proposals.insert(RelationshipProposal {
                            kind: RelationshipKind::Related,
                            issue: item.identifier.clone(),
                            related_issue: reference,
                            reason: line.trim().to_string(),
                            source: "local backlog text".to_string(),
                        });
                    }
                    Some(RelationshipKind::SoftSequence) => {
                        proposals.insert(RelationshipProposal {
                            kind: RelationshipKind::SoftSequence,
                            issue: item.identifier.clone(),
                            related_issue: reference.clone(),
                            reason: line.trim().to_string(),
                            source: "local backlog text".to_string(),
                        });
                        soft_edges.insert((reference, item.identifier.clone()));
                    }
                    _ => {}
                }
            }
        }
    }

    let fallback_soft_sequences = build_fallback_soft_sequences(items, &hard_edges);
    for proposal in fallback_soft_sequences {
        soft_edges.insert((proposal.related_issue.clone(), proposal.issue.clone()));
        proposals.insert(proposal);
    }

    warnings.extend(detect_cycles(
        &collect_nodes(items),
        &hard_edges,
        "hard blocker cycle",
    ));
    warnings.extend(detect_cycles(
        &collect_nodes(items),
        &hard_edges.union(&soft_edges).cloned().collect(),
        "combined dependency cycle",
    ));

    let rollout_order = topological_waves(items, &hard_edges);
    let parallel_workstreams = build_parallel_workstreams(items, &rollout_order, &hard_edges);
    let changes = build_changes(items, &proposals, &warnings);
    let proposal_list = proposals.into_iter().collect::<Vec<_>>();
    let warning_list = warnings.into_iter().collect::<Vec<_>>();
    let summaries = items
        .iter()
        .map(|item| DependencyItemSummary {
            slug: item.slug.clone(),
            identifier: item.identifier.clone(),
            title: item.title.clone(),
            backlog_path: display_path(&item.issue_dir, root),
            parent_identifier: item.parent_identifier.clone(),
            state: item
                .remote
                .as_ref()
                .and_then(|snapshot| {
                    snapshot
                        .issue
                        .state
                        .as_ref()
                        .map(|state| state.name.clone())
                })
                .or_else(|| state_from_metadata(item.metadata.as_ref())),
        })
        .collect::<Vec<_>>();

    DependencyAnalysisResult {
        root: root.display().to_string(),
        fetched_linear_metadata,
        items: summaries,
        proposals: proposal_list,
        changes,
        warnings: warning_list,
        rollout_order,
        parallel_workstreams,
    }
}

fn build_changes(
    items: &[BacklogDependencyItem],
    proposals: &BTreeSet<RelationshipProposal>,
    warnings: &BTreeSet<String>,
) -> Vec<RelationshipChange> {
    let mut changes = BTreeSet::new();
    if !warnings.is_empty() {
        return Vec::new();
    }

    let mut relation_lookup = BTreeMap::new();
    let mut symmetric_lookup = BTreeMap::new();
    let mut parent_lookup = BTreeMap::new();
    for item in items {
        if let Some(snapshot) = item.remote.as_ref() {
            if let Some(parent) = snapshot.issue.parent.as_ref() {
                parent_lookup.insert(snapshot.issue.identifier.clone(), parent.identifier.clone());
            }
            for relation in &snapshot.relations {
                relation_lookup.insert(
                    (
                        relation.issue.identifier.clone(),
                        relation.related_issue.identifier.clone(),
                    ),
                    relation,
                );
                symmetric_lookup.insert(
                    normalized_pair(
                        &relation.issue.identifier,
                        &relation.related_issue.identifier,
                    ),
                    relation,
                );
            }
        }
    }

    for proposal in proposals {
        match proposal.kind {
            RelationshipKind::Parent => {
                let current_parent = parent_lookup.get(&proposal.issue);
                if current_parent != Some(&proposal.related_issue) {
                    changes.insert(RelationshipChange {
                        action: ChangeAction::Update,
                        kind: proposal.kind,
                        issue: proposal.issue.clone(),
                        related_issue: proposal.related_issue.clone(),
                        reason: proposal.reason.clone(),
                        relation_id: None,
                    });
                }
            }
            RelationshipKind::BlockedBy => {
                let desired_pair = (proposal.related_issue.clone(), proposal.issue.clone());
                match relation_lookup.get(&desired_pair) {
                    Some(existing) if existing.relation_type == IssueRelationType::Blocks => {}
                    Some(existing) => {
                        changes.insert(RelationshipChange {
                            action: ChangeAction::Update,
                            kind: proposal.kind,
                            issue: proposal.issue.clone(),
                            related_issue: proposal.related_issue.clone(),
                            reason: proposal.reason.clone(),
                            relation_id: Some(existing.id.clone()),
                        });
                    }
                    None => {
                        changes.insert(RelationshipChange {
                            action: ChangeAction::Create,
                            kind: proposal.kind,
                            issue: proposal.issue.clone(),
                            related_issue: proposal.related_issue.clone(),
                            reason: proposal.reason.clone(),
                            relation_id: None,
                        });
                    }
                }
            }
            RelationshipKind::Related => {
                let key = normalized_pair(&proposal.issue, &proposal.related_issue);
                match symmetric_lookup.get(&key) {
                    Some(existing) if existing.relation_type == IssueRelationType::Related => {}
                    Some(existing) => {
                        changes.insert(RelationshipChange {
                            action: ChangeAction::Update,
                            kind: proposal.kind,
                            issue: proposal.issue.clone(),
                            related_issue: proposal.related_issue.clone(),
                            reason: proposal.reason.clone(),
                            relation_id: Some(existing.id.clone()),
                        });
                    }
                    None => {
                        changes.insert(RelationshipChange {
                            action: ChangeAction::Create,
                            kind: proposal.kind,
                            issue: proposal.issue.clone(),
                            related_issue: proposal.related_issue.clone(),
                            reason: proposal.reason.clone(),
                            relation_id: None,
                        });
                    }
                }
            }
            RelationshipKind::SoftSequence => {}
        }
    }

    changes.into_iter().collect()
}

async fn apply_changes(
    client_args: &LinearClientArgs,
    items: &[BacklogDependencyItem],
    changes: &[RelationshipChange],
) -> Result<()> {
    let LinearCommandContext { service, .. } = load_linear_command_context(client_args, None)?;
    let snapshot_lookup = items
        .iter()
        .filter_map(|item| {
            item.remote
                .as_ref()
                .map(|snapshot| (snapshot.issue.identifier.clone(), snapshot))
        })
        .collect::<BTreeMap<_, _>>();

    for change in changes {
        match change.kind {
            RelationshipKind::Parent => {
                let issue = snapshot_lookup.get(&change.issue).ok_or_else(|| {
                    anyhow!(
                        "cannot apply parent update for `{}` without Linear metadata",
                        change.issue
                    )
                })?;
                let parent = snapshot_lookup.get(&change.related_issue).ok_or_else(|| {
                    anyhow!(
                        "cannot apply parent update because `{}` is missing Linear metadata",
                        change.related_issue
                    )
                })?;
                service
                    .edit_issue(IssueEditSpec {
                        identifier: issue.issue.identifier.clone(),
                        title: None,
                        description: None,
                        project: None,
                        state: None,
                        priority: None,
                        estimate: None,
                        labels: None,
                        parent_identifier: Some(parent.issue.identifier.clone()),
                    })
                    .await
                    .with_context(|| {
                        format!(
                            "failed to update parent for `{}` to `{}`",
                            change.issue, change.related_issue
                        )
                    })?;
            }
            RelationshipKind::BlockedBy | RelationshipKind::Related => {
                let issue = snapshot_lookup.get(&change.issue).ok_or_else(|| {
                    anyhow!(
                        "cannot apply relationship for `{}` without Linear metadata",
                        change.issue
                    )
                })?;
                let related = snapshot_lookup.get(&change.related_issue).ok_or_else(|| {
                    anyhow!(
                        "cannot apply relationship because `{}` is missing Linear metadata",
                        change.related_issue
                    )
                })?;
                match (change.action, change.kind) {
                    (ChangeAction::Create, RelationshipKind::BlockedBy) => {
                        service
                            .create_issue_relation(IssueRelationCreateRequest {
                                relation_type: IssueRelationType::Blocks,
                                issue_id: related.issue.id.clone(),
                                related_issue_id: issue.issue.id.clone(),
                            })
                            .await
                            .with_context(|| {
                                format!(
                                    "failed to create blocker `{}` -> `{}`",
                                    change.related_issue, change.issue
                                )
                            })?;
                    }
                    (ChangeAction::Update, RelationshipKind::BlockedBy) => {
                        service
                            .update_issue_relation(
                                change.relation_id.as_deref().ok_or_else(|| {
                                    anyhow!("missing relation id for blocker update")
                                })?,
                                IssueRelationUpdateRequest {
                                    relation_type: Some(IssueRelationType::Blocks),
                                    issue_id: Some(related.issue.id.clone()),
                                    related_issue_id: Some(issue.issue.id.clone()),
                                },
                            )
                            .await
                            .with_context(|| {
                                format!(
                                    "failed to update blocker `{}` -> `{}`",
                                    change.related_issue, change.issue
                                )
                            })?;
                    }
                    (ChangeAction::Create, RelationshipKind::Related) => {
                        let (left, right) = ordered_pair(issue, related);
                        service
                            .create_issue_relation(IssueRelationCreateRequest {
                                relation_type: IssueRelationType::Related,
                                issue_id: left.issue.id.clone(),
                                related_issue_id: right.issue.id.clone(),
                            })
                            .await
                            .with_context(|| {
                                format!(
                                    "failed to create related link between `{}` and `{}`",
                                    change.issue, change.related_issue
                                )
                            })?;
                    }
                    (ChangeAction::Update, RelationshipKind::Related) => {
                        service
                            .update_issue_relation(
                                change.relation_id.as_deref().ok_or_else(|| {
                                    anyhow!("missing relation id for related update")
                                })?,
                                IssueRelationUpdateRequest {
                                    relation_type: Some(IssueRelationType::Related),
                                    issue_id: None,
                                    related_issue_id: None,
                                },
                            )
                            .await
                            .with_context(|| {
                                format!(
                                    "failed to update related link between `{}` and `{}`",
                                    change.issue, change.related_issue
                                )
                            })?;
                    }
                    _ => {}
                }
            }
            RelationshipKind::SoftSequence => {}
        }
    }

    Ok(())
}

fn render_analysis(analysis: &DependencyAnalysisResult, applied: bool) -> String {
    let mut lines = vec![
        format!(
            "meta backlog dependencies {}",
            if applied { "applied" } else { "preview" }
        ),
        format!("Repository: {}", analysis.root),
        format!("Backlog items: {}", analysis.items.len()),
        format!(
            "Linear metadata: {}",
            if analysis.fetched_linear_metadata {
                "fetched"
            } else {
                "local-only"
            }
        ),
        String::new(),
        "Proposed relationships:".to_string(),
    ];

    if analysis.proposals.is_empty() {
        lines.push("- none".to_string());
    } else {
        for proposal in &analysis.proposals {
            lines.push(format!(
                "- {:?}: {} -> {} ({})",
                proposal.kind, proposal.issue, proposal.related_issue, proposal.reason
            ));
        }
    }

    lines.push(String::new());
    lines.push("Rollout order:".to_string());
    if analysis.rollout_order.is_empty() {
        lines.push("- none".to_string());
    } else {
        for (index, wave) in analysis.rollout_order.iter().enumerate() {
            lines.push(format!("- wave {}: {}", index + 1, wave.join(", ")));
        }
    }

    lines.push(String::new());
    lines.push("Parallel workstreams:".to_string());
    if analysis.parallel_workstreams.is_empty() {
        lines.push("- none".to_string());
    } else {
        for workstream in &analysis.parallel_workstreams {
            lines.push(format!(
                "- wave {} [{}]: {}",
                workstream.wave,
                workstream.group,
                workstream.issues.join(", ")
            ));
        }
    }

    lines.push(String::new());
    lines.push("Warnings:".to_string());
    if analysis.warnings.is_empty() {
        lines.push("- none".to_string());
    } else {
        for warning in &analysis.warnings {
            lines.push(format!("- {warning}"));
        }
    }

    if !analysis.changes.is_empty() {
        lines.push(String::new());
        lines.push(if applied {
            "Applied relationship changes:".to_string()
        } else {
            "Apply preview:".to_string()
        });
        for change in &analysis.changes {
            lines.push(format!(
                "- {:?} {:?}: {} -> {}",
                change.action, change.kind, change.issue, change.related_issue
            ));
        }
    }

    lines.join("\n")
}

fn render_apply_preview(analysis: &DependencyAnalysisResult) -> String {
    let mut lines = vec![
        "meta backlog dependencies apply preview".to_string(),
        format!("Repository: {}", analysis.root),
        String::new(),
    ];
    for change in &analysis.changes {
        lines.push(format!(
            "- {:?} {:?}: {} -> {} ({})",
            change.action, change.kind, change.issue, change.related_issue, change.reason
        ));
    }
    lines.join("\n")
}

fn confirm_apply(preview: &str) -> Result<bool> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    writeln!(writer, "{preview}")?;
    writeln!(
        writer,
        "Choose [a]pply or [c]ancel for `meta backlog dependencies --apply`:"
    )?;
    writer.flush()?;

    let mut input = String::new();
    loop {
        input.clear();
        reader.read_line(&mut input)?;
        match input.trim().to_ascii_lowercase().as_str() {
            "a" | "apply" => return Ok(true),
            "c" | "cancel" => return Ok(false),
            _ => {
                writeln!(writer, "Enter `a` or `c`:")?;
                writer.flush()?;
            }
        }
    }
}

fn state_from_metadata(metadata: Option<&BacklogIssueMetadata>) -> Option<String> {
    metadata.and_then(|entry| {
        entry
            .last_sync_at
            .as_ref()
            .map(|_| "linked".to_string())
            .or(Some("local".to_string()))
    })
}

fn build_fallback_soft_sequences(
    items: &[BacklogDependencyItem],
    hard_edges: &BTreeSet<(String, String)>,
) -> Vec<RelationshipProposal> {
    let mut grouped = BTreeMap::<String, Vec<&BacklogDependencyItem>>::new();
    for item in items {
        let key = item
            .parent_identifier
            .clone()
            .unwrap_or_else(|| workstream_key(&item.title));
        grouped.entry(key).or_default().push(item);
    }

    let mut proposals = Vec::new();
    for (group, mut group_items) in grouped {
        group_items.sort_by(|left, right| left.identifier.cmp(&right.identifier));
        for window in group_items.windows(2) {
            let [previous, current] = window else {
                continue;
            };
            let edge = (previous.identifier.clone(), current.identifier.clone());
            let reverse = (current.identifier.clone(), previous.identifier.clone());
            if hard_edges.contains(&edge) || hard_edges.contains(&reverse) {
                continue;
            }
            proposals.push(RelationshipProposal {
                kind: RelationshipKind::SoftSequence,
                issue: current.identifier.clone(),
                related_issue: previous.identifier.clone(),
                reason: format!("deterministic rollout fallback within `{group}`"),
                source: "fallback sequencing".to_string(),
            });
        }
    }
    proposals.sort_by(|left, right| {
        (
            left.issue.as_str(),
            left.related_issue.as_str(),
            left.reason.as_str(),
        )
            .cmp(&(
                right.issue.as_str(),
                right.related_issue.as_str(),
                right.reason.as_str(),
            ))
    });
    proposals
}

fn build_parallel_workstreams(
    items: &[BacklogDependencyItem],
    rollout_order: &[Vec<String>],
    hard_edges: &BTreeSet<(String, String)>,
) -> Vec<ParallelWorkstream> {
    let item_lookup = items
        .iter()
        .map(|item| (item.identifier.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let mut workstreams = Vec::new();
    for (wave_index, wave) in rollout_order.iter().enumerate() {
        let mut grouped = BTreeMap::<String, Vec<String>>::new();
        for identifier in wave {
            let Some(item) = item_lookup.get(identifier) else {
                continue;
            };
            let key = item
                .parent_identifier
                .clone()
                .unwrap_or_else(|| workstream_key(&item.title));
            grouped.entry(key).or_default().push(identifier.clone());
        }
        for (group, issues) in grouped {
            if issues.len() < 2 {
                continue;
            }
            let has_hard_dependency = issues.iter().enumerate().any(|(index, left)| {
                issues.iter().skip(index + 1).any(|right| {
                    hard_edges.contains(&(left.clone(), right.clone()))
                        || hard_edges.contains(&(right.clone(), left.clone()))
                })
            });
            if !has_hard_dependency {
                workstreams.push(ParallelWorkstream {
                    wave: wave_index + 1,
                    group,
                    issues,
                });
            }
        }
    }
    workstreams
}

fn collect_nodes(items: &[BacklogDependencyItem]) -> Vec<String> {
    items.iter().map(|item| item.identifier.clone()).collect()
}

fn topological_waves(
    items: &[BacklogDependencyItem],
    edges: &BTreeSet<(String, String)>,
) -> Vec<Vec<String>> {
    let mut indegree = items
        .iter()
        .map(|item| (item.identifier.clone(), 0usize))
        .collect::<BTreeMap<_, _>>();
    let mut adjacency = BTreeMap::<String, BTreeSet<String>>::new();
    for (source, target) in edges {
        if let Some(entry) = indegree.get_mut(target) {
            *entry += 1;
        }
        adjacency
            .entry(source.clone())
            .or_default()
            .insert(target.clone());
    }

    let mut ready = indegree
        .iter()
        .filter_map(|(identifier, degree)| (*degree == 0).then_some(identifier.clone()))
        .collect::<VecDeque<_>>();
    let mut waves = Vec::new();
    let mut seen = BTreeSet::new();
    while !ready.is_empty() {
        let mut wave = Vec::new();
        let mut current = ready.into_iter().collect::<Vec<_>>();
        current.sort();
        ready = VecDeque::new();
        for identifier in current {
            if !seen.insert(identifier.clone()) {
                continue;
            }
            wave.push(identifier.clone());
            if let Some(targets) = adjacency.get(&identifier) {
                for target in targets {
                    if let Some(entry) = indegree.get_mut(target) {
                        *entry = entry.saturating_sub(1);
                        if *entry == 0 {
                            ready.push_back(target.clone());
                        }
                    }
                }
            }
        }
        if !wave.is_empty() {
            waves.push(wave);
        }
    }

    let mut remaining = indegree
        .into_iter()
        .filter_map(|(identifier, degree)| {
            (degree > 0 && !seen.contains(&identifier)).then_some(identifier)
        })
        .collect::<Vec<_>>();
    remaining.sort();
    if !remaining.is_empty() {
        waves.push(remaining);
    }
    waves
}

fn detect_cycles(
    nodes: &[String],
    edges: &BTreeSet<(String, String)>,
    label: &str,
) -> BTreeSet<String> {
    let adjacency = edges
        .iter()
        .fold(BTreeMap::<String, Vec<String>>::new(), |mut map, edge| {
            map.entry(edge.0.clone()).or_default().push(edge.1.clone());
            map
        });
    let mut warnings = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut stack = Vec::new();
    let mut active = BTreeSet::new();

    for node in nodes {
        dfs_cycle(
            node,
            &adjacency,
            &mut visited,
            &mut active,
            &mut stack,
            &mut warnings,
            label,
        );
    }

    warnings
}

fn dfs_cycle(
    node: &str,
    adjacency: &BTreeMap<String, Vec<String>>,
    visited: &mut BTreeSet<String>,
    active: &mut BTreeSet<String>,
    stack: &mut Vec<String>,
    warnings: &mut BTreeSet<String>,
    label: &str,
) {
    if !visited.insert(node.to_string()) {
        return;
    }
    active.insert(node.to_string());
    stack.push(node.to_string());

    let mut targets = adjacency.get(node).cloned().unwrap_or_default();
    targets.sort();
    for target in targets {
        if active.contains(&target) {
            if let Some(index) = stack.iter().position(|entry| entry == &target) {
                let mut cycle = stack[index..].to_vec();
                cycle.push(target.clone());
                warnings.insert(format!("{label}: {}", cycle.join(" -> ")));
            }
            continue;
        }
        if !visited.contains(&target) {
            dfs_cycle(&target, adjacency, visited, active, stack, warnings, label);
        }
    }

    active.remove(node);
    let _ = stack.pop();
}

fn classify_line_relationship(lower: &str) -> Option<RelationshipKind> {
    if lower.contains("parent:") || lower.contains("parent issue:") {
        return Some(RelationshipKind::Parent);
    }
    if lower.contains("blocked by")
        || lower.contains("depends on")
        || lower.contains("waiting on")
        || lower.contains("requires ")
        || lower.contains("after ")
    {
        return Some(RelationshipKind::BlockedBy);
    }
    if lower.contains("related") || lower.contains("companion") || lower.contains("alongside") {
        return Some(RelationshipKind::Related);
    }
    if lower.contains("sequence")
        || lower.contains("rollout")
        || lower.contains("follow")
        || lower.contains("later")
        || lower.contains("next")
    {
        return Some(RelationshipKind::SoftSequence);
    }
    None
}

fn extract_issue_identifiers(line: &str) -> Vec<String> {
    let chars = line.chars().collect::<Vec<_>>();
    let mut identifiers = BTreeSet::new();
    let mut index = 0usize;
    while index < chars.len() {
        if !chars[index].is_ascii_uppercase() {
            index += 1;
            continue;
        }
        let start = index;
        while index < chars.len() && chars[index].is_ascii_alphanumeric() {
            index += 1;
        }
        if index >= chars.len() || chars[index] != '-' {
            continue;
        }
        index += 1;
        let digits_start = index;
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
        if digits_start == index {
            continue;
        }
        let candidate = chars[start..index].iter().collect::<String>();
        let prefix_len = candidate
            .split_once('-')
            .map(|(prefix, _)| prefix.len())
            .unwrap_or_default();
        if prefix_len < 2 {
            continue;
        }
        identifiers.insert(candidate);
    }
    identifiers.into_iter().collect()
}

fn collect_markdown_lines(issue_dir: &Path) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for entry in walkdir::WalkDir::new(issue_dir) {
        let entry =
            entry.with_context(|| format!("failed to traverse `{}`", issue_dir.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.starts_with('.'))
        {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let text = fs::read_to_string(path)
            .with_context(|| format!("failed to read `{}`", path.display()))?;
        lines.extend(text.lines().map(ToOwned::to_owned));
    }
    Ok(lines)
}

fn first_heading(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn parent_identifier_from_text(markdown: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        if lower.contains("parent:") || lower.contains("parent issue:") {
            extract_issue_identifiers(line).into_iter().next()
        } else {
            None
        }
    })
}

fn read_optional_text_file(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

fn load_issue_metadata_if_present(issue_dir: &Path) -> Result<Option<BacklogIssueMetadata>> {
    let path = issue_dir.join(".linear.json");
    if !path.is_file() {
        return Ok(None);
    }
    load_issue_metadata(issue_dir).map(Some)
}

fn normalized_pair(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_string(), right.to_string())
    } else {
        (right.to_string(), left.to_string())
    }
}

fn ordered_pair<'a>(
    left: &'a IssueDependencySnapshot,
    right: &'a IssueDependencySnapshot,
) -> (&'a IssueDependencySnapshot, &'a IssueDependencySnapshot) {
    if left.issue.identifier <= right.issue.identifier {
        (left, right)
    } else {
        (right, left)
    }
}

fn workstream_key(title: &str) -> String {
    title
        .split(':')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("top-level")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{
        RelationshipKind, classify_line_relationship, extract_issue_identifiers, normalized_pair,
    };

    #[test]
    fn extract_issue_identifiers_finds_linear_style_tokens() {
        assert_eq!(
            extract_issue_identifiers("Blocked by MET-12 and related to ENG2-7"),
            vec!["ENG2-7".to_string(), "MET-12".to_string()]
        );
    }

    #[test]
    fn relationship_classifier_prioritizes_blockers() {
        assert_eq!(
            classify_line_relationship(
                "Blocked by MET-12 and related to MET-13"
                    .to_ascii_lowercase()
                    .as_str()
            ),
            Some(RelationshipKind::BlockedBy)
        );
    }

    #[test]
    fn normalized_pair_is_stable() {
        assert_eq!(
            normalized_pair("MET-12", "MET-7"),
            ("MET-12".to_string(), "MET-7".to_string())
        );
    }
}

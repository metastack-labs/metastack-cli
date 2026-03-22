use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use time::OffsetDateTime;

use crate::backlog::{BacklogIssueMetadata, INDEX_FILE_NAME, load_issue_metadata};
use crate::cli::{BacklogReleaseArgs, LinearClientArgs};
use crate::config::load_required_planning_meta;
use crate::fs::{PlanningPaths, canonicalize_existing_dir, display_path, write_text_file};
use crate::output::render_json_success;
use crate::{LinearCommandContext, load_linear_command_context};

/// Priority threshold: issues with Linear priority <= this value are considered must-have.
/// Linear priorities: 0 = no priority, 1 = urgent, 2 = high, 3 = medium, 4 = low.
const MUST_HAVE_PRIORITY_THRESHOLD: u8 = 2;

#[derive(Debug, Clone)]
struct ReleaseItem {
    _slug: String,
    issue_dir: PathBuf,
    identifier: String,
    title: String,
    metadata: Option<BacklogIssueMetadata>,
    priority: Option<u8>,
    state: Option<String>,
    estimate: Option<f64>,
    parent_identifier: Option<String>,
    blocked_by: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ReleaseBatch {
    name: String,
    rationale: String,
    issues: Vec<ReleaseBatchItem>,
}

#[derive(Debug, Clone, Serialize)]
struct ReleaseBatchItem {
    identifier: String,
    title: String,
    priority: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    estimate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent_identifier: Option<String>,
    backlog_path: String,
}

#[derive(Debug, Clone, Serialize)]
struct ReleasePlan {
    name: String,
    root: String,
    fetched_linear_metadata: bool,
    total_items: usize,
    batches: Vec<ReleaseBatch>,
    cut_line: CutLine,
    risks: Vec<String>,
    ordering: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CutLine {
    included_count: usize,
    deferred_count: usize,
    rationale: String,
}

/// Analyze repo-scoped backlog items and produce a milestone-ready release plan.
///
/// Returns an error when the repository root or local backlog packets cannot be read, when apply
/// mode is requested without the required Linear metadata, or when confirmation is required in a
/// non-interactive session without `--yes`.
pub async fn run_backlog_release(args: &BacklogReleaseArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.client.root)?;
    let _planning_meta = load_required_planning_meta(&root, "backlog release")?;
    let remote_enabled = args.fetch || args.apply;

    let mut items = load_release_items(&root)?;

    if items.is_empty() {
        let message = "No backlog items found under `.metastack/backlog/`. \
            Create issues with `meta backlog plan` or `meta backlog tech` first.";
        if args.json {
            let empty = ReleasePlan {
                name: resolve_plan_name(args.name.as_deref()),
                root: root.display().to_string(),
                fetched_linear_metadata: false,
                total_items: 0,
                batches: Vec::new(),
                cut_line: CutLine {
                    included_count: 0,
                    deferred_count: 0,
                    rationale: message.to_string(),
                },
                risks: Vec::new(),
                ordering: Vec::new(),
            };
            println!("{}", render_json_success("backlog.release", &empty)?);
        } else {
            println!("{message}");
        }
        return Ok(());
    }

    if remote_enabled {
        enrich_release_items(&args.client, &mut items).await?;
    }

    let plan_name = resolve_plan_name(args.name.as_deref());
    let plan = build_release_plan(&root, &plan_name, &items, remote_enabled);

    if args.apply {
        if plan.batches.is_empty() {
            if args.json {
                println!("{}", render_json_success("backlog.release", &plan)?);
            } else {
                println!("No batches to apply.");
            }
            return Ok(());
        }
        let preview = render_apply_preview(&plan);
        if args.yes && !args.json {
            println!("{preview}");
            println!("Writing release plan because `--yes` was provided.");
        }
        if !args.yes && (!io::stdin().is_terminal() || !io::stdout().is_terminal()) {
            bail!("`meta backlog release --apply` requires confirmation in a TTY or `--yes`");
        }
        if !args.yes && !confirm_apply(&preview)? {
            if args.json {
                println!("{}", render_json_success("backlog.release", &plan)?);
            } else {
                println!("Apply canceled.");
            }
            return Ok(());
        }
    }

    let plan_dir = write_release_plan(&root, &plan)?;

    if args.json {
        println!("{}", render_json_success("backlog.release", &plan)?);
    } else {
        println!("{}", render_plan(&plan, &plan_dir, &root));
    }

    Ok(())
}

fn resolve_plan_name(name: Option<&str>) -> String {
    if let Some(name) = name {
        return name.to_string();
    }
    let now = OffsetDateTime::now_utc();
    format!(
        "release-{:04}-{:02}-{:02}",
        now.year(),
        now.month() as u8,
        now.day()
    )
}

fn load_release_items(root: &Path) -> Result<Vec<ReleaseItem>> {
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
            .map(|m| m.title.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(|| first_heading(&index_text))
            .unwrap_or_else(|| slug.clone());
        let identifier = metadata
            .as_ref()
            .map(|m| m.identifier.clone())
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| slug.clone());
        let parent_identifier = metadata
            .as_ref()
            .and_then(|m| m.parent_identifier.clone())
            .filter(|v| !v.eq_ignore_ascii_case(&identifier));
        let blocked_by = extract_blocked_by(&index_text, &identifier);

        items.push(ReleaseItem {
            _slug: slug,
            issue_dir,
            identifier,
            title,
            metadata,
            priority: None,
            state: None,
            estimate: None,
            parent_identifier,
            blocked_by,
        });
    }

    items.sort_by(|a, b| a.identifier.cmp(&b.identifier));
    Ok(items)
}

async fn enrich_release_items(
    client_args: &LinearClientArgs,
    items: &mut [ReleaseItem],
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
        item.priority = snapshot.issue.priority;
        item.state = snapshot.issue.state.as_ref().map(|s| s.name.clone());
        item.estimate = snapshot.issue.estimate;
        item.parent_identifier = snapshot
            .issue
            .parent
            .as_ref()
            .map(|p| p.identifier.clone())
            .or_else(|| item.parent_identifier.clone());

        // Extract blockedBy from remote relations.
        for rel in &snapshot.inverse_relations {
            if rel.relation_type.as_str() == "blocks" {
                let blocker = &rel.issue.identifier;
                if !item.blocked_by.contains(blocker) {
                    item.blocked_by.push(blocker.clone());
                }
            }
        }
    }
    Ok(())
}

fn build_release_plan(
    root: &Path,
    plan_name: &str,
    items: &[ReleaseItem],
    fetched_linear_metadata: bool,
) -> ReleasePlan {
    let known_identifiers: Vec<String> = items.iter().map(|i| i.identifier.clone()).collect();
    let mut must_have = Vec::new();
    let mut should_have = Vec::new();
    let mut deferred = Vec::new();

    for item in items {
        let batch_item = ReleaseBatchItem {
            identifier: item.identifier.clone(),
            title: item.title.clone(),
            priority: item.priority,
            priority_label: item.priority.map(priority_label),
            state: item.state.clone(),
            estimate: item.estimate,
            parent_identifier: item.parent_identifier.clone(),
            backlog_path: display_path(&item.issue_dir, root),
        };

        // Classify by priority and state.
        let is_done = item
            .state
            .as_ref()
            .is_some_and(|s| s == "Done" || s == "Canceled" || s == "Cancelled");
        if is_done {
            // Skip completed items entirely.
            continue;
        }

        let effective_priority = item.priority.unwrap_or(0);
        if (1..=MUST_HAVE_PRIORITY_THRESHOLD).contains(&effective_priority) {
            must_have.push(batch_item);
        } else if effective_priority == 3 {
            should_have.push(batch_item);
        } else {
            // priority 0 (unset) or 4 (low) — defer.
            deferred.push(batch_item);
        }
    }

    // Build ordering: must-have first, respecting blockedBy within each batch.
    let mut ordering = Vec::new();
    let ordered_must = order_by_dependencies(items, &must_have, &known_identifiers);
    for item in &ordered_must {
        ordering.push(format!(
            "{} ({})",
            item.identifier,
            item.priority_label.as_deref().unwrap_or("must-have")
        ));
    }
    for item in &should_have {
        ordering.push(format!(
            "{} ({})",
            item.identifier,
            item.priority_label.as_deref().unwrap_or("should-have")
        ));
    }
    for item in &deferred {
        ordering.push(format!(
            "{} ({})",
            item.identifier,
            item.priority_label.as_deref().unwrap_or("deferred")
        ));
    }

    // Build risks.
    let mut risks = Vec::new();
    let unestimated = items
        .iter()
        .filter(|i| {
            i.estimate.is_none()
                && !i
                    .state
                    .as_ref()
                    .is_some_and(|s| s == "Done" || s == "Canceled" || s == "Cancelled")
        })
        .count();
    if unestimated > 0 {
        risks.push(format!(
            "{unestimated} issue(s) have no estimate — total scope may be underestimated"
        ));
    }
    let no_priority = items
        .iter()
        .filter(|i| {
            i.priority.unwrap_or(0) == 0
                && !i
                    .state
                    .as_ref()
                    .is_some_and(|s| s == "Done" || s == "Canceled" || s == "Cancelled")
        })
        .count();
    if no_priority > 0 {
        risks.push(format!(
            "{no_priority} issue(s) have no priority — classification is approximate"
        ));
    }
    let has_external_blockers = items
        .iter()
        .any(|i| i.blocked_by.iter().any(|b| !known_identifiers.contains(b)));
    if has_external_blockers {
        risks.push(
            "Some issues are blocked by issues outside this backlog — external coordination required"
                .to_string(),
        );
    }
    if !fetched_linear_metadata {
        risks.push(
            "Analysis used local metadata only (no --fetch). Priority and state may be stale."
                .to_string(),
        );
    }

    let included_count = must_have.len() + should_have.len();
    let deferred_count = deferred.len();

    let cut_rationale = if deferred_count == 0 {
        "All backlog items are included in the release plan.".to_string()
    } else {
        format!(
            "Cut after {included_count} item(s). {deferred_count} low-priority or \
             unprioritized item(s) deferred to a future milestone."
        )
    };

    let mut batches = Vec::new();
    if !must_have.is_empty() {
        batches.push(ReleaseBatch {
            name: "Must-Have".to_string(),
            rationale: "Urgent and high-priority items that must ship in this milestone."
                .to_string(),
            issues: ordered_must,
        });
    }
    if !should_have.is_empty() {
        batches.push(ReleaseBatch {
            name: "Should-Have".to_string(),
            rationale: "Medium-priority items to include if capacity allows.".to_string(),
            issues: should_have,
        });
    }
    if !deferred.is_empty() {
        batches.push(ReleaseBatch {
            name: "Deferred".to_string(),
            rationale: "Low-priority or unprioritized items deferred to a future milestone."
                .to_string(),
            issues: deferred,
        });
    }

    ReleasePlan {
        name: plan_name.to_string(),
        root: root.display().to_string(),
        fetched_linear_metadata,
        total_items: items.len(),
        batches,
        cut_line: CutLine {
            included_count,
            deferred_count,
            rationale: cut_rationale,
        },
        risks,
        ordering,
    }
}

/// Order items within a batch respecting blockedBy relationships via simple topological sort.
fn order_by_dependencies(
    all_items: &[ReleaseItem],
    batch: &[ReleaseBatchItem],
    _known_identifiers: &[String],
) -> Vec<ReleaseBatchItem> {
    let batch_ids: Vec<String> = batch.iter().map(|i| i.identifier.clone()).collect();
    let item_lookup: BTreeMap<&str, &ReleaseItem> = all_items
        .iter()
        .map(|i| (i.identifier.as_str(), i))
        .collect();

    // Build in-degree map for items within this batch.
    let mut in_degree: BTreeMap<&str, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for id in &batch_ids {
        in_degree.entry(id.as_str()).or_insert(0);
    }
    for id in &batch_ids {
        if let Some(item) = item_lookup.get(id.as_str()) {
            for blocker in &item.blocked_by {
                if batch_ids.contains(blocker) {
                    *in_degree.entry(id.as_str()).or_insert(0) += 1;
                    dependents
                        .entry(blocker.as_str())
                        .or_default()
                        .push(id.as_str());
                }
            }
        }
    }

    // Kahn's algorithm.
    let mut queue: Vec<&str> = in_degree
        .iter()
        .filter(|&(_, deg)| *deg == 0)
        .map(|(&id, _)| id)
        .collect();
    queue.sort();

    let mut ordered_ids: Vec<String> = Vec::new();
    while let Some(current) = queue.first().cloned() {
        queue.remove(0);
        ordered_ids.push(current.to_string());
        if let Some(deps) = dependents.get(current) {
            for dep in deps {
                if let Some(deg) = in_degree.get_mut(dep) {
                    *deg = deg.saturating_sub(1);
                    if *deg == 0 {
                        queue.push(dep);
                        queue.sort();
                    }
                }
            }
        }
    }

    // Append any remaining items not reached (cycle case).
    for id in &batch_ids {
        if !ordered_ids.contains(id) {
            ordered_ids.push(id.clone());
        }
    }

    let batch_map: BTreeMap<&str, &ReleaseBatchItem> =
        batch.iter().map(|i| (i.identifier.as_str(), i)).collect();
    ordered_ids
        .iter()
        .filter_map(|id| batch_map.get(id.as_str()).cloned().cloned())
        .collect()
}

fn write_release_plan(root: &Path, plan: &ReleasePlan) -> Result<PathBuf> {
    let paths = PlanningPaths::new(root);
    let plan_dir = paths.releases_dir.join(&plan.name);

    // Write plan.json.
    let json = serde_json::to_string_pretty(plan).context("failed to encode release plan")?;
    write_text_file(&plan_dir.join("plan.json"), &json, true)?;

    // Write plan.md (human-readable).
    let markdown = render_plan_markdown(plan);
    write_text_file(&plan_dir.join("plan.md"), &markdown, true)?;

    Ok(plan_dir)
}

fn render_plan_markdown(plan: &ReleasePlan) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Release Plan: {}\n\n", plan.name));
    out.push_str(&format!(
        "- **Total backlog items:** {}\n",
        plan.total_items
    ));
    out.push_str(&format!(
        "- **Included:** {}\n",
        plan.cut_line.included_count
    ));
    out.push_str(&format!(
        "- **Deferred:** {}\n",
        plan.cut_line.deferred_count
    ));
    out.push_str(&format!(
        "- **Linear metadata:** {}\n\n",
        if plan.fetched_linear_metadata {
            "fetched"
        } else {
            "local only"
        }
    ));

    for batch in &plan.batches {
        out.push_str(&format!("## {}\n\n", batch.name));
        out.push_str(&format!("{}\n\n", batch.rationale));
        for item in &batch.issues {
            let priority_str = item.priority_label.as_deref().unwrap_or("none");
            let state_str = item.state.as_deref().unwrap_or("unknown");
            out.push_str(&format!(
                "- [ ] **{}** — {} (priority: {}, state: {})\n",
                item.identifier, item.title, priority_str, state_str
            ));
        }
        out.push('\n');
    }

    out.push_str("## Cut Line\n\n");
    out.push_str(&format!("{}\n\n", plan.cut_line.rationale));

    out.push_str("## Recommended Ordering\n\n");
    for (i, entry) in plan.ordering.iter().enumerate() {
        out.push_str(&format!("{}. {entry}\n", i + 1));
    }
    out.push('\n');

    if !plan.risks.is_empty() {
        out.push_str("## Risks\n\n");
        for risk in &plan.risks {
            out.push_str(&format!("- {risk}\n"));
        }
        out.push('\n');
    }

    out
}

fn render_plan(plan: &ReleasePlan, plan_dir: &Path, root: &Path) -> String {
    let mut out = String::new();
    out.push_str(&format!("Release plan: {}\n\n", plan.name));
    out.push_str(&format!(
        "  Total items: {}  |  Included: {}  |  Deferred: {}\n\n",
        plan.total_items, plan.cut_line.included_count, plan.cut_line.deferred_count
    ));

    for batch in &plan.batches {
        out.push_str(&format!("{}:\n", batch.name));
        for item in &batch.issues {
            let priority_str = item.priority_label.as_deref().unwrap_or("none");
            out.push_str(&format!(
                "  {} — {} [{}]\n",
                item.identifier, item.title, priority_str
            ));
        }
        out.push('\n');
    }

    out.push_str(&format!("Cut line: {}\n\n", plan.cut_line.rationale));

    if !plan.ordering.is_empty() {
        out.push_str("Recommended ordering:\n");
        for (i, entry) in plan.ordering.iter().enumerate() {
            out.push_str(&format!("  {}. {entry}\n", i + 1));
        }
        out.push('\n');
    }

    if !plan.risks.is_empty() {
        out.push_str("Risks:\n");
        for risk in &plan.risks {
            out.push_str(&format!("  - {risk}\n"));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "Plan written to: {}\n",
        display_path(plan_dir, root)
    ));
    out
}

fn render_apply_preview(plan: &ReleasePlan) -> String {
    let mut out = String::new();
    out.push_str(&format!("Release plan: {}\n\n", plan.name));
    for batch in &plan.batches {
        out.push_str(&format!(
            "  {} ({} issue(s)):\n",
            batch.name,
            batch.issues.len()
        ));
        for item in &batch.issues {
            out.push_str(&format!("    {} — {}\n", item.identifier, item.title));
        }
    }
    out.push_str(&format!(
        "\n  Cut line: {} included, {} deferred\n",
        plan.cut_line.included_count, plan.cut_line.deferred_count
    ));
    out
}

fn confirm_apply(preview: &str) -> Result<bool> {
    println!("{preview}");
    print!("Apply this release plan? [y/N] ");
    io::stdout().flush().context("failed to flush stdout")?;
    let mut input = String::new();
    io::stdin()
        .lock()
        .read_line(&mut input)
        .context("failed to read confirmation")?;
    Ok(input.trim().eq_ignore_ascii_case("y"))
}

fn priority_label(priority: u8) -> String {
    match priority {
        0 => "none".to_string(),
        1 => "urgent".to_string(),
        2 => "high".to_string(),
        3 => "medium".to_string(),
        4 => "low".to_string(),
        other => format!("p{other}"),
    }
}

/// Extract issue identifiers that appear in "blocked by" or "depends on" context.
fn extract_blocked_by(text: &str, own_identifier: &str) -> Vec<String> {
    let mut blockers = Vec::new();
    for line in text.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("blocked by")
            || lower.contains("blockedby")
            || lower.contains("depends on")
            || lower.contains("dependency")
        {
            for identifier in extract_issue_identifiers(line) {
                if !identifier.eq_ignore_ascii_case(own_identifier)
                    && !blockers.contains(&identifier)
                {
                    blockers.push(identifier);
                }
            }
        }
    }
    blockers
}

/// Extract issue identifiers (e.g. MET-6, ENG-123) from a line of text.
fn extract_issue_identifiers(line: &str) -> Vec<String> {
    let mut identifiers = Vec::new();
    let mut chars = line.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch.is_ascii_uppercase() {
            let prefix_start = i;
            let mut prefix_end = i + ch.len_utf8();
            while let Some(&(_, next_ch)) = chars.peek() {
                if next_ch.is_ascii_uppercase() || next_ch.is_ascii_digit() {
                    prefix_end += next_ch.len_utf8();
                    chars.next();
                } else {
                    break;
                }
            }
            if let Some(&(_, '-')) = chars.peek() {
                chars.next();
                let mut number_end = prefix_end + 1;
                let mut has_digits = false;
                while let Some(&(_, digit_ch)) = chars.peek() {
                    if digit_ch.is_ascii_digit() {
                        has_digits = true;
                        number_end += digit_ch.len_utf8();
                        chars.next();
                    } else {
                        break;
                    }
                }
                if has_digits && prefix_end - prefix_start >= 2 {
                    identifiers.push(line[prefix_start..number_end].to_string());
                }
            }
        }
    }
    identifiers
}

fn load_issue_metadata_if_present(issue_dir: &Path) -> Result<Option<BacklogIssueMetadata>> {
    let path = issue_dir.join(".linear.json");
    if !path.is_file() {
        return Ok(None);
    }
    load_issue_metadata(issue_dir).map(Some)
}

fn first_heading(markdown: &str) -> Option<String> {
    markdown
        .lines()
        .find_map(|line| line.strip_prefix("# ").map(str::trim))
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn read_optional_text_file(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

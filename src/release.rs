use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

use crate::agents::run_agent_capture;
use crate::backlog::{BacklogIssueMetadata, load_issue_metadata};
use crate::cli::{ReleaseArgs, RunAgentArgs};
use crate::config::{AGENT_ROUTE_BACKLOG_RELEASE, load_required_planning_meta};
use crate::context::load_workflow_contract;
use crate::fs::{PlanningPaths, canonicalize_existing_dir, write_text_file};
use crate::scaffold::ensure_planning_layout;

/// Summary of a local backlog item discovered in `.metastack/backlog/`.
#[derive(Debug, Clone, Serialize)]
struct LocalBacklogSummary {
    identifier: String,
    title: String,
    url: String,
    index_contents: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    priority: Option<u8>,
}

/// Agent-produced release plan with milestone batches.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ReleasePlan {
    name: String,
    summary: String,
    #[serde(default)]
    batches: Vec<ReleaseBatch>,
    #[serde(default)]
    cut_line_rationale: String,
    #[serde(default)]
    risks: Vec<String>,
}

/// A single milestone-ready batch within a release plan.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ReleaseBatch {
    label: String,
    #[serde(default)]
    rationale: String,
    #[serde(default)]
    issues: Vec<ReleaseBatchIssue>,
    #[serde(default)]
    is_cut: bool,
}

/// An issue assigned to a release batch.
#[derive(Debug, Clone, Deserialize, Serialize)]
struct ReleaseBatchIssue {
    identifier: String,
    title: String,
    #[serde(default)]
    sequence: u32,
    #[serde(default)]
    must_have: bool,
}

/// Run the `meta backlog release` command.
///
/// Enumerates local backlog items, calls the agent to produce a milestone-ready
/// release plan, and writes the plan to `.metastack/releases/<plan-name>/`.
pub fn run_release(args: &ReleaseArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let _planning_meta = load_required_planning_meta(&root, "backlog release")?;
    ensure_planning_layout(&root, false)?;

    let items = enumerate_local_backlog_items(&root)?;
    if items.is_empty() {
        println!(
            "No backlog items found under `.metastack/backlog/`. \
             Create backlog items with `meta backlog plan` or `meta backlog tech` first."
        );
        return Ok(());
    }

    if items.len() < 2 {
        println!(
            "Only {} backlog item found. A release plan requires at least 2 items to \
             produce meaningful batches. Add more items with `meta backlog plan`.",
            items.len()
        );
        return Ok(());
    }

    let plan_name = args.name.clone().unwrap_or_else(generate_plan_name_slug);

    let plan = generate_release_plan(&root, &items, &plan_name, args)?;
    let plan_dir = write_release_plan(&root, &plan_name, &plan)?;

    println!(
        "Release plan \"{}\" written to {}",
        plan_name,
        plan_dir.strip_prefix(&root).unwrap_or(&plan_dir).display()
    );
    println!();
    print_plan_summary(&plan);

    if args.apply {
        println!();
        println!(
            "Apply mode is not yet implemented. The release plan has been saved locally. \
             Linear metadata updates will be available in a future version."
        );
    }

    Ok(())
}

/// Enumerate all local backlog items that have a `.linear.json` metadata file.
fn enumerate_local_backlog_items(root: &Path) -> Result<Vec<LocalBacklogSummary>> {
    let paths = PlanningPaths::new(root);
    let backlog_dir = &paths.backlog_dir;

    if !backlog_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();

    let mut entries: Vec<_> = fs::read_dir(backlog_dir)
        .with_context(|| format!("failed to read `{}`", backlog_dir.display()))?
        .filter_map(|entry| entry.ok())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let entry_path = entry.path();
        if !entry_path.is_dir() {
            continue;
        }

        let dir_name = match entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => continue,
        };

        // Skip template directory
        if dir_name.starts_with('_') {
            continue;
        }

        let metadata_path = entry_path.join(".linear.json");
        if !metadata_path.is_file() {
            continue;
        }

        let metadata: BacklogIssueMetadata = load_issue_metadata(&entry_path)
            .with_context(|| format!("failed to load metadata for `{dir_name}`"))?;

        let index_path = entry_path.join("index.md");
        let index_contents = if index_path.is_file() {
            fs::read_to_string(&index_path)
                .with_context(|| format!("failed to read `{}`", index_path.display()))?
        } else {
            String::new()
        };

        items.push(LocalBacklogSummary {
            identifier: metadata.identifier,
            title: metadata.title,
            url: metadata.url,
            index_contents,
            priority: None,
        });
    }

    Ok(items)
}

/// Call the agent to produce a structured release plan from the given backlog items.
fn generate_release_plan(
    root: &Path,
    items: &[LocalBacklogSummary],
    plan_name: &str,
    args: &ReleaseArgs,
) -> Result<ReleasePlan> {
    let prompt = build_release_prompt(root, items, plan_name)?;

    let output = run_agent_capture(&RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_BACKLOG_RELEASE.to_string()),
        agent: args.agent.clone(),
        prompt,
        instructions: None,
        model: args.model.clone(),
        reasoning: args.reasoning.clone(),
        transport: None,
    })?;

    parse_release_plan(&output.stdout)
}

/// Build the agent prompt that includes all backlog item context.
fn build_release_prompt(
    root: &Path,
    items: &[LocalBacklogSummary],
    plan_name: &str,
) -> Result<String> {
    let workflow_contract = load_workflow_contract(root)?;

    let items_block = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let truncated = truncate_content(&item.index_contents, 2000);
            format!(
                "### Item {} — `{}`\n\
                 - Title: {}\n\
                 - URL: {}\n\
                 - Content:\n```md\n{}\n```",
                index + 1,
                item.identifier,
                item.title,
                item.url,
                truncated,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    Ok(format!(
        "You are producing a release plan for the active repository's backlog.\n\n\
         Injected workflow contract:\n{workflow_contract}\n\n\
         Release plan name: `{plan_name}`\n\n\
         Total backlog items: {count}\n\n\
         ## Backlog Items\n\n\
         {items_block}\n\n\
         ## Instructions\n\n\
         1. Analyze all backlog items above.\n\
         2. Group them into ordered execution batches that form a coherent milestone plan.\n\
         3. Use priority and dependency signals to separate must-have work from deferrable work.\n\
         4. For each batch, provide a label, rationale, and ordered list of issues with sequencing.\n\
         5. Identify a recommended cut line — the point where deferrable work begins.\n\
         6. List risks associated with the proposed release plan.\n\
         7. Return JSON only with this exact shape:\n\
         ```json\n\
         {{\n\
           \"name\": \"{plan_name}\",\n\
           \"summary\": \"One-paragraph summary of the release plan.\",\n\
           \"batches\": [\n\
             {{\n\
               \"label\": \"Batch 1: Core infrastructure\",\n\
               \"rationale\": \"Why these items are grouped together.\",\n\
               \"issues\": [\n\
                 {{\n\
                   \"identifier\": \"MET-1\",\n\
                   \"title\": \"Issue title\",\n\
                   \"sequence\": 1,\n\
                   \"must_have\": true\n\
                 }}\n\
               ],\n\
               \"is_cut\": false\n\
             }},\n\
             {{\n\
               \"label\": \"Batch 2: Nice-to-have improvements\",\n\
               \"rationale\": \"These are deferrable.\",\n\
               \"issues\": [...],\n\
               \"is_cut\": true\n\
             }}\n\
           ],\n\
           \"cut_line_rationale\": \"Why the cut line is placed here.\",\n\
           \"risks\": [\"Risk 1\", \"Risk 2\"]\n\
         }}\n\
         ```",
        count = items.len(),
    ))
}

/// Parse the agent response into a structured release plan.
fn parse_release_plan(raw: &str) -> Result<ReleasePlan> {
    let trimmed = raw.trim();
    let mut candidates = vec![trimmed.to_string()];

    if let Some(stripped) = strip_code_fence(trimmed) {
        candidates.push(stripped);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && start <= end
    {
        candidates.push(trimmed[start..=end].to_string());
    }

    for candidate in candidates {
        if let Ok(parsed) = serde_json::from_str::<ReleasePlan>(&candidate) {
            return Ok(parsed);
        }
    }

    bail!(
        "release planning agent returned invalid JSON: {}",
        preview_text(trimmed)
    )
}

/// Write the release plan to `.metastack/releases/<plan-name>/`.
fn write_release_plan(
    root: &Path,
    plan_name: &str,
    plan: &ReleasePlan,
) -> Result<std::path::PathBuf> {
    let paths = PlanningPaths::new(root);
    let plan_dir = paths.release_plan_dir(plan_name);

    let plan_md = render_plan_markdown(plan);
    write_text_file(&plan_dir.join("plan.md"), &plan_md, true)?;

    let metadata_json =
        serde_json::to_string_pretty(plan).context("failed to serialize release plan metadata")?;
    write_text_file(&plan_dir.join("metadata.json"), &metadata_json, true)?;

    Ok(plan_dir)
}

/// Render the release plan as human-readable Markdown.
fn render_plan_markdown(plan: &ReleasePlan) -> String {
    let mut lines = Vec::new();

    lines.push(format!("# Release Plan: {}", plan.name));
    lines.push(String::new());
    lines.push(plan.summary.clone());
    lines.push(String::new());

    for (batch_index, batch) in plan.batches.iter().enumerate() {
        let cut_marker = if batch.is_cut {
            " (below cut line)"
        } else {
            ""
        };
        lines.push(format!(
            "## Batch {}: {}{}",
            batch_index + 1,
            batch.label,
            cut_marker
        ));
        lines.push(String::new());
        if !batch.rationale.is_empty() {
            lines.push(format!("> {}", batch.rationale));
            lines.push(String::new());
        }

        lines.push("| Seq | Identifier | Title | Must-have |".to_string());
        lines.push("| --- | ---------- | ----- | --------- |".to_string());
        for issue in &batch.issues {
            let must_have = if issue.must_have { "Yes" } else { "No" };
            lines.push(format!(
                "| {} | `{}` | {} | {} |",
                issue.sequence, issue.identifier, issue.title, must_have
            ));
        }
        lines.push(String::new());
    }

    if !plan.cut_line_rationale.is_empty() {
        lines.push("## Cut Line Rationale".to_string());
        lines.push(String::new());
        lines.push(plan.cut_line_rationale.clone());
        lines.push(String::new());
    }

    if !plan.risks.is_empty() {
        lines.push("## Risks".to_string());
        lines.push(String::new());
        for risk in &plan.risks {
            lines.push(format!("- {risk}"));
        }
        lines.push(String::new());
    }

    lines.join("\n")
}

/// Print a concise summary of the release plan to stdout.
fn print_plan_summary(plan: &ReleasePlan) {
    println!("{}", plan.summary);
    println!();

    let total_issues: usize = plan.batches.iter().map(|b| b.issues.len()).sum();
    let must_have_count: usize = plan
        .batches
        .iter()
        .flat_map(|b| &b.issues)
        .filter(|i| i.must_have)
        .count();
    let above_cut: usize = plan
        .batches
        .iter()
        .filter(|b| !b.is_cut)
        .map(|b| b.issues.len())
        .sum();

    println!(
        "Batches: {}  |  Issues: {} ({} must-have)  |  Above cut: {}",
        plan.batches.len(),
        total_issues,
        must_have_count,
        above_cut,
    );

    for (index, batch) in plan.batches.iter().enumerate() {
        let cut_tag = if batch.is_cut { " [deferred]" } else { "" };
        println!(
            "  {}. {} ({} issues){}",
            index + 1,
            batch.label,
            batch.issues.len(),
            cut_tag,
        );
    }

    if !plan.risks.is_empty() {
        println!();
        println!("Risks:");
        for risk in &plan.risks {
            println!("  - {risk}");
        }
    }
}

/// Generate a timestamped slug for the plan name.
fn generate_plan_name_slug() -> String {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let now = OffsetDateTime::now_utc().to_offset(offset);
    now.format(&format_description!(
        "release-[year][month][day]-[hour][minute][second]"
    ))
    .unwrap_or_else(|_| "release-plan".to_string())
}

/// Truncate content to a maximum number of characters with an ellipsis marker.
fn truncate_content(content: &str, max_chars: usize) -> &str {
    if content.len() <= max_chars {
        content
    } else {
        let boundary = content
            .char_indices()
            .nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(content.len());
        &content[..boundary]
    }
}

fn strip_code_fence(raw: &str) -> Option<String> {
    let stripped = raw.strip_prefix("```")?;
    let stripped = stripped
        .strip_prefix("json")
        .or_else(|| stripped.strip_prefix("JSON"))
        .unwrap_or(stripped);
    let stripped = stripped.trim_start_matches('\n');
    stripped
        .strip_suffix("```")
        .map(|s| s.trim_end().to_string())
}

fn preview_text(text: &str) -> String {
    if text.len() <= 200 {
        text.to_string()
    } else {
        format!("{}...", &text[..200])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_release_plan_from_fenced_json() {
        let raw = r#"```json
{
  "name": "v0.3",
  "summary": "Core infrastructure release.",
  "batches": [
    {
      "label": "Core",
      "rationale": "Foundation work.",
      "issues": [
        {
          "identifier": "MET-1",
          "title": "Setup CLI",
          "sequence": 1,
          "must_have": true
        }
      ],
      "is_cut": false
    },
    {
      "label": "Polish",
      "rationale": "Nice-to-have.",
      "issues": [
        {
          "identifier": "MET-2",
          "title": "Add docs",
          "sequence": 1,
          "must_have": false
        }
      ],
      "is_cut": true
    }
  ],
  "cut_line_rationale": "Core items are required for launch.",
  "risks": ["Timeline pressure"]
}
```"#;

        let plan = parse_release_plan(raw).expect("should parse fenced JSON");
        assert_eq!(plan.name, "v0.3");
        assert_eq!(plan.batches.len(), 2);
        assert!(plan.batches[0].issues[0].must_have);
        assert!(plan.batches[1].is_cut);
        assert_eq!(plan.risks, vec!["Timeline pressure"]);
    }

    #[test]
    fn parse_release_plan_from_bare_json() {
        let raw =
            r#"{"name":"q1","summary":"Quick.","batches":[],"cut_line_rationale":"","risks":[]}"#;
        let plan = parse_release_plan(raw).expect("should parse bare JSON");
        assert_eq!(plan.name, "q1");
        assert!(plan.batches.is_empty());
    }

    #[test]
    fn empty_backlog_detected() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let backlog_dir = root.join(".metastack").join("backlog");
        fs::create_dir_all(&backlog_dir)?;

        let items = enumerate_local_backlog_items(root)?;
        assert!(items.is_empty());
        Ok(())
    }

    #[test]
    fn enumerate_skips_template_dirs() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let backlog_dir = root.join(".metastack").join("backlog");
        let template_dir = backlog_dir.join("_TEMPLATE");
        fs::create_dir_all(&template_dir)?;
        fs::write(
            template_dir.join(".linear.json"),
            r#"{"issue_id":"","identifier":"_TEMPLATE","title":"","url":"","team_key":""}"#,
        )?;

        let items = enumerate_local_backlog_items(root)?;
        assert!(items.is_empty());
        Ok(())
    }

    #[test]
    fn enumerate_reads_valid_backlog_items() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let issue_dir = root.join(".metastack").join("backlog").join("MET-10");
        fs::create_dir_all(&issue_dir)?;
        fs::write(
            issue_dir.join(".linear.json"),
            r#"{"issue_id":"id-10","identifier":"MET-10","title":"Test issue","url":"https://example.com/MET-10","team_key":"MET"}"#,
        )?;
        fs::write(issue_dir.join("index.md"), "# Test issue\nSome content.")?;

        let items = enumerate_local_backlog_items(root)?;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].identifier, "MET-10");
        assert_eq!(items[0].title, "Test issue");
        assert!(items[0].index_contents.contains("Some content"));
        Ok(())
    }

    #[test]
    fn render_plan_markdown_includes_all_sections() {
        let plan = ReleasePlan {
            name: "v1.0".to_string(),
            summary: "First milestone.".to_string(),
            batches: vec![
                ReleaseBatch {
                    label: "Core".to_string(),
                    rationale: "Must ship.".to_string(),
                    issues: vec![ReleaseBatchIssue {
                        identifier: "MET-1".to_string(),
                        title: "Init".to_string(),
                        sequence: 1,
                        must_have: true,
                    }],
                    is_cut: false,
                },
                ReleaseBatch {
                    label: "Stretch".to_string(),
                    rationale: "If time allows.".to_string(),
                    issues: vec![ReleaseBatchIssue {
                        identifier: "MET-2".to_string(),
                        title: "Polish".to_string(),
                        sequence: 1,
                        must_have: false,
                    }],
                    is_cut: true,
                },
            ],
            cut_line_rationale: "Core items are required.".to_string(),
            risks: vec!["Tight schedule".to_string()],
        };

        let md = render_plan_markdown(&plan);
        assert!(md.contains("# Release Plan: v1.0"));
        assert!(md.contains("First milestone."));
        assert!(md.contains("## Batch 1: Core"));
        assert!(md.contains("## Batch 2: Stretch (below cut line)"));
        assert!(md.contains("## Cut Line Rationale"));
        assert!(md.contains("## Risks"));
        assert!(md.contains("Tight schedule"));
        assert!(md.contains("| `MET-1` |"));
    }

    #[test]
    fn generate_plan_name_slug_produces_valid_name() {
        let name = generate_plan_name_slug();
        assert!(name.starts_with("release-"));
        assert!(name.len() > 10);
    }

    #[test]
    fn truncate_content_short_string_unchanged() {
        assert_eq!(truncate_content("hello", 100), "hello");
    }

    #[test]
    fn truncate_content_long_string_cut() {
        let long = "a".repeat(300);
        let truncated = truncate_content(&long, 100);
        assert_eq!(truncated.len(), 100);
    }
}

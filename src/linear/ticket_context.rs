use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::backlog::write_issue_attachment_file;

use super::{IssueComment, IssueSummary, LinearClient, LinearService};

pub(crate) const DEFAULT_DISCUSSION_PROMPT_CHARS: usize = 6_000;
pub(crate) const DEFAULT_DISCUSSION_PERSISTED_CHARS: usize = 20_000;
const LOCALIZED_CONTEXT_METADATA_FILE: &str = ".ticket-context.json";
const DISCUSSION_CONTEXT_PATH: &str = "context/ticket-discussion.md";
const IMAGE_MANIFEST_PATH: &str = "artifacts/ticket-images.md";

#[derive(Debug, Clone, Copy)]
pub(crate) struct TicketDiscussionBudgets {
    pub(crate) prompt_chars: usize,
    pub(crate) persisted_chars: usize,
}

impl Default for TicketDiscussionBudgets {
    fn default() -> Self {
        Self {
            prompt_chars: DEFAULT_DISCUSSION_PROMPT_CHARS,
            persisted_chars: DEFAULT_DISCUSSION_PERSISTED_CHARS,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedIssueContext {
    pub(crate) issue: IssueSummary,
    pub(crate) prompt_discussion: String,
    pub(crate) persisted_discussion: String,
    pub(crate) images: Vec<PreparedTicketImage>,
}

impl PreparedIssueContext {
    pub(crate) fn image_manifest_markdown(&self) -> String {
        let mut lines = vec![
            "# Ticket Images".to_string(),
            String::new(),
            "| File | Alt Text | Source | Original URL |".to_string(),
            "| --- | --- | --- | --- |".to_string(),
        ];

        for image in &self.images {
            lines.push(format!(
                "| `{}` | {} | {} | <{}> |",
                image.filename,
                escape_markdown_cell(&image.alt_text),
                escape_markdown_cell(&image.source_label),
                image.original_url
            ));
        }

        lines.join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedTicketImage {
    pub(crate) filename: String,
    pub(crate) alt_text: String,
    pub(crate) source_label: String,
    pub(crate) original_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TicketImageDownloadFailure {
    pub(crate) filename: String,
    pub(crate) original_url: String,
    pub(crate) source_label: String,
    pub(crate) error: String,
}

#[derive(Debug, Clone, Copy)]
enum ImageSource {
    Description,
    Parent,
    Comment(usize),
}

impl ImageSource {
    fn display_label(self) -> String {
        match self {
            Self::Description => "Issue description".to_string(),
            Self::Parent => "Parent description".to_string(),
            Self::Comment(index) => format!("comment-{index}"),
        }
    }

    fn filename_prefix(self) -> Option<String> {
        match self {
            Self::Description => None,
            Self::Parent => Some("parent".to_string()),
            Self::Comment(index) => Some(format!("comment-{index}")),
        }
    }
}

#[derive(Debug, Clone)]
struct MarkdownImage {
    span_start: usize,
    span_end: usize,
    alt_text: String,
    original_url: String,
}

#[derive(Debug, Clone)]
struct DiscussionSection {
    sort_key: Option<String>,
    original_index: usize,
    rendered: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalizedTicketContextMetadata {
    ignored_paths: Vec<String>,
}

/// Rewrite ticket description, parent description, and comment image references to local
/// `artifacts/` paths and build prompt/persisted discussion-context strings.
pub(crate) fn prepare_issue_context(
    issue: &IssueSummary,
    budgets: TicketDiscussionBudgets,
) -> PreparedIssueContext {
    let mut issue = issue.clone();
    let mut images = Vec::new();
    let mut used_filenames = BTreeSet::new();
    let mut image_sequence = 1usize;

    if let Some(description) = issue.description.clone() {
        issue.description = Some(rewrite_markdown_images(
            &description,
            ImageSource::Description,
            &mut image_sequence,
            &mut used_filenames,
            &mut images,
            None,
        ));
    }

    if let Some(parent) = &mut issue.parent
        && let Some(description) = parent.description.clone()
    {
        parent.description = Some(rewrite_markdown_images(
            &description,
            ImageSource::Parent,
            &mut image_sequence,
            &mut used_filenames,
            &mut images,
            None,
        ));
    }

    issue.comments = issue
        .comments
        .iter()
        .enumerate()
        .map(|(index, comment)| {
            let source_index = index + 1;
            let source_label = comment_source_label(&comment.body, source_index);
            let mut rewritten = comment.clone();
            rewritten.body = rewrite_markdown_images(
                &comment.body,
                ImageSource::Comment(source_index),
                &mut image_sequence,
                &mut used_filenames,
                &mut images,
                Some(source_label),
            );
            rewritten
        })
        .collect();

    let prompt_discussion = build_discussion_context(&issue.comments, budgets.prompt_chars);
    let persisted_discussion = build_discussion_context(&issue.comments, budgets.persisted_chars);

    PreparedIssueContext {
        issue,
        prompt_discussion,
        persisted_discussion,
        images,
    }
}

/// Download localized ticket images, write the ticket-image manifest, and persist the ticket
/// discussion context under the backlog item directory. Returns non-fatal image download failures.
///
/// Errors are returned when local filesystem writes or cleanup fail.
pub(crate) async fn materialize_issue_context<C>(
    service: &LinearService<C>,
    issue_dir: &Path,
    context: &PreparedIssueContext,
) -> Result<Vec<TicketImageDownloadFailure>>
where
    C: LinearClient,
{
    let previous_images = read_manifest_image_paths(&issue_dir.join(IMAGE_MANIFEST_PATH))?;
    let current_images = context
        .images
        .iter()
        .map(|image| format!("artifacts/{}", image.filename))
        .collect::<BTreeSet<_>>();
    let ignored_paths = std::iter::once(DISCUSSION_CONTEXT_PATH.to_string())
        .chain(std::iter::once(IMAGE_MANIFEST_PATH.to_string()))
        .chain(current_images.iter().cloned())
        .collect::<Vec<_>>();

    write_issue_attachment_file(
        issue_dir,
        IMAGE_MANIFEST_PATH,
        context.image_manifest_markdown().as_bytes(),
    )?;
    write_issue_attachment_file(
        issue_dir,
        DISCUSSION_CONTEXT_PATH,
        context.persisted_discussion.as_bytes(),
    )?;
    let metadata = serde_json::to_string_pretty(&LocalizedTicketContextMetadata { ignored_paths })
        .context("failed to encode localized ticket context metadata")?;
    write_issue_attachment_file(
        issue_dir,
        LOCALIZED_CONTEXT_METADATA_FILE,
        metadata.as_bytes(),
    )?;

    let mut failures = Vec::new();
    for image in &context.images {
        match service.download_file(&image.original_url).await {
            Ok(contents) => {
                write_issue_attachment_file(
                    issue_dir,
                    &format!("artifacts/{}", image.filename),
                    &contents,
                )?;
            }
            Err(error) => failures.push(TicketImageDownloadFailure {
                filename: image.filename.clone(),
                original_url: image.original_url.clone(),
                source_label: image.source_label.clone(),
                error: error.to_string(),
            }),
        }
    }

    for stale_path in previous_images {
        if current_images.contains(&stale_path) {
            continue;
        }
        let absolute_path = issue_dir.join(&stale_path);
        if absolute_path.exists() {
            fs::remove_file(&absolute_path).with_context(|| {
                format!(
                    "failed to remove stale localized ticket image `{}`",
                    absolute_path.display()
                )
            })?;
        }
    }

    Ok(failures)
}

/// Load generated localized ticket-context paths that should not participate in backlog sync
/// hashing or managed attachment uploads.
///
/// Returns an empty set when the localized ticket-context metadata file is absent.
pub(crate) fn load_localized_ticket_context_ignored_paths(
    issue_dir: &Path,
) -> Result<BTreeSet<String>> {
    let metadata_path = issue_dir.join(LOCALIZED_CONTEXT_METADATA_FILE);
    if !metadata_path.is_file() {
        return Ok(BTreeSet::new());
    }

    let contents = fs::read_to_string(&metadata_path)
        .with_context(|| format!("failed to read `{}`", metadata_path.display()))?;
    let metadata: LocalizedTicketContextMetadata = serde_json::from_str(&contents)
        .with_context(|| format!("failed to decode `{}`", metadata_path.display()))?;
    Ok(metadata.ignored_paths.into_iter().collect())
}

/// Render a concise image summary for agent-facing prompt context.
pub(crate) fn render_ticket_image_summary(images: &[PreparedTicketImage]) -> String {
    if images.is_empty() {
        return "_No markdown image references were found in the issue, parent, or comments._"
            .to_string();
    }

    images
        .iter()
        .map(|image| {
            format!(
                "- `{}` from {} (`{}`) -> {}",
                image.filename, image.source_label, image.alt_text, image.original_url
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn rewrite_markdown_images(
    markdown: &str,
    source: ImageSource,
    image_sequence: &mut usize,
    used_filenames: &mut BTreeSet<String>,
    images: &mut Vec<PreparedTicketImage>,
    source_label_override: Option<String>,
) -> String {
    let matches = parse_markdown_images(markdown);
    if matches.is_empty() {
        return markdown.to_string();
    }

    let mut rendered = String::with_capacity(markdown.len());
    let mut previous_end = 0usize;
    for image in matches {
        rendered.push_str(&markdown[previous_end..image.span_start]);
        let filename =
            generate_image_filename(&image.original_url, source, *image_sequence, used_filenames);
        *image_sequence += 1;
        images.push(PreparedTicketImage {
            filename: filename.clone(),
            alt_text: image.alt_text.clone(),
            source_label: source_label_override
                .clone()
                .unwrap_or_else(|| source.display_label()),
            original_url: image.original_url.clone(),
        });
        rendered.push_str(&format!("![{}](artifacts/{filename})", image.alt_text));
        previous_end = image.span_end;
    }
    rendered.push_str(&markdown[previous_end..]);

    rendered
}

fn parse_markdown_images(markdown: &str) -> Vec<MarkdownImage> {
    let bytes = markdown.as_bytes();
    let mut images = Vec::new();
    let mut index = 0usize;

    while index + 3 < bytes.len() {
        if bytes[index] != b'!' || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let Some(alt_end) = markdown[index + 2..].find(']') else {
            break;
        };
        let alt_end = index + 2 + alt_end;
        if alt_end + 1 >= bytes.len() || bytes[alt_end + 1] != b'(' {
            index += 1;
            continue;
        }
        let Some(url_end_offset) = markdown[alt_end + 2..].find(')') else {
            break;
        };
        let url_end = alt_end + 2 + url_end_offset;
        let alt_text = markdown[index + 2..alt_end].to_string();
        let original_url = markdown[alt_end + 2..url_end].trim().to_string();
        images.push(MarkdownImage {
            span_start: index,
            span_end: url_end + 1,
            alt_text,
            original_url,
        });
        index = url_end + 1;
    }

    images
}

fn generate_image_filename(
    original_url: &str,
    source: ImageSource,
    image_sequence: usize,
    used_filenames: &mut BTreeSet<String>,
) -> String {
    let basename = sanitized_basename(original_url);
    let fallback_extension = basename
        .as_deref()
        .and_then(file_extension)
        .unwrap_or_else(|| extension_from_url(original_url).unwrap_or_else(|| "bin".to_string()));

    let candidate = match (source.filename_prefix(), basename) {
        (None, Some(basename)) => basename,
        (Some(prefix), Some(basename)) => format!("{prefix}-{basename}"),
        (_, None) => format!("image-{image_sequence}.{fallback_extension}"),
    };

    uniquify_filename(&candidate, used_filenames)
}

fn sanitized_basename(original_url: &str) -> Option<String> {
    let raw = Url::parse(original_url)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
                .map(str::to_string)
        })
        .or_else(|| {
            original_url
                .split('/')
                .next_back()
                .map(str::trim)
                .filter(|segment| !segment.is_empty())
                .map(str::to_string)
        })?;

    sanitize_filename(&raw)
}

fn sanitize_filename(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut sanitized = String::with_capacity(trimmed.len());
    for character in trimmed.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
            sanitized.push(character);
        } else {
            sanitized.push('-');
        }
    }

    while sanitized.contains("--") {
        sanitized = sanitized.replace("--", "-");
    }

    let sanitized = sanitized.trim_matches(['-', '.']).to_string();
    if sanitized.is_empty()
        || !sanitized
            .chars()
            .any(|character| character.is_ascii_alphanumeric())
        || file_stem(&sanitized).is_none()
    {
        None
    } else {
        Some(sanitized)
    }
}

fn uniquify_filename(candidate: &str, used_filenames: &mut BTreeSet<String>) -> String {
    if used_filenames.insert(candidate.to_string()) {
        return candidate.to_string();
    }

    let stem = file_stem(candidate).unwrap_or(candidate);
    let extension = file_extension(candidate);
    let mut suffix = 2usize;
    loop {
        let deduped = match extension {
            Some(ref extension) => format!("{stem}-{suffix}.{extension}"),
            None => format!("{stem}-{suffix}"),
        };
        if used_filenames.insert(deduped.clone()) {
            return deduped;
        }
        suffix += 1;
    }
}

fn file_stem(filename: &str) -> Option<&str> {
    filename
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty())
}

fn file_extension(filename: &str) -> Option<String> {
    filename
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_string())
        .filter(|extension| !extension.is_empty())
}

fn extension_from_url(original_url: &str) -> Option<String> {
    Url::parse(original_url).ok().and_then(|url| {
        url.path_segments()
            .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
            .and_then(file_extension)
    })
}

fn read_manifest_image_paths(manifest_path: &Path) -> Result<BTreeSet<String>> {
    if !manifest_path.is_file() {
        return Ok(BTreeSet::new());
    }

    let contents = fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read `{}`", manifest_path.display()))?;

    let mut paths = BTreeSet::new();
    for line in contents.lines() {
        if !line.starts_with('|') || line.contains("---") {
            continue;
        }
        let columns = line
            .trim_matches('|')
            .split('|')
            .map(|column| column.trim())
            .collect::<Vec<_>>();
        if columns.len() < 4 || columns[0] == "File" || columns[0].is_empty() {
            continue;
        }
        let filename = columns[0].trim_matches('`');
        paths.insert(format!("artifacts/{filename}"));
    }

    Ok(paths)
}

fn comment_source_label(body: &str, index: usize) -> String {
    for line in body.lines() {
        let normalized = normalize_comment_label_line(line);
        if !normalized.is_empty() {
            return truncate_chars(&normalized, 80);
        }
    }

    format!("comment-{index}")
}

fn normalize_comment_label_line(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let stripped = trimmed.trim_start_matches(|character: char| {
        character.is_ascii_whitespace()
            || matches!(character, '#' | '>' | '-' | '*' | '+' | '.' | ')')
            || character.is_ascii_digit()
    });
    let inline = strip_inline_markdown(stripped);
    collapse_whitespace(&inline)
}

fn strip_inline_markdown(line: &str) -> String {
    let mut rendered = String::new();
    let characters = line.chars().collect::<Vec<_>>();
    let mut index = 0usize;

    while index < characters.len() {
        if characters[index] == '!'
            && characters.get(index + 1) == Some(&'[')
            && let Some((replacement, consumed)) = bracketed_markdown_text(&characters, index + 1)
        {
            rendered.push_str(&replacement);
            index += consumed + 1;
            continue;
        }

        if characters[index] == '['
            && let Some((replacement, consumed)) = bracketed_markdown_text(&characters, index)
        {
            rendered.push_str(&replacement);
            index += consumed;
            continue;
        }

        if matches!(characters[index], '*' | '_' | '`' | '~') {
            index += 1;
            continue;
        }

        rendered.push(characters[index]);
        index += 1;
    }

    rendered
}

fn bracketed_markdown_text(characters: &[char], start: usize) -> Option<(String, usize)> {
    if characters.get(start) != Some(&'[') {
        return None;
    }
    let mut index = start + 1;
    while index < characters.len() && characters[index] != ']' {
        index += 1;
    }
    if index + 1 >= characters.len() || characters[index] != ']' || characters[index + 1] != '(' {
        return None;
    }

    let mut url_end = index + 2;
    while url_end < characters.len() && characters[url_end] != ')' {
        url_end += 1;
    }
    if url_end >= characters.len() {
        return None;
    }

    let replacement = characters[start + 1..index].iter().collect::<String>();
    Some((replacement, url_end - start + 1))
}

fn collapse_whitespace(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn build_discussion_context(comments: &[IssueComment], budget_chars: usize) -> String {
    if comments.is_empty() || budget_chars == 0 {
        return String::new();
    }

    let mut sections = comments
        .iter()
        .enumerate()
        .map(|(index, comment)| DiscussionSection {
            sort_key: comment.created_at.clone(),
            original_index: index,
            rendered: render_comment_section(comment),
        })
        .collect::<Vec<_>>();

    sections.sort_by(compare_discussion_sections);

    let total_chars: usize = sections
        .iter()
        .map(|section| section.rendered.chars().count())
        .sum();
    if total_chars <= budget_chars {
        return join_discussion_sections(
            &sections
                .into_iter()
                .map(|section| section.rendered)
                .collect::<Vec<_>>(),
        );
    }

    let mut selected = Vec::new();
    let mut used_chars = 0usize;
    for section in sections.iter().rev() {
        let section_chars = section.rendered.chars().count();
        if used_chars + section_chars <= budget_chars {
            selected.push(section.rendered.clone());
            used_chars += section_chars;
            continue;
        }
        if selected.is_empty() {
            selected.push(truncate_chars(&section.rendered, budget_chars));
            break;
        }
    }

    selected.reverse();
    join_discussion_sections(&selected)
}

fn compare_discussion_sections(left: &DiscussionSection, right: &DiscussionSection) -> Ordering {
    match (&left.sort_key, &right.sort_key) {
        (Some(left_key), Some(right_key)) => left_key
            .cmp(right_key)
            .then(left.original_index.cmp(&right.original_index)),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => left.original_index.cmp(&right.original_index),
    }
}

fn join_discussion_sections(sections: &[String]) -> String {
    sections.join("\n\n")
}

fn render_comment_section(comment: &IssueComment) -> String {
    let author = comment.user_name.as_deref().unwrap_or("Unknown");
    let date = comment
        .created_at
        .as_deref()
        .and_then(|created_at| created_at.get(..10))
        .unwrap_or("Unknown date");
    let body = comment.body.trim();

    format!("### **{author}** ({date})\n\n{body}")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let total_chars = value.chars().count();
    if total_chars <= max_chars {
        return value.to_string();
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }

    let truncated = value.chars().take(max_chars - 3).collect::<String>();
    format!("{truncated}...")
}

fn escape_markdown_cell(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::{
        PreparedIssueContext, TicketDiscussionBudgets, build_discussion_context,
        comment_source_label, prepare_issue_context, render_ticket_image_summary,
    };
    use crate::linear::{IssueComment, IssueLink, IssueSummary, TeamRef, WorkflowState};

    fn issue_with_context() -> IssueSummary {
        IssueSummary {
            id: "issue-1".to_string(),
            identifier: "MET-35".to_string(),
            title: "Localize ticket images".to_string(),
            description: Some(
                "Main issue\n\n![diagram](https://example.com/assets/diagram.png)\n".to_string(),
            ),
            url: "https://linear.app/issues/MET-35".to_string(),
            priority: Some(2),
            estimate: Some(3.0),
            updated_at: "2026-03-14T16:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "MET".to_string(),
                name: "Metastack".to_string(),
            },
            project: None,
            assignee: None,
            labels: Vec::new(),
            comments: vec![
                IssueComment {
                    id: "comment-1".to_string(),
                    body:
                        "Need parent art\n\n![comment-shot](https://example.com/uploads/shot.jpg)"
                            .to_string(),
                    created_at: Some("2026-03-16T10:00:00Z".to_string()),
                    user_name: Some("Alice".to_string()),
                    resolved_at: None,
                },
                IssueComment {
                    id: "comment-2".to_string(),
                    body: "Most recent note\n\nMore details".to_string(),
                    created_at: Some("2026-03-17T08:00:00Z".to_string()),
                    user_name: Some("Bob".to_string()),
                    resolved_at: None,
                },
            ],
            state: Some(WorkflowState {
                id: "state-1".to_string(),
                name: "In Progress".to_string(),
                kind: Some("started".to_string()),
            }),
            attachments: Vec::new(),
            parent: Some(IssueLink {
                id: "parent-1".to_string(),
                identifier: "MET-34".to_string(),
                title: "Parent".to_string(),
                url: "https://linear.app/issues/MET-34".to_string(),
                description: Some(
                    "Parent description\n\n![parent-art](https://example.com/path/parent.svg)"
                        .to_string(),
                ),
            }),
            children: Vec::new(),
        }
    }

    #[test]
    fn prepare_issue_context_rewrites_markdown_and_generates_prefixed_filenames() {
        let prepared =
            prepare_issue_context(&issue_with_context(), TicketDiscussionBudgets::default());

        assert_eq!(prepared.images.len(), 3);
        assert_eq!(prepared.images[0].filename, "diagram.png");
        assert_eq!(prepared.images[1].filename, "parent-parent.svg");
        assert_eq!(prepared.images[2].filename, "comment-1-shot.jpg");
        assert!(
            prepared.issue.description.as_deref().is_some_and(
                |description| description.contains("![diagram](artifacts/diagram.png)")
            )
        );
        assert!(
            prepared
                .issue
                .parent
                .as_ref()
                .and_then(|parent| parent.description.as_deref())
                .is_some_and(|description| {
                    description.contains("![parent-art](artifacts/parent-parent.svg)")
                })
        );
        assert!(
            prepared.issue.comments[0]
                .body
                .contains("![comment-shot](artifacts/comment-1-shot.jpg)")
        );
    }

    #[test]
    fn prepare_issue_context_falls_back_to_numbered_image_names_for_unusable_basenames() {
        let mut issue = issue_with_context();
        issue.description = Some("![alt](https://example.com/%%%%)".to_string());

        let prepared = prepare_issue_context(&issue, TicketDiscussionBudgets::default());

        assert_eq!(prepared.images[0].filename, "image-1.bin");
        assert!(
            prepared
                .issue
                .description
                .unwrap()
                .contains("artifacts/image-1.bin")
        );
    }

    #[test]
    fn comment_source_label_uses_first_meaningful_line_and_falls_back() {
        assert_eq!(
            comment_source_label("## Heading\n\n**Ship this now**", 2),
            "Heading"
        );
        assert_eq!(
            comment_source_label("\n\n![alt](https://example.com/a.png)", 4),
            "alt"
        );
        assert_eq!(comment_source_label("   \n   ", 7), "comment-7");
    }

    #[test]
    fn discussion_context_is_chronological_and_budgeted_to_most_recent_sections() {
        let issue = issue_with_context();
        let rendered = build_discussion_context(&issue.comments, 80);

        assert!(rendered.contains("### **Bob** (2026-03-17)"));
        assert!(!rendered.contains("### **Alice** (2026-03-16)"));

        let full = build_discussion_context(&issue.comments, 500);
        let alice_index = full
            .find("### **Alice** (2026-03-16)")
            .expect("alice section");
        let bob_index = full.find("### **Bob** (2026-03-17)").expect("bob section");
        assert!(alice_index < bob_index);
    }

    #[test]
    fn manifest_and_prompt_summary_include_all_discovered_images() {
        let prepared: PreparedIssueContext =
            prepare_issue_context(&issue_with_context(), TicketDiscussionBudgets::default());

        let manifest = prepared.image_manifest_markdown();
        let summary = render_ticket_image_summary(&prepared.images);

        assert!(manifest.contains("| `diagram.png` | diagram | Issue description |"));
        assert!(manifest.contains("| `comment-1-shot.jpg` | comment-shot | Need parent art |"));
        assert!(summary.contains("`parent-parent.svg` from Parent description"));
    }
}

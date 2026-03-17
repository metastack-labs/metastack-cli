use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};
use walkdir::WalkDir;

use crate::fs::{PlanningPaths, write_text_file};

pub const INDEX_FILE_NAME: &str = "index.md";
pub const METADATA_FILE_NAME: &str = ".linear.json";
const CANONICAL_PLACEHOLDERS: &[&str] = &[
    "{{BACKLOG_TITLE}}",
    "{{BACKLOG_SLUG}}",
    "{{TODAY}}",
    "{{issue_identifier}}",
    "{{issue_title}}",
    "{{issue_url}}",
    "{{parent_identifier}}",
    "{{parent_title}}",
    "{{parent_url}}",
    "{{parent_description}}",
];
const CANONICAL_TEMPLATE_FILES: &[(&str, &str)] = &[
    (
        "README.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/README.md"
        )),
    ),
    (
        "index.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/index.md"
        )),
    ),
    (
        "checklist.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/checklist.md"
        )),
    ),
    (
        "contacts.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/contacts.md"
        )),
    ),
    (
        "proposed-prs.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/proposed-prs.md"
        )),
    ),
    (
        "decisions.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/decisions.md"
        )),
    ),
    (
        "risks.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/risks.md"
        )),
    ),
    (
        "specification.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/specification.md"
        )),
    ),
    (
        "implementation.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/implementation.md"
        )),
    ),
    (
        "validation.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/validation.md"
        )),
    ),
    (
        "context/README.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/context/README.md"
        )),
    ),
    (
        "context/context-note-template.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/context/context-note-template.md"
        )),
    ),
    (
        "tasks/README.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/tasks/README.md"
        )),
    ),
    (
        "tasks/workstream-template.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/tasks/workstream-template.md"
        )),
    ),
    (
        "artifacts/README.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/artifacts/README.md"
        )),
    ),
    (
        "artifacts/artifact-template.md",
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/artifacts/BACKLOG_TEMPLATE/artifacts/artifact-template.md"
        )),
    ),
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BacklogIssueMetadata {
    pub issue_id: String,
    pub identifier: String,
    pub title: String,
    pub url: String,
    pub team_key: String,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub parent_identifier: Option<String>,
    #[serde(default)]
    pub managed_files: Vec<ManagedFileRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManagedFileRecord {
    pub path: String,
    #[serde(default)]
    pub attachment_id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TemplateContext {
    pub backlog_title: Option<String>,
    pub backlog_slug: Option<String>,
    pub today: Option<String>,
    pub issue_identifier: Option<String>,
    pub issue_title: Option<String>,
    pub issue_url: Option<String>,
    pub parent_identifier: Option<String>,
    pub parent_title: Option<String>,
    pub parent_url: Option<String>,
    pub parent_description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RenderedTemplateFile {
    pub relative_path: String,
    pub contents: String,
}

#[derive(Debug, Clone)]
pub struct LocalBacklogFile {
    pub relative_path: String,
    pub absolute_path: PathBuf,
    pub title: String,
    pub content_type: String,
    pub contents: Vec<u8>,
}

pub fn template_seed_files(paths: &PlanningPaths) -> Vec<(PathBuf, String)> {
    canonical_template_files()
        .into_iter()
        .map(|file| {
            (
                paths.backlog_template_dir.join(file.relative_path),
                file.contents,
            )
        })
        .collect()
}

pub fn template_seed_conflicts(template_dir: &Path) -> Result<Vec<String>> {
    let mut conflicts = Vec::new();

    for file in canonical_template_files() {
        let path = template_dir.join(&file.relative_path);
        if !path.exists() {
            continue;
        }

        let existing = fs::read_to_string(&path)
            .with_context(|| format!("failed to read `{}`", path.display()))?;
        if existing != file.contents {
            conflicts.push(file.relative_path);
        }
    }

    conflicts.sort();
    Ok(conflicts)
}

pub fn render_template_files(
    root: &Path,
    context: &TemplateContext,
) -> Result<Vec<RenderedTemplateFile>> {
    let paths = PlanningPaths::new(root);
    let context = resolve_template_context(context)?;
    let source_files = if paths.backlog_template_dir.is_dir() {
        read_template_files(&paths.backlog_template_dir)?
    } else {
        canonical_template_files()
    };

    Ok(source_files
        .into_iter()
        .map(|file| RenderedTemplateFile {
            relative_path: file.relative_path,
            contents: render_template(&file.contents, &context),
        })
        .collect())
}

pub fn write_rendered_backlog_item(
    root: &Path,
    identifier: &str,
    rendered_files: &[RenderedTemplateFile],
) -> Result<PathBuf> {
    let paths = PlanningPaths::new(root);
    let issue_dir = paths.backlog_issue_dir(identifier);

    for file in rendered_files {
        write_text_file(&issue_dir.join(&file.relative_path), &file.contents, true)?;
    }

    Ok(issue_dir)
}

pub fn backlog_issue_dir(root: &Path, identifier: &str) -> PathBuf {
    PlanningPaths::new(root).backlog_issue_dir(identifier)
}

pub fn backlog_issue_index_path(root: &Path, identifier: &str) -> PathBuf {
    backlog_issue_dir(root, identifier).join(INDEX_FILE_NAME)
}

pub fn backlog_issue_metadata_path(issue_dir: &Path) -> PathBuf {
    issue_dir.join(METADATA_FILE_NAME)
}

pub fn save_issue_metadata(issue_dir: &Path, metadata: &BacklogIssueMetadata) -> Result<()> {
    let path = backlog_issue_metadata_path(issue_dir);
    let contents =
        serde_json::to_string_pretty(metadata).context("failed to encode backlog metadata")?;
    write_text_file(&path, &contents, true)?;
    Ok(())
}

pub fn load_issue_metadata(issue_dir: &Path) -> Result<BacklogIssueMetadata> {
    let path = backlog_issue_metadata_path(issue_dir);
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    serde_json::from_str(&contents)
        .with_context(|| format!("failed to decode `{}`", path.display()))
}

pub fn write_issue_description(root: &Path, identifier: &str, contents: &str) -> Result<PathBuf> {
    let index_path = backlog_issue_index_path(root, identifier);
    write_text_file(&index_path, contents, true)?;
    Ok(index_path)
}

pub fn write_issue_attachment_file(
    issue_dir: &Path,
    relative_path: &str,
    contents: &[u8],
) -> Result<PathBuf> {
    let path = issue_dir.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    fs::write(&path, contents).with_context(|| format!("failed to write `{}`", path.display()))?;
    Ok(path)
}

pub fn collect_local_sync_files(issue_dir: &Path) -> Result<Vec<LocalBacklogFile>> {
    let mut files = WalkDir::new(issue_dir)
        .into_iter()
        .filter_entry(|entry| {
            if entry.depth() == 0 {
                return true;
            }

            entry
                .file_name()
                .to_str()
                .map(|name| !name.starts_with('.'))
                .unwrap_or(false)
        })
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_file() => Some(Ok(entry)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .map(|entry| -> Result<LocalBacklogFile> {
            let entry = entry.with_context(|| {
                format!(
                    "failed to traverse backlog issue directory `{}`",
                    issue_dir.display()
                )
            })?;
            let relative_path = relative_path(issue_dir, entry.path())?;

            let contents = fs::read(entry.path())
                .with_context(|| format!("failed to read `{}`", entry.path().display()))?;

            Ok(LocalBacklogFile {
                title: relative_path.clone(),
                content_type: content_type_for_path(entry.path()),
                absolute_path: entry.into_path(),
                relative_path,
                contents,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

pub fn ensure_no_unresolved_placeholders(rendered_files: &[RenderedTemplateFile]) -> Result<()> {
    for file in rendered_files {
        for placeholder in CANONICAL_PLACEHOLDERS {
            if file.contents.contains(placeholder) {
                bail!(
                    "backlog rendering left unresolved placeholder `{placeholder}` in `{}`",
                    file.relative_path
                );
            }
        }
    }

    Ok(())
}

fn canonical_template_files() -> Vec<RenderedTemplateFile> {
    CANONICAL_TEMPLATE_FILES
        .iter()
        .map(|(relative_path, contents)| RenderedTemplateFile {
            relative_path: (*relative_path).to_string(),
            contents: (*contents).to_string(),
        })
        .collect()
}

fn read_template_files(template_dir: &Path) -> Result<Vec<RenderedTemplateFile>> {
    let mut files = WalkDir::new(template_dir)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_file() => Some(Ok(entry)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .map(|entry| -> Result<RenderedTemplateFile> {
            let entry = entry
                .with_context(|| format!("failed to traverse `{}`", template_dir.display()))?;
            let contents = fs::read_to_string(entry.path())
                .with_context(|| format!("failed to read `{}`", entry.path().display()))?;

            Ok(RenderedTemplateFile {
                relative_path: relative_path(template_dir, entry.path())?,
                contents,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

fn relative_path(base: &Path, path: &Path) -> Result<String> {
    path.strip_prefix(base)
        .with_context(|| {
            format!(
                "failed to strip `{}` from `{}`",
                base.display(),
                path.display()
            )
        })
        .map(|path| path.to_string_lossy().replace('\\', "/"))
}

fn render_template(contents: &str, context: &ResolvedTemplateContext) -> String {
    [
        ("{{BACKLOG_TITLE}}", context.backlog_title.as_str()),
        ("{{BACKLOG_SLUG}}", context.backlog_slug.as_str()),
        ("{{TODAY}}", context.today.as_str()),
        ("{{issue_identifier}}", context.issue_identifier.as_str()),
        ("{{issue_title}}", context.issue_title.as_str()),
        ("{{issue_url}}", context.issue_url.as_str()),
        ("{{parent_identifier}}", context.parent_identifier.as_str()),
        ("{{parent_title}}", context.parent_title.as_str()),
        ("{{parent_url}}", context.parent_url.as_str()),
        (
            "{{parent_description}}",
            context.parent_description.as_str(),
        ),
    ]
    .into_iter()
    .fold(contents.to_string(), |rendered, (needle, value)| {
        rendered.replace(needle, value)
    })
}

struct ResolvedTemplateContext {
    backlog_title: String,
    backlog_slug: String,
    today: String,
    issue_identifier: String,
    issue_title: String,
    issue_url: String,
    parent_identifier: String,
    parent_title: String,
    parent_url: String,
    parent_description: String,
}

fn resolve_template_context(context: &TemplateContext) -> Result<ResolvedTemplateContext> {
    let backlog_title = context
        .backlog_title
        .clone()
        .or_else(|| context.issue_title.clone())
        .unwrap_or_else(|| "Backlog item".to_string());
    let backlog_slug = context
        .backlog_slug
        .clone()
        .unwrap_or_else(|| slugify(&backlog_title));
    let today = match &context.today {
        Some(today) => today.clone(),
        None => current_local_date()?,
    };
    let issue_identifier = context
        .issue_identifier
        .clone()
        .unwrap_or_else(|| "TBD".to_string());
    let issue_title = context
        .issue_title
        .clone()
        .unwrap_or_else(|| backlog_title.clone());
    let issue_url = context
        .issue_url
        .clone()
        .unwrap_or_else(|| "TBD".to_string());
    let parent_identifier = context
        .parent_identifier
        .clone()
        .unwrap_or_else(|| "None".to_string());
    let parent_title = context
        .parent_title
        .clone()
        .unwrap_or_else(|| "Standalone backlog item".to_string());
    let parent_url = context
        .parent_url
        .clone()
        .unwrap_or_else(|| "n/a".to_string());
    let parent_description = context.parent_description.clone().unwrap_or_default();

    Ok(ResolvedTemplateContext {
        backlog_title,
        backlog_slug,
        today,
        issue_identifier,
        issue_title,
        issue_url,
        parent_identifier,
        parent_title,
        parent_url,
        parent_description,
    })
}

fn current_local_date() -> Result<String> {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    OffsetDateTime::now_utc()
        .to_offset(offset)
        .format(&format_description!("[year]-[month]-[day]"))
        .context("failed to format the current date for the backlog template")
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "backlog-item".to_string()
    } else {
        slug
    }
}

fn content_type_for_path(path: &Path) -> String {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("md") => "text/markdown".to_string(),
        Some("txt") => "text/plain".to_string(),
        Some("json") => "application/json".to_string(),
        Some("toml") => "application/toml".to_string(),
        Some("yaml") | Some("yml") => "application/yaml".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

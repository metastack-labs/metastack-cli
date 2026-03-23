use std::fmt;
use std::fs;
use std::io::{ErrorKind, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Shared root layout for durable workflow state.
#[derive(Debug, Clone)]
pub(crate) struct WorkflowRootLayout {
    root: PathBuf,
    active_session_path: PathBuf,
}

impl WorkflowRootLayout {
    /// Create an install-scoped workflow layout rooted at the provided directory.
    pub(crate) fn install_scoped(root: PathBuf, active_session_file_name: &str) -> Self {
        let active_session_path = root.join(active_session_file_name);
        Self {
            root,
            active_session_path,
        }
    }

    /// Create a repo-local workflow layout under the provided repository root.
    pub(crate) fn repo_scoped(
        repo_root: &Path,
        relative_root: impl AsRef<Path>,
        active_session_file_name: &str,
    ) -> Self {
        Self::install_scoped(
            repo_root.join(relative_root.as_ref()),
            active_session_file_name,
        )
    }

    /// Return the workflow root directory.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Return the active-session marker path.
    pub(crate) fn active_session_path(&self) -> &Path {
        &self.active_session_path
    }

    /// Join an additional path segment under the workflow root.
    pub(crate) fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative.as_ref())
    }

    /// Build an active-session file handle rooted at this layout's marker path.
    pub(crate) fn active_session_file<T>(&self) -> ActiveSessionFile<T> {
        ActiveSessionFile::new(self.active_session_path.clone())
    }
}

/// Shared layout for workflows that store per-session directories under a root.
#[derive(Debug, Clone)]
pub(crate) struct WorkflowSessionLayout {
    workflow: WorkflowRootLayout,
    sessions_dir: PathBuf,
}

impl WorkflowSessionLayout {
    /// Create a session-oriented workflow layout from an existing root layout.
    pub(crate) fn with_sessions_dir(workflow: WorkflowRootLayout, sessions_dir_name: &str) -> Self {
        let sessions_dir = workflow.path(sessions_dir_name);
        Self {
            workflow,
            sessions_dir,
        }
    }

    /// Return the shared workflow root layout.
    pub(crate) fn workflow(&self) -> &WorkflowRootLayout {
        &self.workflow
    }

    /// Return the directory that stores per-session subdirectories.
    pub(crate) fn sessions_dir(&self) -> &Path {
        &self.sessions_dir
    }

    /// Return the directory for a specific session identifier.
    pub(crate) fn session_dir(&self, session_id: &str) -> PathBuf {
        self.sessions_dir.join(session_id)
    }
}

/// Shared wrapper around an active-session marker file.
#[derive(Debug, Clone)]
pub(crate) struct ActiveSessionFile<T> {
    path: PathBuf,
    _marker: PhantomData<T>,
}

impl<T> ActiveSessionFile<T> {
    /// Create a typed active-session file handle for the provided path.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            _marker: PhantomData,
        }
    }

    /// Return the underlying marker path.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl<T> ActiveSessionFile<T>
where
    T: Serialize + DeserializeOwned,
{
    /// Persist the marker payload, creating parent directories when needed.
    pub(crate) fn store(&self, value: &T) -> Result<()> {
        write_json(&self.path, value)
    }

    /// Load the marker payload, returning `None` when the file does not exist.
    pub(crate) fn load_optional(&self) -> Result<Option<T>> {
        read_optional_json(&self.path)
    }

    /// Attempt to create the marker file atomically.
    ///
    /// Returns `Ok(false)` when the file already exists.
    pub(crate) fn try_create_new(&self, value: &T) -> Result<bool> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create `{}`", parent.display()))?;
        }

        let contents = serde_json::to_vec_pretty(value)
            .with_context(|| format!("failed to serialize `{}`", self.path.display()))?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
        {
            Ok(mut file) => {
                file.write_all(&contents)
                    .with_context(|| format!("failed to write `{}`", self.path.display()))?;
                Ok(true)
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
            Err(error) => {
                Err(error).with_context(|| format!("failed to create `{}`", self.path.display()))
            }
        }
    }

    /// Remove the marker when the stored payload matches the provided predicate.
    pub(crate) fn remove_if<F>(&self, predicate: F) -> Result<bool>
    where
        F: FnOnce(&T) -> bool,
    {
        let Some(existing) = self.load_optional()? else {
            return Ok(false);
        };
        if !predicate(&existing) {
            return Ok(false);
        }

        match fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => {
                Err(error).with_context(|| format!("failed to remove `{}`", self.path.display()))
            }
        }
    }
}

/// Shared labeled summary field used by text and dashboard status rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SummaryField {
    pub(crate) label: &'static str,
    pub(crate) value: String,
}

impl SummaryField {
    /// Create a labeled summary field.
    pub(crate) fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
        }
    }
}

/// Add an optional labeled summary field when a value is present.
pub(crate) fn push_optional_summary_field<T>(
    fields: &mut Vec<SummaryField>,
    label: &'static str,
    value: Option<T>,
) where
    T: Into<String>,
{
    if let Some(value) = value {
        fields.push(SummaryField::new(label, value));
    }
}

/// Render aligned labeled summary fields into a text buffer.
pub(crate) fn write_summary_fields(
    out: &mut impl fmt::Write,
    fields: &[SummaryField],
    label_width: usize,
) -> fmt::Result {
    for field in fields {
        let label = format!("{}:", field.label);
        if label.len() >= label_width {
            writeln!(out, "{label} {}", field.value)?;
            continue;
        }
        writeln!(out, "{label:<width$}{}", field.value, width = label_width)?;
    }
    Ok(())
}

/// Write a JSON file with contextual filesystem errors.
pub(crate) fn write_json<T>(path: &Path, value: &T) -> Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let contents = serde_json::to_string_pretty(value)
        .with_context(|| format!("failed to serialize `{}`", path.display()))?;
    fs::write(path, contents).with_context(|| format!("failed to write `{}`", path.display()))
}

/// Read a required JSON file with contextual filesystem and decode errors.
pub(crate) fn read_json<T>(path: &Path) -> Result<T>
where
    T: DeserializeOwned,
{
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    serde_json::from_str(&contents)
        .with_context(|| format!("failed to decode `{}`", path.display()))
}

/// Read an optional JSON file, returning `None` when the file does not exist.
pub(crate) fn read_optional_json<T>(path: &Path) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    match fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents)
            .map(Some)
            .with_context(|| format!("failed to decode `{}`", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

/// Read an optional JSON file, suppressing decode failures.
pub(crate) fn read_optional_json_lossy<T>(path: &Path) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    match read_optional_json(path) {
        Ok(value) => Ok(value),
        Err(_error) => Ok(None),
    }
}

/// Load every JSON record in a directory, skipping malformed entries with a warning.
pub(crate) fn load_json_records<T>(
    dir: &Path,
    dir_label: &str,
    record_label: &str,
) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut records = Vec::new();
    for entry in fs::read_dir(dir)
        .with_context(|| format!("failed to read {dir_label}: {}", dir.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to read {dir_label} entry: {}", dir.display()))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {record_label}: {}", path.display()))?;
            match serde_json::from_str::<T>(&content) {
                Ok(record) => records.push(record),
                Err(err) => {
                    eprintln!(
                        "warning: skipping corrupted {record_label} at {}: {err}",
                        path.display()
                    );
                }
            }
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestMarker {
        value: String,
    }

    #[test]
    fn workflow_layout_builds_expected_paths() {
        let workflow = WorkflowRootLayout::repo_scoped(
            Path::new("/tmp/example"),
            ".metastack/orchestrate",
            "current.json",
        );
        let sessions = WorkflowSessionLayout::with_sessions_dir(workflow.clone(), "sessions");

        assert_eq!(
            workflow.root(),
            Path::new("/tmp/example/.metastack/orchestrate")
        );
        assert_eq!(
            workflow.active_session_path(),
            Path::new("/tmp/example/.metastack/orchestrate/current.json")
        );
        assert_eq!(
            sessions.sessions_dir(),
            Path::new("/tmp/example/.metastack/orchestrate/sessions")
        );
        assert_eq!(
            sessions.session_dir("sess-1"),
            Path::new("/tmp/example/.metastack/orchestrate/sessions/sess-1")
        );
    }

    #[test]
    fn active_session_file_round_trips_and_clears() {
        let dir = tempdir().unwrap();
        let file = ActiveSessionFile::<TestMarker>::new(dir.path().join("active.json"));
        let marker = TestMarker {
            value: "lock".to_string(),
        };

        assert!(file.try_create_new(&marker).unwrap());
        assert!(!file.try_create_new(&marker).unwrap());
        assert_eq!(file.load_optional().unwrap(), Some(marker.clone()));
        assert!(file.remove_if(|existing| existing.value == "lock").unwrap());
        assert_eq!(file.load_optional().unwrap(), None);
    }

    #[test]
    fn load_json_records_skips_corrupted_entries() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("good.json"), "{\"value\":\"ok\"}").unwrap();
        fs::write(dir.path().join("bad.json"), "{ not valid json").unwrap();

        let records =
            load_json_records::<TestMarker>(dir.path(), "test records", "test record").unwrap();

        assert_eq!(
            records,
            vec![TestMarker {
                value: "ok".to_string()
            }]
        );
    }
}

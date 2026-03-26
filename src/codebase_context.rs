use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::branding;
use crate::fs::PlanningPaths;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodebaseContextSection {
    Scan,
    Architecture,
    Concerns,
    Conventions,
    Integrations,
    Stack,
    Structure,
    Testing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissingCodebaseContextHint {
    Scan,
    ReloadOrScan,
}

impl CodebaseContextSection {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Scan => "SCAN.md",
            Self::Architecture => "ARCHITECTURE.md",
            Self::Concerns => "CONCERNS.md",
            Self::Conventions => "CONVENTIONS.md",
            Self::Integrations => "INTEGRATIONS.md",
            Self::Stack => "STACK.md",
            Self::Structure => "STRUCTURE.md",
            Self::Testing => "TESTING.md",
        }
    }

    pub(crate) fn path(self, paths: &PlanningPaths) -> PathBuf {
        match self {
            Self::Scan => paths.scan_path(),
            Self::Architecture => paths.architecture_path(),
            Self::Concerns => paths.concerns_path(),
            Self::Conventions => paths.conventions_path(),
            Self::Integrations => paths.integrations_path(),
            Self::Stack => paths.stack_path(),
            Self::Structure => paths.structure_path(),
            Self::Testing => paths.testing_path(),
        }
    }
}

pub(crate) fn load_codebase_context_bundle(
    root: &Path,
    sections: &[CodebaseContextSection],
    missing_hint: MissingCodebaseContextHint,
) -> Result<String> {
    let mut lines = Vec::new();

    for (title, path) in codebase_context_paths(root, sections) {
        lines.push(format!("## {title}"));
        lines.push(String::new());
        lines.push(read_codebase_context_file(root, &path, missing_hint)?);
        lines.push(String::new());
    }

    Ok(lines.join("\n"))
}

pub(crate) fn codebase_context_paths(
    root: &Path,
    sections: &[CodebaseContextSection],
) -> Vec<(&'static str, PathBuf)> {
    let paths = PlanningPaths::new(root);
    sections
        .iter()
        .copied()
        .map(|section| (section.title(), section.path(&paths)))
        .collect()
}

pub(crate) fn read_codebase_context_file(
    root: &Path,
    path: &Path,
    missing_hint: MissingCodebaseContextHint,
) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(render_missing_codebase_context(path, root, missing_hint))
        }
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

fn render_missing_codebase_context(
    path: &Path,
    root: &Path,
    missing_hint: MissingCodebaseContextHint,
) -> String {
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy())
        .unwrap_or_default();
    match missing_hint {
        MissingCodebaseContextHint::Scan => {
            format!(
                "_Missing `{file_name}`. Run `{} scan` to generate it._",
                branding::COMMAND_NAME
            )
        }
        MissingCodebaseContextHint::ReloadOrScan => format!(
            "_Missing `{file_name}`. Run `{} context reload --root {}` or `{} context scan --root {}` to generate it._",
            branding::COMMAND_NAME,
            root.display(),
            branding::COMMAND_NAME,
            root.display()
        ),
    }
}

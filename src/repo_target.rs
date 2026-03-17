use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepoTarget {
    project_name: String,
    repo_root: PathBuf,
    workspace_root: Option<PathBuf>,
}

impl RepoTarget {
    pub(crate) fn from_root(root: &Path) -> Self {
        Self {
            project_name: derive_project_name(root),
            repo_root: root.to_path_buf(),
            workspace_root: None,
        }
    }

    pub(crate) fn with_workspace(repo_root: &Path, workspace_root: &Path) -> Self {
        Self {
            project_name: derive_project_name(repo_root),
            repo_root: repo_root.to_path_buf(),
            workspace_root: Some(workspace_root.to_path_buf()),
        }
    }

    pub(crate) fn project_name(&self) -> &str {
        &self.project_name
    }

    pub(crate) fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub(crate) fn prompt_scope_block(&self) -> String {
        let mut lines = vec![
            "Target repository:".to_string(),
            format!("- Active project: `{}`", self.project_name),
            format!("- Repository root: `{}`", self.repo_root.display()),
            format!(
                "- Default scope: the full repository rooted at `{}`",
                self.repo_root.display()
            ),
            "- Scope rule: plan and modify only this repository unless the user explicitly asks for a narrower subproject within it.".to_string(),
            "- Backlog rule: create backlog issues only for work inside this repository directory.".to_string(),
        ];

        if let Some(workspace_root) = &self.workspace_root {
            lines.push(format!(
                "- Active workspace checkout: `{}`",
                workspace_root.display()
            ));
            lines.push(
                "- Workspace rule: implement, validate, and update local files only inside this workspace checkout."
                    .to_string(),
            );
        }

        lines.join("\n")
    }
}

fn derive_project_name(root: &Path) -> String {
    cargo_package_name(root)
        .or_else(|| package_json_name(root))
        .or_else(|| readme_title(root))
        .unwrap_or_else(|| {
            root.file_name()
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string())
        })
}

fn cargo_package_name(root: &Path) -> Option<String> {
    let contents = fs::read_to_string(root.join("Cargo.toml")).ok()?;
    let manifest = toml::from_str::<toml::Value>(&contents).ok()?;
    manifest
        .get("package")?
        .get("name")?
        .as_str()
        .map(ToString::to_string)
}

fn package_json_name(root: &Path) -> Option<String> {
    let contents = fs::read_to_string(root.join("package.json")).ok()?;
    let manifest = serde_json::from_str::<serde_json::Value>(&contents).ok()?;
    manifest.get("name")?.as_str().map(ToString::to_string)
}

fn readme_title(root: &Path) -> Option<String> {
    let contents = fs::read_to_string(root.join("README.md")).ok()?;
    contents.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# ")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::RepoTarget;

    #[test]
    fn repo_target_prefers_root_manifest_name_over_directory_name() {
        let temp = tempdir().expect("tempdir should create");
        let root = temp.path().join("MET-129");
        std::fs::create_dir_all(&root).expect("repo dir should exist");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"metastack-cli\"\nversion = \"0.1.0\"\n",
        )
        .expect("cargo manifest should write");

        let target = RepoTarget::from_root(&root);

        assert_eq!(target.project_name(), "metastack-cli");
        assert!(
            target
                .prompt_scope_block()
                .contains("Active project: `metastack-cli`")
        );
    }

    #[test]
    fn repo_target_includes_workspace_identity_when_present() {
        let temp = tempdir().expect("tempdir should create");
        let root = temp.path().join("repo");
        let workspace = temp.path().join("repo-workspace").join("MET-21");
        std::fs::create_dir_all(&root).expect("repo dir should exist");
        std::fs::create_dir_all(&workspace).expect("workspace dir should exist");

        let target = RepoTarget::with_workspace(&root, &workspace);
        let block = target.prompt_scope_block();

        assert!(block.contains(&format!("Repository root: `{}`", root.display())));
        assert!(block.contains(&format!(
            "Active workspace checkout: `{}`",
            workspace.display()
        )));
        assert!(block.contains("Workspace rule: implement, validate, and update local files only inside this workspace checkout."));
    }

    #[test]
    fn repo_target_can_resolve_self_hosted_name_from_repo_metadata() {
        let temp = tempdir().expect("tempdir should create");
        let root = temp.path().join("repo");
        std::fs::create_dir_all(&root).expect("repo dir should exist");
        std::fs::write(root.join("README.md"), "# MetaStack CLI\n").expect("readme should write");

        let target = RepoTarget::from_root(&root);

        assert_eq!(target.project_name(), "MetaStack CLI");
    }
}

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};

use crate::fs::{PlanningPaths, ensure_dir};

use super::state::ImproveState;

/// Load persisted improve state from `.metastack/agents/improve/`.
///
/// Returns the default empty state when no state file exists.
pub fn load_improve_state(paths: &PlanningPaths) -> Result<ImproveState> {
    let state_path = paths.improve_sessions_dir.join("state.json");
    match fs::read_to_string(&state_path) {
        Ok(content) => serde_json::from_str(&content).with_context(|| {
            format!(
                "invalid improve state JSON at `{}`",
                state_path.display()
            )
        }),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(ImproveState::default()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to read improve state from `{}`",
                state_path.display()
            )
        }),
    }
}

/// Save improve state to `.metastack/agents/improve/`.
///
/// Creates the directory structure if it does not exist.
#[allow(dead_code)]
pub fn save_improve_state(paths: &PlanningPaths, state: &ImproveState) -> Result<()> {
    ensure_dir(&paths.improve_sessions_dir)?;
    let state_path = paths.improve_sessions_dir.join("state.json");
    let content =
        serde_json::to_string_pretty(state).context("failed to serialize improve state")?;
    fs::write(&state_path, content)
        .with_context(|| format!("failed to write improve state to `{}`", state_path.display()))
}

/// Write the stacked PR body to a temp-like file under the improve sessions directory.
///
/// Returns the path to the written file.
#[allow(dead_code)]
pub fn write_pr_body_file(paths: &PlanningPaths, session_id: &str, body: &str) -> Result<std::path::PathBuf> {
    ensure_dir(&paths.improve_sessions_dir)?;
    let body_path = paths.improve_sessions_dir.join(format!("{session_id}.pr-body.md"));
    fs::write(&body_path, body)
        .with_context(|| format!("failed to write PR body to `{}`", body_path.display()))?;
    Ok(body_path)
}

/// Resolve the improve state file path for display.
pub fn state_file_display(paths: &PlanningPaths, root: &Path) -> String {
    let state_path = paths.improve_sessions_dir.join("state.json");
    crate::fs::display_path(&state_path, root)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::fs::PlanningPaths;
    use crate::improve::state::{ImprovePhase, ImproveSession, ImproveSourcePr, ImproveState};

    fn test_session(id: &str) -> ImproveSession {
        ImproveSession {
            session_id: id.to_string(),
            source_pr: ImproveSourcePr {
                number: 42,
                title: "Test PR".to_string(),
                url: "https://example.test/pull/42".to_string(),
                author: "alice".to_string(),
                head_branch: "feature".to_string(),
                base_branch: "main".to_string(),
            },
            instructions: "Fix tests".to_string(),
            phase: ImprovePhase::Queued,
            workspace_path: None,
            improve_branch: None,
            stacked_pr_number: None,
            stacked_pr_url: None,
            error_summary: None,
            created_at_epoch_seconds: 1000,
            updated_at_epoch_seconds: 1000,
        }
    }

    #[test]
    fn store_round_trips_state() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let paths = PlanningPaths::new(temp.path());

        let mut state = ImproveState::default();
        state.upsert(test_session("sess-1"));
        save_improve_state(&paths, &state)?;

        let loaded = load_improve_state(&paths)?;
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.sessions[0].session_id, "sess-1");
        Ok(())
    }

    #[test]
    fn load_returns_default_when_no_file() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let paths = PlanningPaths::new(temp.path());
        let state = load_improve_state(&paths)?;
        assert!(state.sessions.is_empty());
        Ok(())
    }

    #[test]
    fn write_pr_body_file_creates_file() -> anyhow::Result<()> {
        let temp = tempdir()?;
        let paths = PlanningPaths::new(temp.path());
        let path = write_pr_body_file(&paths, "sess-1", "# PR Body")?;
        assert!(path.exists());
        assert_eq!(fs::read_to_string(&path)?, "# PR Body");
        Ok(())
    }
}

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;

use crate::fs::PlanningPaths;
use crate::github_pr::{
    GhCli, PullRequestLifecycleResult, PullRequestPublishMode, PullRequestPublishRequest,
};

use super::state::{
    ImprovePhase, ImproveSession, ImproveSourcePr, ImproveState, improve_branch_name,
    stacked_pr_body, stacked_pr_title,
};
use super::store::{save_improve_state, write_pr_body_file};
use super::workspace::{ensure_improve_workspace, push_improve_branch};

/// Create a new improve session from a selected PR and instructions.
///
/// Persists the session in `Queued` phase and returns the session ID.
#[allow(dead_code)]
pub fn create_improve_session(
    paths: &PlanningPaths,
    state: &mut ImproveState,
    source_pr: &ImproveSourcePr,
    instructions: &str,
) -> Result<String> {
    let session_id = generate_session_id(source_pr.number);
    let now = now_epoch_seconds();

    let session = ImproveSession {
        session_id: session_id.clone(),
        source_pr: source_pr.clone(),
        instructions: instructions.to_string(),
        phase: ImprovePhase::Queued,
        workspace_path: None,
        improve_branch: None,
        stacked_pr_number: None,
        stacked_pr_url: None,
        error_summary: None,
        created_at_epoch_seconds: now,
        updated_at_epoch_seconds: now,
    };

    state.upsert(session);
    save_improve_state(paths, state)?;

    Ok(session_id)
}

/// Run the full improve execution for a session: provision workspace, then publish stacked PR.
///
/// Updates session state through each phase and persists after each transition.
/// Returns an error only for unrecoverable infrastructure issues; execution failures
/// are recorded in the session's `error_summary` field.
#[allow(dead_code)]
pub fn run_improve_session(
    root: &Path,
    paths: &PlanningPaths,
    state: &mut ImproveState,
    session_id: &str,
) -> Result<()> {
    // Phase: Queued -> Running
    update_session_phase(state, session_id, ImprovePhase::Running);
    save_improve_state(paths, state)?;

    let session = state
        .find_session(session_id)
        .ok_or_else(|| anyhow::anyhow!("session `{session_id}` not found"))?;
    let source_branch = session.source_pr.head_branch.clone();

    // Provision workspace
    let workspace = match ensure_improve_workspace(root, &source_branch, session_id) {
        Ok(ws) => ws,
        Err(err) => {
            fail_session(state, session_id, &format!("workspace provisioning failed: {err}"));
            save_improve_state(paths, state)?;
            return Ok(());
        }
    };

    // Record workspace info
    if let Some(s) = state
        .sessions
        .iter_mut()
        .find(|s| s.session_id == session_id)
    {
        s.workspace_path = Some(workspace.workspace_path.display().to_string());
        s.improve_branch = Some(workspace.improve_branch.clone());
        s.updated_at_epoch_seconds = now_epoch_seconds();
    }
    save_improve_state(paths, state)?;

    // Phase: Running -> Publishing
    update_session_phase(state, session_id, ImprovePhase::Publishing);
    save_improve_state(paths, state)?;

    // Push and publish stacked PR
    match publish_stacked_pr(paths, state, session_id, &workspace.workspace_path) {
        Ok(()) => {
            update_session_phase(state, session_id, ImprovePhase::Completed);
            save_improve_state(paths, state)?;
        }
        Err(err) => {
            fail_session(state, session_id, &format!("stacked PR publication failed: {err}"));
            save_improve_state(paths, state)?;
        }
    }

    Ok(())
}

/// Publish a stacked PR targeting the source PR branch.
///
/// Pushes the improve branch and creates a PR using `gh`.
fn publish_stacked_pr(
    paths: &PlanningPaths,
    state: &mut ImproveState,
    session_id: &str,
    workspace_path: &Path,
) -> Result<()> {
    let session = state
        .find_session(session_id)
        .ok_or_else(|| anyhow::anyhow!("session `{session_id}` not found"))?;

    let improve_branch = session
        .improve_branch
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("session has no improve branch set"))?
        .to_string();
    let source_branch = session.source_pr.head_branch.clone();
    let source_pr_number = session.source_pr.number;
    let instructions = session.instructions.clone();
    let source_pr = session.source_pr.clone();

    push_improve_branch(workspace_path, &improve_branch)?;

    let title = stacked_pr_title(source_pr_number, &instructions);
    let body = stacked_pr_body(&source_pr, &instructions);
    let body_path = write_pr_body_file(paths, session_id, &body)?;

    let gh = GhCli;
    let result: PullRequestLifecycleResult = gh.publish_branch_pull_request(
        workspace_path,
        PullRequestPublishRequest {
            head_branch: &improve_branch,
            base_branch: &source_branch,
            title: &title,
            body_path: &body_path,
            mode: PullRequestPublishMode::Ready,
        },
    )?;

    if let Some(s) = state
        .sessions
        .iter_mut()
        .find(|s| s.session_id == session_id)
    {
        s.stacked_pr_number = Some(result.number);
        s.stacked_pr_url = Some(result.url);
        s.updated_at_epoch_seconds = now_epoch_seconds();
    }

    Ok(())
}

fn update_session_phase(state: &mut ImproveState, session_id: &str, phase: ImprovePhase) {
    if let Some(session) = state
        .sessions
        .iter_mut()
        .find(|s| s.session_id == session_id)
    {
        session.phase = phase;
        session.updated_at_epoch_seconds = now_epoch_seconds();
    }
}

fn fail_session(state: &mut ImproveState, session_id: &str, error: &str) {
    if let Some(session) = state
        .sessions
        .iter_mut()
        .find(|s| s.session_id == session_id)
    {
        session.phase = ImprovePhase::Failed;
        session.error_summary = Some(error.to_string());
        session.updated_at_epoch_seconds = now_epoch_seconds();
    }
}

fn generate_session_id(pr_number: u64) -> String {
    let ts = now_epoch_seconds();
    format!("improve-{pr_number}-{ts}")
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Build the arguments that would be used for stacked PR publication.
///
/// Useful for asserting exact publication parameters in tests without running `gh`.
#[allow(dead_code)]
pub fn stacked_pr_publish_args(
    source_pr: &ImproveSourcePr,
    instructions: &str,
) -> (String, String, String, String) {
    let improve_branch = improve_branch_name(&source_pr.head_branch);
    let base_branch = source_pr.head_branch.clone();
    let title = stacked_pr_title(source_pr.number, instructions);
    let body = stacked_pr_body(source_pr, instructions);
    (improve_branch, base_branch, title, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_source_pr() -> ImproveSourcePr {
        ImproveSourcePr {
            number: 42,
            title: "Test PR #42".to_string(),
            url: "https://github.com/test/repo/pull/42".to_string(),
            author: "alice".to_string(),
            head_branch: "met-42-feature".to_string(),
            base_branch: "main".to_string(),
        }
    }

    #[test]
    fn create_session_persists_queued_state() {
        let temp = tempfile::tempdir().unwrap();
        let paths = PlanningPaths::new(temp.path());
        let mut state = ImproveState::default();

        let session_id = create_improve_session(
            &paths,
            &mut state,
            &test_source_pr(),
            "Fix the flaky test",
        )
        .unwrap();

        assert_eq!(state.sessions.len(), 1);
        let session = state.find_session(&session_id).unwrap();
        assert_eq!(session.phase, ImprovePhase::Queued);
        assert_eq!(session.instructions, "Fix the flaky test");
        assert_eq!(session.source_pr.number, 42);
    }

    #[test]
    fn stacked_pr_publish_args_derive_correctly() {
        let source = test_source_pr();
        let (improve_branch, base_branch, title, body) =
            stacked_pr_publish_args(&source, "Fix the tests");

        assert_eq!(improve_branch, "improve/met-42-feature");
        assert_eq!(base_branch, "met-42-feature");
        assert!(title.contains("#42"));
        assert!(title.contains("Fix the tests"));
        assert!(body.contains("met-42-feature"));
        assert!(body.contains("https://github.com/test/repo/pull/42"));
    }

    #[test]
    fn session_id_contains_pr_number() {
        let id = generate_session_id(42);
        assert!(id.starts_with("improve-42-"));
    }

    #[test]
    fn fail_session_records_error() {
        let temp = tempfile::tempdir().unwrap();
        let paths = PlanningPaths::new(temp.path());
        let mut state = ImproveState::default();

        let session_id = create_improve_session(
            &paths,
            &mut state,
            &test_source_pr(),
            "Fix tests",
        )
        .unwrap();

        fail_session(&mut state, &session_id, "clone failed");
        let session = state.find_session(&session_id).unwrap();
        assert_eq!(session.phase, ImprovePhase::Failed);
        assert_eq!(session.error_summary.as_deref(), Some("clone failed"));
    }

    #[test]
    fn update_phase_transitions_correctly() {
        let temp = tempfile::tempdir().unwrap();
        let paths = PlanningPaths::new(temp.path());
        let mut state = ImproveState::default();

        let session_id = create_improve_session(
            &paths,
            &mut state,
            &test_source_pr(),
            "Fix tests",
        )
        .unwrap();

        update_session_phase(&mut state, &session_id, ImprovePhase::Running);
        assert_eq!(
            state.find_session(&session_id).unwrap().phase,
            ImprovePhase::Running
        );

        update_session_phase(&mut state, &session_id, ImprovePhase::Publishing);
        assert_eq!(
            state.find_session(&session_id).unwrap().phase,
            ImprovePhase::Publishing
        );

        update_session_phase(&mut state, &session_id, ImprovePhase::Completed);
        assert_eq!(
            state.find_session(&session_id).unwrap().phase,
            ImprovePhase::Completed
        );
    }
}

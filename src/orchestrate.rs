use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::Utc;

use crate::cli::OrchestrateArgs;
use crate::fs::canonicalize_existing_dir;

pub(crate) mod dashboard;
pub(crate) mod state;

use self::dashboard::{OrchestrateStatus, build_status, render_status_json, render_status_text};
use self::state::{
    OrchestrateCycle, OrchestrateEvent, OrchestratePaths, OrchestratePhase, OrchestrateSession,
    StagingState, append_event, load_all_issue_readiness, load_all_review_records,
    load_current_pointer, load_events, load_session, load_staging_state, save_current_pointer,
    save_cycle, save_session, save_staging_state,
};

/// Run the repository-local orchestrator command.
///
/// Returns an error when the repository root cannot be resolved, the repository-local
/// `.metastack/` workspace is missing, git metadata cannot be read, or persisted orchestrator
/// state is missing/corrupted.
pub async fn run_orchestrate(args: &OrchestrateArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    ensure_metastack_dir(&root)?;

    let status = if args.status || args.render_once {
        load_status(&root)?
    } else {
        run_single_cycle(&root, args)?
    };

    if args.json {
        println!(
            "{}",
            render_status_json(&status).context("failed to encode orchestrator status as JSON")?
        );
    } else {
        print!("{}", render_status_text(&status));
    }

    Ok(())
}

fn ensure_metastack_dir(root: &Path) -> Result<()> {
    let metastack_dir = root.join(".metastack");
    if !metastack_dir.is_dir() {
        bail!("no .metastack/ directory found under `{}`", root.display());
    }
    Ok(())
}

fn run_single_cycle(root: &Path, args: &OrchestrateArgs) -> Result<OrchestrateStatus> {
    validate_staging_branch_name(args.staging_branch.as_deref())?;

    let paths = OrchestratePaths::new(root);
    let session_id = format!("orchestrate-{}", Utc::now().format("%Y%m%dT%H%M%SZ"));
    let staging_branch = args
        .staging_branch
        .clone()
        .unwrap_or_else(|| format!("staging/{session_id}"));
    let main_sha = resolve_head_sha(root)?;

    paths.ensure_session_dirs(&session_id)?;

    let mut session = OrchestrateSession::new(session_id.clone(), staging_branch, main_sha);
    session.config_source = "cli".to_string();
    save_session(&paths, &session)?;
    save_current_pointer(&paths, &session)?;
    append_event(
        &paths,
        &session_id,
        &OrchestrateEvent::new(
            "session_start",
            format!("Started orchestrate session {}", session.session_id),
        ),
    )?;

    let mut cycle = OrchestrateCycle::new("cycle-001".to_string());
    cycle.finish();
    save_cycle(&paths, &session_id, &cycle)?;
    let staging = StagingState::new(session.staging_branch.clone(), session.main_sha.clone());
    save_staging_state(&paths, &session_id, &staging)?;

    session.cycles_completed = 1;
    session.set_phase(OrchestratePhase::Completed);
    save_session(&paths, &session)?;
    append_event(
        &paths,
        &session_id,
        &OrchestrateEvent::new(
            "session_complete",
            format!("Completed orchestrate session {}", session.session_id),
        ),
    )?;

    load_status_for_session(&paths, &session)
}

fn load_status(root: &Path) -> Result<OrchestrateStatus> {
    let paths = OrchestratePaths::new(root);
    let Some(pointer) = load_current_pointer(&paths)? else {
        bail!(
            "no active orchestrator session found under `{}`",
            paths.root.display()
        );
    };

    let session = load_session(&paths, &pointer.session_id).with_context(|| {
        format!(
            "failed to load orchestrator session `{}`",
            pointer.session_id
        )
    })?;
    load_status_for_session(&paths, &session)
}

fn load_status_for_session(
    paths: &OrchestratePaths,
    session: &OrchestrateSession,
) -> Result<OrchestrateStatus> {
    let issues = load_all_issue_readiness(paths, &session.session_id)?;
    let reviews = load_all_review_records(paths, &session.session_id)?;
    let staging = load_staging_state(paths, &session.session_id)?;
    let events = load_events(paths, &session.session_id)?;
    Ok(build_status(
        session,
        &issues,
        &reviews,
        staging.as_ref(),
        &events,
        10,
    ))
}

fn validate_staging_branch_name(branch_name: Option<&str>) -> Result<()> {
    let Some(branch_name) = branch_name else {
        return Ok(());
    };

    if branch_name.is_empty() {
        bail!("staging branch name must not be empty");
    }
    if branch_name.contains("..") {
        bail!("staging branch name `{branch_name}` must not contain '..'");
    }
    if branch_name.starts_with('/') || branch_name.ends_with('/') {
        bail!("staging branch name `{branch_name}` must not start or end with '/'");
    }
    if branch_name.contains(' ') {
        bail!("staging branch name `{branch_name}` must not contain spaces");
    }

    Ok(())
}

fn resolve_head_sha(root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .context("failed to run `git rev-parse HEAD`")?;
    if !output.status.success() {
        bail!(
            "failed to resolve repository HEAD: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

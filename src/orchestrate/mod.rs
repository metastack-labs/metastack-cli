// Submodules expose public API for the daemon loop and tests. Some helpers are
// not yet called from the single-cycle placeholder; suppress dead-code until
// the full polling loop is wired.
#[allow(dead_code)]
pub mod backlog;
pub mod dashboard;
#[allow(dead_code)]
pub mod review;
#[allow(dead_code)]
pub mod staging;
#[allow(dead_code)]
pub mod state;

use std::path::Path;

use anyhow::{Context, Result, bail};
use chrono::Utc;

use crate::cli::OrchestrateArgs;
use crate::orchestrate::dashboard::{build_status, render_status_json, render_status_text};
use crate::orchestrate::staging::{default_staging_branch_name, validate_branch_name};
use crate::orchestrate::state::{
    OrchestrateEvent, OrchestratePaths, OrchestratePhase, OrchestrateSession, StagingState,
    append_event, load_all_issue_readiness, load_all_review_records, load_current_pointer,
    load_events, load_session, load_staging_state, save_current_pointer, save_session,
    save_staging_state,
};

/// Run the `meta agents orchestrate` command.
///
/// Depending on the flags this either starts the daemon, renders the current
/// status once, or prints help. The daemon mode runs a repeatable polling
/// cycle that analyses the backlog, coordinates PR reviews, and integrates
/// approved work into a staging branch.
pub async fn run_orchestrate(args: &OrchestrateArgs) -> Result<String> {
    let root = resolve_repo_root(&args.root)?;
    let paths = OrchestratePaths::new(&root);

    // Status/render-once: load existing state and render.
    if args.status || args.render_once {
        return render_current_status(&paths, args.json);
    }

    // Daemon execution: single cycle in the current implementation.
    run_orchestrate_session(&paths, args).await
}

/// Resolve the repository root from the provided path.
fn resolve_repo_root(root: &Path) -> Result<std::path::PathBuf> {
    let canonical = std::fs::canonicalize(root)
        .with_context(|| format!("failed to resolve repository root: {}", root.display()))?;
    if !canonical.join(".metastack").is_dir() {
        bail!(
            "no .metastack/ directory found at {}; run `meta runtime setup` first",
            canonical.display()
        );
    }
    Ok(canonical)
}

/// Render the current orchestrator status from persisted state.
fn render_current_status(paths: &OrchestratePaths, json: bool) -> Result<String> {
    let pointer = load_current_pointer(paths)?.context(
        "no active orchestrator session found; start one with `meta agents orchestrate`",
    )?;

    let session = load_session(paths, &pointer.session_id)
        .context("failed to load orchestrator session; state may be corrupted")?;
    let issues = load_all_issue_readiness(paths, &pointer.session_id)?;
    let reviews = load_all_review_records(paths, &pointer.session_id)?;
    let staging = load_staging_state(paths, &pointer.session_id)?;
    let events = load_events(paths, &pointer.session_id)?;

    let status = build_status(&session, &issues, &reviews, staging.as_ref(), &events, 20);

    if json {
        render_status_json(&status).context("failed to render orchestrate status as JSON")
    } else {
        Ok(render_status_text(&status))
    }
}

/// Run a single orchestrator session: initialize state, run one cycle, then
/// persist results.
///
/// In the initial implementation each invocation runs exactly one cycle. A
/// future iteration will add the long-running polling loop with configurable
/// intervals and signal handling.
async fn run_orchestrate_session(
    paths: &OrchestratePaths,
    args: &OrchestrateArgs,
) -> Result<String> {
    let session_id = generate_session_id();

    // Resolve staging branch name.
    let staging_branch = match &args.staging_branch {
        Some(name) => {
            validate_branch_name(name)?;
            name.clone()
        }
        None => default_staging_branch_name(&session_id),
    };

    // Resolve main SHA.
    let main_sha = resolve_main_sha(&args.root)?;

    // Initialize session.
    paths.ensure_session_dirs(&session_id)?;
    let mut session =
        OrchestrateSession::new(session_id.clone(), staging_branch.clone(), main_sha.clone());
    session.config_source = format!("root={}", args.root.display());
    save_session(paths, &session)?;
    save_current_pointer(paths, &session)?;

    // Initialize staging state.
    let staging = StagingState::new(staging_branch.clone(), main_sha.clone());
    save_staging_state(paths, &session_id, &staging)?;

    append_event(
        paths,
        &session_id,
        &OrchestrateEvent::new(
            "session_start",
            format!("Session {session_id} started with staging branch {staging_branch}"),
        ),
    )?;

    // Run one orchestration cycle.
    session.set_phase(OrchestratePhase::AnalyzingBacklog);
    save_session(paths, &session)?;
    append_event(
        paths,
        &session_id,
        &OrchestrateEvent::new("phase", "Analyzing backlog readiness"),
    )?;

    // Phase: backlog analysis (placeholder for Linear integration).
    // In the full implementation this queries Linear and classifies issues.
    // For now we record the phase transition.

    session.set_phase(OrchestratePhase::CoordinatingReviews);
    save_session(paths, &session)?;
    append_event(
        paths,
        &session_id,
        &OrchestrateEvent::new("phase", "Coordinating PR reviews"),
    )?;

    // Phase: review coordination (placeholder for gh/review integration).

    session.set_phase(OrchestratePhase::IntegratingStaging);
    save_session(paths, &session)?;
    append_event(
        paths,
        &session_id,
        &OrchestrateEvent::new("phase", "Integrating staging branch"),
    )?;

    // Phase: staging integration (placeholder for merge integration).

    // Cycle complete.
    session.cycles_completed += 1;
    session.set_phase(OrchestratePhase::Completed);
    save_session(paths, &session)?;
    append_event(
        paths,
        &session_id,
        &OrchestrateEvent::new(
            "session_complete",
            format!("Session {session_id} completed after 1 cycle"),
        ),
    )?;

    // Render final status.
    let issues = load_all_issue_readiness(paths, &session_id)?;
    let reviews = load_all_review_records(paths, &session_id)?;
    let staging_state = load_staging_state(paths, &session_id)?;
    let events = load_events(paths, &session_id)?;

    let status = build_status(
        &session,
        &issues,
        &reviews,
        staging_state.as_ref(),
        &events,
        20,
    );

    if args.json {
        render_status_json(&status).context("failed to render orchestrate status as JSON")
    } else {
        Ok(render_status_text(&status))
    }
}

/// Generate a unique session id based on the current timestamp.
fn generate_session_id() -> String {
    let now = Utc::now();
    now.format("%Y%m%d-%H%M%S").to_string()
}

/// Resolve the current SHA of `main` (or `origin/main`) from the repository.
fn resolve_main_sha(root: &Path) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .context("failed to run git rev-parse to resolve main SHA")?;

    if !output.status.success() {
        bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

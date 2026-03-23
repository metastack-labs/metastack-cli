use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::session_runtime::{
    ActiveSessionFile, WorkflowRootLayout, WorkflowSessionLayout, load_json_records, read_json,
    read_optional_json, write_json,
};

/// Version tag for forward-compatible state schema evolution.
const STATE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Top-level session
// ---------------------------------------------------------------------------

/// Persisted orchestrator session representing one daemon run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrateSession {
    pub version: u32,
    pub session_id: String,
    pub started_at: String,
    pub updated_at: String,
    pub phase: OrchestratePhase,
    pub staging_branch: String,
    pub main_sha: String,
    pub config_source: String,
    pub cycles_completed: u64,
}

impl OrchestrateSession {
    /// Create a new session with the given id and staging branch context.
    pub fn new(session_id: String, staging_branch: String, main_sha: String) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            version: STATE_VERSION,
            session_id,
            started_at: now.clone(),
            updated_at: now,
            phase: OrchestratePhase::Initializing,
            staging_branch,
            main_sha,
            config_source: String::new(),
            cycles_completed: 0,
        }
    }

    /// Mark the session phase and bump the update timestamp.
    pub fn set_phase(&mut self, phase: OrchestratePhase) {
        self.phase = phase;
        self.updated_at = Utc::now().to_rfc3339();
    }
}

/// High-level daemon phase visible in status output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratePhase {
    Initializing,
    AnalyzingBacklog,
    CoordinatingReviews,
    IntegratingStaging,
    Idle,
    ShuttingDown,
    Completed,
    Failed,
}

impl std::fmt::Display for OrchestratePhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Initializing => "Initializing",
            Self::AnalyzingBacklog => "Analyzing Backlog",
            Self::CoordinatingReviews => "Coordinating Reviews",
            Self::IntegratingStaging => "Integrating Staging",
            Self::Idle => "Idle",
            Self::ShuttingDown => "Shutting Down",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
        };
        write!(f, "{label}")
    }
}

// ---------------------------------------------------------------------------
// Cycle record
// ---------------------------------------------------------------------------

/// Record of a single orchestration polling cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrateCycle {
    pub cycle_id: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub issues_analyzed: u32,
    pub reviews_launched: u32,
    pub prs_merged: u32,
    pub errors: Vec<String>,
}

impl OrchestrateCycle {
    /// Start a new cycle record.
    pub fn new(cycle_id: String) -> Self {
        Self {
            cycle_id,
            started_at: Utc::now().to_rfc3339(),
            finished_at: None,
            issues_analyzed: 0,
            reviews_launched: 0,
            prs_merged: 0,
            errors: Vec::new(),
        }
    }

    /// Mark the cycle as complete.
    pub fn finish(&mut self) {
        self.finished_at = Some(Utc::now().to_rfc3339());
    }
}

// ---------------------------------------------------------------------------
// Issue readiness
// ---------------------------------------------------------------------------

/// Classification of a backlog issue's readiness for promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    Ready,
    Blocked,
    Deferred,
}

impl std::fmt::Display for ReadinessStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready => write!(f, "Ready"),
            Self::Blocked => write!(f, "Blocked"),
            Self::Deferred => write!(f, "Deferred"),
        }
    }
}

/// Persisted readiness decision for one backlog issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueReadiness {
    pub issue_identifier: String,
    pub issue_title: String,
    pub status: ReadinessStatus,
    pub reason: String,
    pub evaluated_at: String,
    pub promoted: bool,
    /// Hash of the readiness inputs used for churn suppression.
    pub decision_fingerprint: String,
}

// ---------------------------------------------------------------------------
// Review tracking
// ---------------------------------------------------------------------------

/// Status of a review run for a PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Queued,
    Running,
    Passed,
    ChangesRequested,
    Failed,
}

impl std::fmt::Display for ReviewStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queued => write!(f, "Queued"),
            Self::Running => write!(f, "Running"),
            Self::Passed => write!(f, "Passed"),
            Self::ChangesRequested => write!(f, "Changes Requested"),
            Self::Failed => write!(f, "Failed"),
        }
    }
}

/// Persisted review lineage for one PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRecord {
    pub pr_number: u64,
    pub issue_identifier: String,
    pub head_ref: String,
    pub status: ReviewStatus,
    pub launched_at: Option<String>,
    pub completed_at: Option<String>,
    /// Fingerprint of PR state at review launch for duplicate suppression.
    pub pr_state_fingerprint: String,
    pub is_canonical: bool,
    pub merge_eligible: bool,
}

// ---------------------------------------------------------------------------
// Staging state
// ---------------------------------------------------------------------------

/// Record of a PR merge attempt into the staging branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagingMergeRecord {
    pub pr_number: u64,
    pub issue_identifier: String,
    pub result: StagingMergeResult,
    pub attempted_at: String,
    pub commit_sha: Option<String>,
    pub error: Option<String>,
}

/// Outcome of a staging merge attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StagingMergeResult {
    Merged,
    Conflict,
    ValidationFailed,
    Skipped,
}

impl std::fmt::Display for StagingMergeResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Merged => write!(f, "Merged"),
            Self::Conflict => write!(f, "Conflict"),
            Self::ValidationFailed => write!(f, "Validation Failed"),
            Self::Skipped => write!(f, "Skipped"),
        }
    }
}

/// Persisted staging integration state for the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagingState {
    pub branch_name: String,
    pub base_sha: String,
    pub created_at: String,
    pub updated_at: String,
    pub merges: Vec<StagingMergeRecord>,
}

impl StagingState {
    /// Create a new staging state record.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(branch_name: String, base_sha: String) -> Self {
        let now = Utc::now().to_rfc3339();
        Self {
            branch_name,
            base_sha,
            created_at: now.clone(),
            updated_at: now,
            merges: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Event log
// ---------------------------------------------------------------------------

/// A single orchestrator event for the machine-readable event log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestrateEvent {
    pub timestamp: String,
    pub kind: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl OrchestrateEvent {
    /// Create a new event with the current timestamp.
    pub fn new(kind: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            timestamp: Utc::now().to_rfc3339(),
            kind: kind.into(),
            summary: summary.into(),
            detail: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Current-session pointer
// ---------------------------------------------------------------------------

/// Pointer to the active orchestrator session stored at `.metastack/orchestrate/current.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurrentSession {
    pub session_id: String,
    pub started_at: String,
}

// ---------------------------------------------------------------------------
// Persistence helpers
// ---------------------------------------------------------------------------

/// Paths for orchestrator durable state under `.metastack/orchestrate/`.
#[derive(Debug, Clone)]
pub struct OrchestratePaths {
    layout: WorkflowSessionLayout,
    pub root: PathBuf,
    pub current_path: PathBuf,
}

impl OrchestratePaths {
    /// Derive orchestrator paths from a repository root.
    pub fn new(repo_root: &Path) -> Self {
        let workflow =
            WorkflowRootLayout::repo_scoped(repo_root, ".metastack/orchestrate", "current.json");
        let layout = WorkflowSessionLayout::with_sessions_dir(workflow, "sessions");
        let root = layout.workflow().root().to_path_buf();
        let current_path = layout.workflow().active_session_path().to_path_buf();
        Self {
            layout,
            root,
            current_path,
        }
    }

    /// Directory for a specific session.
    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.layout.session_dir(session_id)
    }

    /// Session manifest path.
    pub fn session_path(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("session.json")
    }

    /// Cycles subdirectory for a session.
    pub fn cycles_dir(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("cycles")
    }

    /// Issues subdirectory for a session.
    pub fn issues_dir(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("issues")
    }

    /// Reviews subdirectory for a session.
    pub fn reviews_dir(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("reviews")
    }

    /// Staging state path for a session.
    pub fn staging_path(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("staging.json")
    }

    /// Event log path for a session.
    pub fn events_path(&self, session_id: &str) -> PathBuf {
        self.session_dir(session_id).join("events.log")
    }

    /// Ensure all required directories exist for a session.
    pub fn ensure_session_dirs(&self, session_id: &str) -> Result<()> {
        let dirs = [
            self.session_dir(session_id),
            self.cycles_dir(session_id),
            self.issues_dir(session_id),
            self.reviews_dir(session_id),
        ];
        for dir in &dirs {
            fs::create_dir_all(dir).with_context(|| {
                format!("failed to create orchestrate directory: {}", dir.display())
            })?;
        }
        Ok(())
    }

    fn current_session_file(&self) -> ActiveSessionFile<CurrentSession> {
        self.layout.workflow().active_session_file()
    }
}

/// Save a session to disk.
pub fn save_session(paths: &OrchestratePaths, session: &OrchestrateSession) -> Result<()> {
    let path = paths.session_path(&session.session_id);
    write_json(&path, session)
}

/// Load a session from disk.
pub fn load_session(paths: &OrchestratePaths, session_id: &str) -> Result<OrchestrateSession> {
    let path = paths.session_path(session_id);
    read_json(&path)
}

/// Save the current-session pointer.
pub fn save_current_pointer(paths: &OrchestratePaths, session: &OrchestrateSession) -> Result<()> {
    let pointer = CurrentSession {
        session_id: session.session_id.clone(),
        started_at: session.started_at.clone(),
    };
    paths.current_session_file().store(&pointer)
}

/// Load the current-session pointer, returning None if it does not exist.
pub fn load_current_pointer(paths: &OrchestratePaths) -> Result<Option<CurrentSession>> {
    read_optional_json(paths.current_session_file().path()).map_err(|error| {
        error.context(format!(
            "corrupted current session pointer: {}",
            paths.current_path.display()
        ))
    })
}

/// Save a cycle record.
pub fn save_cycle(
    paths: &OrchestratePaths,
    session_id: &str,
    cycle: &OrchestrateCycle,
) -> Result<()> {
    let path = paths
        .cycles_dir(session_id)
        .join(format!("{}.json", cycle.cycle_id));
    write_json(&path, cycle)
}

/// Save an issue readiness record.
#[cfg_attr(not(test), allow(dead_code))]
pub fn save_issue_readiness(
    paths: &OrchestratePaths,
    session_id: &str,
    record: &IssueReadiness,
) -> Result<()> {
    let path = paths
        .issues_dir(session_id)
        .join(format!("{}.json", record.issue_identifier));
    write_json(&path, record)
}

/// Load an issue readiness record, returning None if it does not exist.
#[cfg_attr(not(test), allow(dead_code))]
pub fn load_issue_readiness(
    paths: &OrchestratePaths,
    session_id: &str,
    issue_identifier: &str,
) -> Result<Option<IssueReadiness>> {
    let path = paths
        .issues_dir(session_id)
        .join(format!("{issue_identifier}.json"));
    read_optional_json(&path)
}

/// Load all issue readiness records for a session.
pub fn load_all_issue_readiness(
    paths: &OrchestratePaths,
    session_id: &str,
) -> Result<Vec<IssueReadiness>> {
    let dir = paths.issues_dir(session_id);
    let mut records: Vec<IssueReadiness> =
        load_json_records(&dir, "issues directory", "issue readiness")?;
    records.sort_by(|a, b| a.issue_identifier.cmp(&b.issue_identifier));
    Ok(records)
}

/// Save a review record.
#[cfg_attr(not(test), allow(dead_code))]
pub fn save_review_record(
    paths: &OrchestratePaths,
    session_id: &str,
    record: &ReviewRecord,
) -> Result<()> {
    let path = paths
        .reviews_dir(session_id)
        .join(format!("{}.json", record.pr_number));
    write_json(&path, record)
}

/// Load all review records for a session.
pub fn load_all_review_records(
    paths: &OrchestratePaths,
    session_id: &str,
) -> Result<Vec<ReviewRecord>> {
    let dir = paths.reviews_dir(session_id);
    let mut records: Vec<ReviewRecord> =
        load_json_records(&dir, "reviews directory", "review record")?;
    records.sort_by_key(|r| r.pr_number);
    Ok(records)
}

/// Save the staging state.
pub fn save_staging_state(
    paths: &OrchestratePaths,
    session_id: &str,
    state: &StagingState,
) -> Result<()> {
    let path = paths.staging_path(session_id);
    write_json(&path, state)
}

/// Load the staging state, returning None if it does not exist.
pub fn load_staging_state(
    paths: &OrchestratePaths,
    session_id: &str,
) -> Result<Option<StagingState>> {
    let path = paths.staging_path(session_id);
    read_optional_json(&path)
}

/// Append an event to the session event log.
pub fn append_event(
    paths: &OrchestratePaths,
    session_id: &str,
    event: &OrchestrateEvent,
) -> Result<()> {
    use std::io::Write;
    let path = paths.events_path(session_id);
    let line = serde_json::to_string(event).context("failed to serialize orchestrate event")?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("failed to open event log: {}", path.display()))?;
    writeln!(file, "{line}")
        .with_context(|| format!("failed to write to event log: {}", path.display()))?;
    Ok(())
}

/// Load all events from the session event log.
pub fn load_events(paths: &OrchestratePaths, session_id: &str) -> Result<Vec<OrchestrateEvent>> {
    let path = paths.events_path(session_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read event log: {}", path.display()))?;
    let mut events = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<OrchestrateEvent>(line) {
            Ok(event) => events.push(event),
            Err(err) => {
                eprintln!("warning: skipping malformed event log line: {err}");
            }
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn session_round_trip() {
        let dir = tempdir().unwrap();
        let paths = OrchestratePaths::new(dir.path());
        let session = OrchestrateSession::new(
            "test-session-001".to_string(),
            "staging/orchestrate-test".to_string(),
            "abc1234".to_string(),
        );
        paths.ensure_session_dirs(&session.session_id).unwrap();
        save_session(&paths, &session).unwrap();
        let loaded = load_session(&paths, &session.session_id).unwrap();
        assert_eq!(loaded.session_id, "test-session-001");
        assert_eq!(loaded.staging_branch, "staging/orchestrate-test");
        assert_eq!(loaded.main_sha, "abc1234");
        assert_eq!(loaded.phase, OrchestratePhase::Initializing);
    }

    #[test]
    fn current_pointer_round_trip() {
        let dir = tempdir().unwrap();
        let paths = OrchestratePaths::new(dir.path());
        fs::create_dir_all(&paths.root).unwrap();
        assert!(load_current_pointer(&paths).unwrap().is_none());
        let session = OrchestrateSession::new(
            "sess-ptr".to_string(),
            "staging/ptr".to_string(),
            "def5678".to_string(),
        );
        save_current_pointer(&paths, &session).unwrap();
        let loaded = load_current_pointer(&paths).unwrap().unwrap();
        assert_eq!(loaded.session_id, "sess-ptr");
    }

    #[test]
    fn issue_readiness_round_trip() {
        let dir = tempdir().unwrap();
        let paths = OrchestratePaths::new(dir.path());
        let sid = "issue-test-session";
        paths.ensure_session_dirs(sid).unwrap();
        let record = IssueReadiness {
            issue_identifier: "MET-100".to_string(),
            issue_title: "Test issue".to_string(),
            status: ReadinessStatus::Ready,
            reason: "All dependencies met".to_string(),
            evaluated_at: Utc::now().to_rfc3339(),
            promoted: true,
            decision_fingerprint: "fp-abc".to_string(),
        };
        save_issue_readiness(&paths, sid, &record).unwrap();
        let loaded = load_issue_readiness(&paths, sid, "MET-100")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.issue_identifier, "MET-100");
        assert_eq!(loaded.status, ReadinessStatus::Ready);

        let all = load_all_issue_readiness(&paths, sid).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn review_record_round_trip() {
        let dir = tempdir().unwrap();
        let paths = OrchestratePaths::new(dir.path());
        let sid = "review-test-session";
        paths.ensure_session_dirs(sid).unwrap();
        let record = ReviewRecord {
            pr_number: 42,
            issue_identifier: "MET-200".to_string(),
            head_ref: "feature/met-200".to_string(),
            status: ReviewStatus::Passed,
            launched_at: Some(Utc::now().to_rfc3339()),
            completed_at: Some(Utc::now().to_rfc3339()),
            pr_state_fingerprint: "fp-review".to_string(),
            is_canonical: true,
            merge_eligible: true,
        };
        save_review_record(&paths, sid, &record).unwrap();
        let all = load_all_review_records(&paths, sid).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].pr_number, 42);
    }

    #[test]
    fn staging_state_round_trip() {
        let dir = tempdir().unwrap();
        let paths = OrchestratePaths::new(dir.path());
        let sid = "staging-test-session";
        paths.ensure_session_dirs(sid).unwrap();
        let state = StagingState::new("staging/test".to_string(), "aaa1111".to_string());
        save_staging_state(&paths, sid, &state).unwrap();
        let loaded = load_staging_state(&paths, sid).unwrap().unwrap();
        assert_eq!(loaded.branch_name, "staging/test");
        assert_eq!(loaded.base_sha, "aaa1111");
    }

    #[test]
    fn event_log_append_and_load() {
        let dir = tempdir().unwrap();
        let paths = OrchestratePaths::new(dir.path());
        let sid = "event-test-session";
        paths.ensure_session_dirs(sid).unwrap();
        append_event(
            &paths,
            sid,
            &OrchestrateEvent::new("init", "Session started"),
        )
        .unwrap();
        append_event(
            &paths,
            sid,
            &OrchestrateEvent::new("cycle", "Cycle 1 completed"),
        )
        .unwrap();
        let events = load_events(&paths, sid).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "init");
        assert_eq!(events[1].kind, "cycle");
    }

    #[test]
    fn cycle_round_trip() {
        let dir = tempdir().unwrap();
        let paths = OrchestratePaths::new(dir.path());
        let sid = "cycle-test-session";
        paths.ensure_session_dirs(sid).unwrap();
        let mut cycle = OrchestrateCycle::new("cycle-001".to_string());
        cycle.issues_analyzed = 5;
        cycle.finish();
        save_cycle(&paths, sid, &cycle).unwrap();
        // Verify file exists
        let path = paths.cycles_dir(sid).join("cycle-001.json");
        assert!(path.exists());
    }

    #[test]
    fn corrupted_state_returns_contextual_error() {
        let dir = tempdir().unwrap();
        let paths = OrchestratePaths::new(dir.path());
        fs::create_dir_all(&paths.root).unwrap();
        fs::write(&paths.current_path, "not valid json").unwrap();
        let result = load_current_pointer(&paths);
        assert!(result.is_err());
        let err_msg = format!("{:#}", result.unwrap_err());
        assert!(err_msg.contains("corrupted current session pointer"));
    }
}

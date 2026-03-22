use std::fmt::Write;

use serde::Serialize;

use crate::orchestrate::state::{
    IssueReadiness, OrchestrateEvent, OrchestratePhase, OrchestrateSession, ReviewRecord,
    StagingMergeResult, StagingState,
};
use crate::session_runtime::{SummaryField, write_summary_fields};

/// Structured status snapshot that can be rendered as human-readable text or
/// serialized to JSON for machine consumption.
#[derive(Debug, Clone, Serialize)]
pub struct OrchestrateStatus {
    pub session_id: String,
    pub phase: OrchestratePhase,
    pub started_at: String,
    pub updated_at: String,
    pub cycles_completed: u64,
    pub staging_branch: String,
    pub main_sha: String,
    pub issues: Vec<IssueStatusEntry>,
    pub reviews: Vec<ReviewStatusEntry>,
    pub staging: Option<StagingStatusEntry>,
    pub recent_events: Vec<EventEntry>,
}

/// Issue readiness summary for status output.
#[derive(Debug, Clone, Serialize)]
pub struct IssueStatusEntry {
    pub identifier: String,
    pub title: String,
    pub readiness: String,
    pub reason: String,
    pub promoted: bool,
}

/// Review summary for status output.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewStatusEntry {
    pub pr_number: u64,
    pub issue_identifier: String,
    pub head_ref: String,
    pub status: String,
    pub is_canonical: bool,
    pub merge_eligible: bool,
}

/// Staging integration summary for status output.
#[derive(Debug, Clone, Serialize)]
pub struct StagingStatusEntry {
    pub branch_name: String,
    pub base_sha: String,
    pub merged_count: u32,
    pub conflict_count: u32,
    pub validation_failed_count: u32,
    pub merges: Vec<StagingMergeEntry>,
}

/// Individual staging merge entry for status output.
#[derive(Debug, Clone, Serialize)]
pub struct StagingMergeEntry {
    pub pr_number: u64,
    pub issue_identifier: String,
    pub result: String,
}

/// Recent event for status output.
#[derive(Debug, Clone, Serialize)]
pub struct EventEntry {
    pub timestamp: String,
    pub kind: String,
    pub summary: String,
}

/// Build the status snapshot from persisted state.
pub fn build_status(
    session: &OrchestrateSession,
    issues: &[IssueReadiness],
    reviews: &[ReviewRecord],
    staging: Option<&StagingState>,
    events: &[OrchestrateEvent],
    max_events: usize,
) -> OrchestrateStatus {
    let issue_entries: Vec<IssueStatusEntry> = issues
        .iter()
        .map(|i| IssueStatusEntry {
            identifier: i.issue_identifier.clone(),
            title: i.issue_title.clone(),
            readiness: i.status.to_string(),
            reason: i.reason.clone(),
            promoted: i.promoted,
        })
        .collect();

    let review_entries: Vec<ReviewStatusEntry> = reviews
        .iter()
        .map(|r| ReviewStatusEntry {
            pr_number: r.pr_number,
            issue_identifier: r.issue_identifier.clone(),
            head_ref: r.head_ref.clone(),
            status: r.status.to_string(),
            is_canonical: r.is_canonical,
            merge_eligible: r.merge_eligible,
        })
        .collect();

    let staging_entry = staging.map(|s| {
        let mut merged = 0u32;
        let mut conflicts = 0u32;
        let mut validation_failed = 0u32;
        let merges: Vec<StagingMergeEntry> = s
            .merges
            .iter()
            .map(|m| {
                match m.result {
                    StagingMergeResult::Merged => merged += 1,
                    StagingMergeResult::Conflict => conflicts += 1,
                    StagingMergeResult::ValidationFailed => validation_failed += 1,
                    StagingMergeResult::Skipped => {}
                }
                StagingMergeEntry {
                    pr_number: m.pr_number,
                    issue_identifier: m.issue_identifier.clone(),
                    result: m.result.to_string(),
                }
            })
            .collect();
        StagingStatusEntry {
            branch_name: s.branch_name.clone(),
            base_sha: s.base_sha.clone(),
            merged_count: merged,
            conflict_count: conflicts,
            validation_failed_count: validation_failed,
            merges,
        }
    });

    let recent: Vec<EventEntry> = events
        .iter()
        .rev()
        .take(max_events)
        .map(|e| EventEntry {
            timestamp: e.timestamp.clone(),
            kind: e.kind.clone(),
            summary: e.summary.clone(),
        })
        .collect();

    OrchestrateStatus {
        session_id: session.session_id.clone(),
        phase: session.phase,
        started_at: session.started_at.clone(),
        updated_at: session.updated_at.clone(),
        cycles_completed: session.cycles_completed,
        staging_branch: session.staging_branch.clone(),
        main_sha: session.main_sha.clone(),
        issues: issue_entries,
        reviews: review_entries,
        staging: staging_entry,
        recent_events: recent,
    }
}

/// Render the status snapshot as human-readable text.
pub fn render_status_text(status: &OrchestrateStatus) -> String {
    let mut out = String::new();
    let header = vec![
        SummaryField::new("Orchestrate Session", status.session_id.clone()),
        SummaryField::new("Phase", status.phase.to_string()),
        SummaryField::new("Started", status.started_at.clone()),
        SummaryField::new("Updated", status.updated_at.clone()),
        SummaryField::new("Cycles", status.cycles_completed.to_string()),
        SummaryField::new("Staging branch", status.staging_branch.clone()),
        SummaryField::new("Main SHA", status.main_sha.clone()),
    ];
    write_summary_fields(&mut out, &header, 18).unwrap();

    if !status.issues.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "Issues ({}):", status.issues.len()).unwrap();
        for entry in &status.issues {
            let promoted_marker = if entry.promoted { " [promoted]" } else { "" };
            writeln!(
                out,
                "  {:<12} {:<8} {}{}",
                entry.identifier, entry.readiness, entry.reason, promoted_marker
            )
            .unwrap();
        }
    }

    if !status.reviews.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "Reviews ({}):", status.reviews.len()).unwrap();
        for entry in &status.reviews {
            let canonical = if entry.is_canonical {
                " [canonical]"
            } else {
                ""
            };
            let eligible = if entry.merge_eligible {
                " [merge-eligible]"
            } else {
                ""
            };
            writeln!(
                out,
                "  PR #{:<6} {:<12} {:<20} {}{}{}",
                entry.pr_number,
                entry.issue_identifier,
                entry.status,
                entry.head_ref,
                canonical,
                eligible
            )
            .unwrap();
        }
    }

    if let Some(ref staging) = status.staging {
        writeln!(out).unwrap();
        writeln!(out, "Staging:").unwrap();
        writeln!(out, "  Branch:     {}", staging.branch_name).unwrap();
        writeln!(out, "  Base SHA:   {}", staging.base_sha).unwrap();
        writeln!(
            out,
            "  Merged: {}  Conflicts: {}  Validation failures: {}",
            staging.merged_count, staging.conflict_count, staging.validation_failed_count
        )
        .unwrap();
        if !staging.merges.is_empty() {
            for merge in &staging.merges {
                writeln!(
                    out,
                    "    PR #{:<6} {:<12} {}",
                    merge.pr_number, merge.issue_identifier, merge.result
                )
                .unwrap();
            }
        }
    }

    if !status.recent_events.is_empty() {
        writeln!(out).unwrap();
        writeln!(out, "Recent events:").unwrap();
        for event in &status.recent_events {
            writeln!(
                out,
                "  [{}] {}: {}",
                event.timestamp, event.kind, event.summary
            )
            .unwrap();
        }
    }

    out
}

/// Render the status snapshot as JSON.
pub fn render_status_json(status: &OrchestrateStatus) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate::state::*;

    #[test]
    fn status_renders_text_with_all_sections() {
        let session = OrchestrateSession::new(
            "test-001".to_string(),
            "staging/test".to_string(),
            "abc1234".to_string(),
        );
        let issues = vec![IssueReadiness {
            issue_identifier: "MET-10".to_string(),
            issue_title: "Test issue".to_string(),
            status: ReadinessStatus::Ready,
            reason: "all checks passed".to_string(),
            evaluated_at: "2026-03-21T00:00:00Z".to_string(),
            promoted: true,
            decision_fingerprint: "fp".to_string(),
        }];
        let reviews = vec![ReviewRecord {
            pr_number: 5,
            issue_identifier: "MET-10".to_string(),
            head_ref: "feature/met-10".to_string(),
            status: ReviewStatus::Passed,
            launched_at: None,
            completed_at: None,
            pr_state_fingerprint: "fp".to_string(),
            is_canonical: true,
            merge_eligible: true,
        }];
        let staging = StagingState::new("staging/test".to_string(), "abc1234".to_string());
        let events = vec![OrchestrateEvent::new("init", "Session started")];

        let status = build_status(&session, &issues, &reviews, Some(&staging), &events, 10);
        let text = render_status_text(&status);

        assert!(text.contains("Orchestrate Session: test-001"));
        assert!(text.contains("Initializing"));
        assert!(text.contains("MET-10"));
        assert!(text.contains("PR #5"));
        assert!(text.contains("[canonical]"));
        assert!(text.contains("[merge-eligible]"));
        assert!(text.contains("staging/test"));
        assert!(text.contains("Session started"));
    }

    #[test]
    fn status_renders_json() {
        let session = OrchestrateSession::new(
            "json-test".to_string(),
            "staging/json".to_string(),
            "def5678".to_string(),
        );
        let status = build_status(&session, &[], &[], None, &[], 10);
        let json = render_status_json(&status).unwrap();
        assert!(json.contains("\"session_id\": \"json-test\""));
        assert!(json.contains("\"phase\": \"initializing\""));
    }
}

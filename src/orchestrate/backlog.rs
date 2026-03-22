use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::orchestrate::state::{IssueReadiness, ReadinessStatus};

/// Lightweight issue summary used for readiness classification.
#[derive(Debug, Clone)]
pub struct BacklogCandidate {
    pub identifier: String,
    pub title: String,
    pub state_name: String,
    pub priority: Option<u32>,
    pub estimate: Option<f64>,
    pub labels: Vec<String>,
    pub has_description: bool,
    pub has_acceptance_criteria: bool,
    pub blocking_identifiers: Vec<String>,
    pub parent_identifier: Option<String>,
}

/// Classify a backlog candidate as ready, blocked, or deferred.
///
/// Readiness heuristics:
/// - Blocked: the issue has unresolved blocking dependencies that are not yet done.
/// - Deferred: the issue lacks required metadata (description, priority) or has a
///   label indicating deferral.
/// - Ready: all checks pass.
pub fn classify_readiness(
    candidate: &BacklogCandidate,
    done_identifiers: &BTreeSet<String>,
) -> (ReadinessStatus, String) {
    // Check blocking dependencies.
    let unresolved: Vec<&str> = candidate
        .blocking_identifiers
        .iter()
        .filter(|id| !done_identifiers.contains(id.as_str()))
        .map(String::as_str)
        .collect();

    if !unresolved.is_empty() {
        return (
            ReadinessStatus::Blocked,
            format!(
                "blocked by unresolved dependencies: {}",
                unresolved.join(", ")
            ),
        );
    }

    // Check for deferral labels.
    let defer_labels = ["deferred", "on-hold", "wontfix", "icebox"];
    for label in &candidate.labels {
        let lower = label.to_lowercase();
        if defer_labels.iter().any(|dl| lower.contains(dl)) {
            return (
                ReadinessStatus::Deferred,
                format!("deferred due to label: {label}"),
            );
        }
    }

    // Check minimum metadata.
    if !candidate.has_description {
        return (
            ReadinessStatus::Deferred,
            "deferred: issue has no description".to_string(),
        );
    }

    (
        ReadinessStatus::Ready,
        "all readiness checks passed".to_string(),
    )
}

/// Compute a fingerprint over the inputs that drive readiness classification.
///
/// If the fingerprint has not changed since the last evaluation, the decision
/// can be suppressed to avoid churn.
pub fn readiness_fingerprint(candidate: &BacklogCandidate) -> String {
    let mut hasher = Sha256::new();
    hasher.update(candidate.identifier.as_bytes());
    hasher.update(candidate.state_name.as_bytes());
    hasher.update(candidate.has_description.to_string().as_bytes());
    if let Some(p) = candidate.priority {
        hasher.update(p.to_le_bytes());
    }
    for id in &candidate.blocking_identifiers {
        hasher.update(id.as_bytes());
    }
    for label in &candidate.labels {
        hasher.update(label.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Determine whether a readiness re-evaluation should be suppressed because the
/// inputs have not changed since the last recorded decision.
pub fn should_suppress_reevaluation(
    previous: Option<&IssueReadiness>,
    current_fingerprint: &str,
) -> bool {
    match previous {
        Some(prev) => prev.decision_fingerprint == current_fingerprint,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_candidate() -> BacklogCandidate {
        BacklogCandidate {
            identifier: "MET-100".to_string(),
            title: "Test issue".to_string(),
            state_name: "Backlog".to_string(),
            priority: Some(2),
            estimate: Some(3.0),
            labels: vec!["tech".to_string()],
            has_description: true,
            has_acceptance_criteria: true,
            blocking_identifiers: Vec::new(),
            parent_identifier: None,
        }
    }

    #[test]
    fn ready_when_all_checks_pass() {
        let candidate = base_candidate();
        let done = BTreeSet::new();
        let (status, _reason) = classify_readiness(&candidate, &done);
        assert_eq!(status, ReadinessStatus::Ready);
    }

    #[test]
    fn blocked_by_unresolved_dependency() {
        let mut candidate = base_candidate();
        candidate.blocking_identifiers = vec!["MET-99".to_string()];
        let done = BTreeSet::new();
        let (status, reason) = classify_readiness(&candidate, &done);
        assert_eq!(status, ReadinessStatus::Blocked);
        assert!(reason.contains("MET-99"));
    }

    #[test]
    fn resolved_dependency_is_not_blocking() {
        let mut candidate = base_candidate();
        candidate.blocking_identifiers = vec!["MET-99".to_string()];
        let done = BTreeSet::from(["MET-99".to_string()]);
        let (status, _reason) = classify_readiness(&candidate, &done);
        assert_eq!(status, ReadinessStatus::Ready);
    }

    #[test]
    fn deferred_by_label() {
        let mut candidate = base_candidate();
        candidate.labels = vec!["deferred".to_string()];
        let done = BTreeSet::new();
        let (status, reason) = classify_readiness(&candidate, &done);
        assert_eq!(status, ReadinessStatus::Deferred);
        assert!(reason.contains("deferred"));
    }

    #[test]
    fn deferred_when_no_description() {
        let mut candidate = base_candidate();
        candidate.has_description = false;
        let done = BTreeSet::new();
        let (status, reason) = classify_readiness(&candidate, &done);
        assert_eq!(status, ReadinessStatus::Deferred);
        assert!(reason.contains("no description"));
    }

    #[test]
    fn fingerprint_changes_when_inputs_change() {
        let candidate = base_candidate();
        let fp1 = readiness_fingerprint(&candidate);

        let mut changed = base_candidate();
        changed.blocking_identifiers = vec!["MET-50".to_string()];
        let fp2 = readiness_fingerprint(&changed);

        assert_ne!(fp1, fp2);
    }

    #[test]
    fn suppress_reevaluation_when_fingerprint_unchanged() {
        let previous = IssueReadiness {
            issue_identifier: "MET-100".to_string(),
            issue_title: "Test".to_string(),
            status: ReadinessStatus::Ready,
            reason: "ok".to_string(),
            evaluated_at: "2026-03-21T00:00:00Z".to_string(),
            promoted: false,
            decision_fingerprint: "same-fp".to_string(),
        };
        assert!(should_suppress_reevaluation(Some(&previous), "same-fp"));
        assert!(!should_suppress_reevaluation(
            Some(&previous),
            "different-fp"
        ));
        assert!(!should_suppress_reevaluation(None, "any-fp"));
    }
}

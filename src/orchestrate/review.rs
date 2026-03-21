use sha2::{Digest, Sha256};

use crate::orchestrate::state::{ReviewRecord, ReviewStatus};

/// Lightweight PR summary used for review coordination decisions.
#[derive(Debug, Clone)]
pub struct PullRequestCandidate {
    pub number: u64,
    pub title: String,
    pub head_ref: String,
    pub base_ref: String,
    pub state: String,
    pub is_draft: bool,
    pub updated_at: String,
    pub issue_identifier: Option<String>,
    pub review_decision: Option<String>,
    pub check_status: Option<String>,
}

/// Compute a fingerprint of the PR state used for duplicate review suppression.
///
/// Reviews should not be relaunched when the fingerprint has not changed.
pub fn pr_state_fingerprint(pr: &PullRequestCandidate) -> String {
    let mut hasher = Sha256::new();
    hasher.update(pr.number.to_le_bytes());
    hasher.update(pr.head_ref.as_bytes());
    hasher.update(pr.state.as_bytes());
    hasher.update(pr.is_draft.to_string().as_bytes());
    hasher.update(pr.updated_at.as_bytes());
    if let Some(ref decision) = pr.review_decision {
        hasher.update(decision.as_bytes());
    }
    if let Some(ref check) = pr.check_status {
        hasher.update(check.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

/// Determine whether a PR is in a review-ready state.
///
/// A PR is review-ready when it is open, not a draft, and optionally has
/// passing checks.
pub fn is_review_ready(pr: &PullRequestCandidate) -> bool {
    pr.state == "OPEN" && !pr.is_draft
}

/// Determine whether a review should be launched for a PR, or whether it
/// should be suppressed because the PR state has not changed since the last
/// review.
pub fn should_launch_review(pr: &PullRequestCandidate, existing_records: &[ReviewRecord]) -> bool {
    if !is_review_ready(pr) {
        return false;
    }

    let fingerprint = pr_state_fingerprint(pr);

    // Check if there is an existing review with the same fingerprint that is
    // still running or already passed.
    for record in existing_records {
        if record.pr_number == pr.number && record.pr_state_fingerprint == fingerprint {
            match record.status {
                ReviewStatus::Queued | ReviewStatus::Running | ReviewStatus::Passed => {
                    return false;
                }
                ReviewStatus::ChangesRequested | ReviewStatus::Failed => {
                    // Allow re-review after changes or failure if the PR state
                    // actually changed, but the fingerprint match means it hasn't.
                    return false;
                }
            }
        }
    }

    true
}

/// Identify the canonical PR for a given issue from a set of review records.
///
/// The canonical PR is the most recently launched non-superseded PR. When
/// multiple PRs exist for the same issue, only the latest open one is canonical.
pub fn resolve_canonical_pr(
    issue_identifier: &str,
    review_records: &[ReviewRecord],
    open_pr_numbers: &[u64],
) -> Option<u64> {
    let mut candidates: Vec<&ReviewRecord> = review_records
        .iter()
        .filter(|r| r.issue_identifier == issue_identifier)
        .collect();

    // Prefer records that are still open.
    candidates.retain(|r| open_pr_numbers.contains(&r.pr_number));

    // Among open candidates, prefer the highest PR number (most recent).
    candidates.sort_by(|a, b| b.pr_number.cmp(&a.pr_number));

    candidates.first().map(|r| r.pr_number)
}

/// Build a new review record from a PR candidate.
pub fn create_review_record(
    pr: &PullRequestCandidate,
    issue_identifier: &str,
    is_canonical: bool,
) -> ReviewRecord {
    ReviewRecord {
        pr_number: pr.number,
        issue_identifier: issue_identifier.to_string(),
        head_ref: pr.head_ref.clone(),
        status: ReviewStatus::Queued,
        launched_at: None,
        completed_at: None,
        pr_state_fingerprint: pr_state_fingerprint(pr),
        is_canonical,
        merge_eligible: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_pr() -> PullRequestCandidate {
        PullRequestCandidate {
            number: 42,
            title: "Feature work".to_string(),
            head_ref: "feature/met-100".to_string(),
            base_ref: "main".to_string(),
            state: "OPEN".to_string(),
            is_draft: false,
            updated_at: "2026-03-21T10:00:00Z".to_string(),
            issue_identifier: Some("MET-100".to_string()),
            review_decision: None,
            check_status: None,
        }
    }

    #[test]
    fn review_ready_when_open_and_not_draft() {
        let pr = base_pr();
        assert!(is_review_ready(&pr));
    }

    #[test]
    fn not_review_ready_when_draft() {
        let mut pr = base_pr();
        pr.is_draft = true;
        assert!(!is_review_ready(&pr));
    }

    #[test]
    fn not_review_ready_when_closed() {
        let mut pr = base_pr();
        pr.state = "CLOSED".to_string();
        assert!(!is_review_ready(&pr));
    }

    #[test]
    fn launch_review_when_no_existing_records() {
        let pr = base_pr();
        assert!(should_launch_review(&pr, &[]));
    }

    #[test]
    fn suppress_duplicate_review_same_fingerprint() {
        let pr = base_pr();
        let fp = pr_state_fingerprint(&pr);
        let record = ReviewRecord {
            pr_number: 42,
            issue_identifier: "MET-100".to_string(),
            head_ref: "feature/met-100".to_string(),
            status: ReviewStatus::Running,
            launched_at: Some("2026-03-21T10:00:00Z".to_string()),
            completed_at: None,
            pr_state_fingerprint: fp,
            is_canonical: true,
            merge_eligible: false,
        };
        assert!(!should_launch_review(&pr, &[record]));
    }

    #[test]
    fn launch_review_when_fingerprint_changed() {
        let pr = base_pr();
        let record = ReviewRecord {
            pr_number: 42,
            issue_identifier: "MET-100".to_string(),
            head_ref: "feature/met-100".to_string(),
            status: ReviewStatus::Passed,
            launched_at: Some("2026-03-21T09:00:00Z".to_string()),
            completed_at: Some("2026-03-21T09:30:00Z".to_string()),
            pr_state_fingerprint: "old-fingerprint".to_string(),
            is_canonical: true,
            merge_eligible: true,
        };
        assert!(should_launch_review(&pr, &[record]));
    }

    #[test]
    fn canonical_pr_is_highest_open_number() {
        let records = vec![
            ReviewRecord {
                pr_number: 10,
                issue_identifier: "MET-100".to_string(),
                head_ref: "old-branch".to_string(),
                status: ReviewStatus::ChangesRequested,
                launched_at: None,
                completed_at: None,
                pr_state_fingerprint: "fp1".to_string(),
                is_canonical: false,
                merge_eligible: false,
            },
            ReviewRecord {
                pr_number: 20,
                issue_identifier: "MET-100".to_string(),
                head_ref: "new-branch".to_string(),
                status: ReviewStatus::Passed,
                launched_at: None,
                completed_at: None,
                pr_state_fingerprint: "fp2".to_string(),
                is_canonical: true,
                merge_eligible: true,
            },
        ];
        let open = vec![20u64];
        assert_eq!(resolve_canonical_pr("MET-100", &records, &open), Some(20));
    }

    #[test]
    fn no_canonical_when_all_closed() {
        let records = vec![ReviewRecord {
            pr_number: 10,
            issue_identifier: "MET-100".to_string(),
            head_ref: "branch".to_string(),
            status: ReviewStatus::Passed,
            launched_at: None,
            completed_at: None,
            pr_state_fingerprint: "fp".to_string(),
            is_canonical: true,
            merge_eligible: true,
        }];
        let open: Vec<u64> = vec![];
        assert_eq!(resolve_canonical_pr("MET-100", &records, &open), None);
    }

    #[test]
    fn fingerprint_changes_with_pr_update() {
        let pr1 = base_pr();
        let mut pr2 = base_pr();
        pr2.updated_at = "2026-03-21T11:00:00Z".to_string();
        assert_ne!(pr_state_fingerprint(&pr1), pr_state_fingerprint(&pr2));
    }
}

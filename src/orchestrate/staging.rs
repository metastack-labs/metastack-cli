use anyhow::{Result, bail};
use chrono::Utc;

use crate::orchestrate::state::{
    ReviewRecord, ReviewStatus, StagingMergeRecord, StagingMergeResult,
};

/// Default staging branch name prefix.
pub const STAGING_BRANCH_PREFIX: &str = "staging/orchestrate";

/// Compute the default staging branch name from the session id.
pub fn default_staging_branch_name(session_id: &str) -> String {
    format!("{STAGING_BRANCH_PREFIX}-{session_id}")
}

/// Validate a staging branch name.
///
/// Returns an error if the name is empty or contains characters that are
/// invalid in git branch names.
pub fn validate_branch_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("staging branch name must not be empty");
    }
    if name.contains("..") {
        bail!("staging branch name must not contain '..'");
    }
    if name.contains(' ') || name.contains('~') || name.contains('^') || name.contains(':') {
        bail!("staging branch name contains invalid characters");
    }
    if name.starts_with('-') || name.starts_with('/') {
        bail!("staging branch name must not start with '-' or '/'");
    }
    if name.ends_with('/') || name.ends_with('.') || name.ends_with(".lock") {
        bail!("staging branch name must not end with '/', '.', or '.lock'");
    }
    Ok(())
}

/// Select merge-eligible PRs from review records.
///
/// A PR is merge-eligible when:
/// - Its review status is `Passed`
/// - It is marked as canonical
/// - It is flagged as merge-eligible
pub fn select_merge_eligible(review_records: &[ReviewRecord]) -> Vec<&ReviewRecord> {
    review_records
        .iter()
        .filter(|r| r.status == ReviewStatus::Passed && r.is_canonical && r.merge_eligible)
        .collect()
}

/// Create a merge record for a successful staging merge.
pub fn merged_record(
    pr_number: u64,
    issue_identifier: &str,
    commit_sha: &str,
) -> StagingMergeRecord {
    StagingMergeRecord {
        pr_number,
        issue_identifier: issue_identifier.to_string(),
        result: StagingMergeResult::Merged,
        attempted_at: Utc::now().to_rfc3339(),
        commit_sha: Some(commit_sha.to_string()),
        error: None,
    }
}

/// Create a merge record for a conflict.
pub fn conflict_record(pr_number: u64, issue_identifier: &str, error: &str) -> StagingMergeRecord {
    StagingMergeRecord {
        pr_number,
        issue_identifier: issue_identifier.to_string(),
        result: StagingMergeResult::Conflict,
        attempted_at: Utc::now().to_rfc3339(),
        commit_sha: None,
        error: Some(error.to_string()),
    }
}

/// Create a merge record for a validation failure.
pub fn validation_failed_record(
    pr_number: u64,
    issue_identifier: &str,
    error: &str,
) -> StagingMergeRecord {
    StagingMergeRecord {
        pr_number,
        issue_identifier: issue_identifier.to_string(),
        result: StagingMergeResult::ValidationFailed,
        attempted_at: Utc::now().to_rfc3339(),
        commit_sha: None,
        error: Some(error.to_string()),
    }
}

/// Create a merge record for a skipped PR.
pub fn skipped_record(pr_number: u64, issue_identifier: &str, reason: &str) -> StagingMergeRecord {
    StagingMergeRecord {
        pr_number,
        issue_identifier: issue_identifier.to_string(),
        result: StagingMergeResult::Skipped,
        attempted_at: Utc::now().to_rfc3339(),
        commit_sha: None,
        error: Some(reason.to_string()),
    }
}

/// Summary of staging integration results from a single cycle.
#[derive(Debug, Clone)]
pub struct StagingCycleSummary {
    pub merged_count: u32,
    pub conflict_count: u32,
    pub validation_failed_count: u32,
    pub skipped_count: u32,
}

impl StagingCycleSummary {
    /// Compute a summary from a list of merge records.
    pub fn from_records(records: &[StagingMergeRecord]) -> Self {
        let mut summary = Self {
            merged_count: 0,
            conflict_count: 0,
            validation_failed_count: 0,
            skipped_count: 0,
        };
        for record in records {
            match record.result {
                StagingMergeResult::Merged => summary.merged_count += 1,
                StagingMergeResult::Conflict => summary.conflict_count += 1,
                StagingMergeResult::ValidationFailed => summary.validation_failed_count += 1,
                StagingMergeResult::Skipped => summary.skipped_count += 1,
            }
        }
        summary
    }

    /// Whether any merges were attempted (regardless of result).
    pub fn any_attempted(&self) -> bool {
        self.merged_count + self.conflict_count + self.validation_failed_count + self.skipped_count
            > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_branch_name_includes_session_id() {
        let name = default_staging_branch_name("abc123");
        assert_eq!(name, "staging/orchestrate-abc123");
    }

    #[test]
    fn valid_branch_names() {
        assert!(validate_branch_name("staging/orchestrate-001").is_ok());
        assert!(validate_branch_name("feature/test").is_ok());
        assert!(validate_branch_name("my-branch").is_ok());
    }

    #[test]
    fn invalid_branch_names() {
        assert!(validate_branch_name("").is_err());
        assert!(validate_branch_name("bad..name").is_err());
        assert!(validate_branch_name("bad name").is_err());
        assert!(validate_branch_name("-leading-dash").is_err());
        assert!(validate_branch_name("trailing/").is_err());
        assert!(validate_branch_name("trailing.lock").is_err());
    }

    #[test]
    fn select_merge_eligible_filters_correctly() {
        let records = vec![
            ReviewRecord {
                pr_number: 1,
                issue_identifier: "MET-1".to_string(),
                head_ref: "a".to_string(),
                status: ReviewStatus::Passed,
                launched_at: None,
                completed_at: None,
                pr_state_fingerprint: "fp".to_string(),
                is_canonical: true,
                merge_eligible: true,
            },
            ReviewRecord {
                pr_number: 2,
                issue_identifier: "MET-2".to_string(),
                head_ref: "b".to_string(),
                status: ReviewStatus::ChangesRequested,
                launched_at: None,
                completed_at: None,
                pr_state_fingerprint: "fp".to_string(),
                is_canonical: true,
                merge_eligible: false,
            },
            ReviewRecord {
                pr_number: 3,
                issue_identifier: "MET-3".to_string(),
                head_ref: "c".to_string(),
                status: ReviewStatus::Passed,
                launched_at: None,
                completed_at: None,
                pr_state_fingerprint: "fp".to_string(),
                is_canonical: false,
                merge_eligible: true,
            },
        ];
        let eligible = select_merge_eligible(&records);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].pr_number, 1);
    }

    #[test]
    fn staging_cycle_summary_counts() {
        let records = vec![
            merged_record(1, "MET-1", "sha1"),
            merged_record(2, "MET-2", "sha2"),
            conflict_record(3, "MET-3", "conflict"),
            validation_failed_record(4, "MET-4", "test failed"),
            skipped_record(5, "MET-5", "superseded"),
        ];
        let summary = StagingCycleSummary::from_records(&records);
        assert_eq!(summary.merged_count, 2);
        assert_eq!(summary.conflict_count, 1);
        assert_eq!(summary.validation_failed_count, 1);
        assert_eq!(summary.skipped_count, 1);
        assert!(summary.any_attempted());
    }

    #[test]
    fn empty_summary() {
        let summary = StagingCycleSummary::from_records(&[]);
        assert!(!summary.any_attempted());
    }
}

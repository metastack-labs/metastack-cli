use serde::{Deserialize, Serialize};

pub(super) const COMPLETED_SESSION_TTL_SECONDS: u64 = 24 * 60 * 60;

/// Phase of a PR review session in the review lifecycle.
///
/// The original five variants (`Claimed`, `ReviewStarted`, `Running`,
/// `Completed`, `Blocked`) are used by the listener-daemon polling path.
/// The extended variants model the interactive and scripted fix-agent
/// remediation flow with explicit per-PR state transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewPhase {
    // -- Original listener-daemon phases --
    Claimed,
    ReviewStarted,
    Running,
    Completed,
    Blocked,
    // -- Extended interactive / fix-agent phases --
    /// User selected this PR for review but has not started execution yet.
    Selected,
    /// Review agent is running for this PR.
    ReviewInProgress,
    /// Review agent finished; awaiting user remediation decision.
    ReviewComplete,
    /// User approved remediation; fix agent queued but not yet started.
    FixAgentPending,
    /// Fix agent is actively running.
    FixAgentInProgress,
    /// Fix agent succeeded and the remediation PR was created.
    FixAgentComplete,
    /// Fix agent execution failed with an actionable error.
    FixAgentFailed,
    /// User explicitly skipped remediation for this PR.
    Skipped,
}

impl ReviewPhase {
    /// Human-readable label for display in dashboards and summaries.
    pub fn display_label(self) -> &'static str {
        match self {
            Self::Claimed => "Claimed",
            Self::ReviewStarted => "Review Started",
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Blocked => "Blocked",
            Self::Selected => "Selected",
            Self::ReviewInProgress => "Review In Progress",
            Self::ReviewComplete => "Review Complete",
            Self::FixAgentPending => "Fix Agent Pending",
            Self::FixAgentInProgress => "Fix Agent Running",
            Self::FixAgentComplete => "Fix Agent Complete",
            Self::FixAgentFailed => "Fix Agent Failed",
            Self::Skipped => "Skipped",
        }
    }

    /// Returns `true` for phases considered "completed" for pruning and TTL
    /// purposes.
    pub fn is_completed(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::FixAgentComplete | Self::Skipped
        )
    }

    /// Returns `true` for terminal phases that do not block new pickup of the
    /// same PR number.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Blocked
                | Self::FixAgentComplete
                | Self::FixAgentFailed
                | Self::Skipped
        )
    }

    /// Returns `true` when a fix-agent subprocess is actively pending or
    /// running.
    pub fn is_fix_agent_active(self) -> bool {
        matches!(self, Self::FixAgentPending | Self::FixAgentInProgress)
    }

    /// Validate whether a transition from `self` to `target` is permitted.
    ///
    /// Returns `true` when the transition is valid. Invalid transitions should
    /// be treated as programming errors and surfaced with actionable context.
    pub fn can_transition_to(self, target: Self) -> bool {
        if self == target {
            return true;
        }
        matches!(
            (self, target),
            // Selection flow
            (Self::Selected, Self::ReviewInProgress)
                | (Self::Selected, Self::Skipped)
                // Legacy listener-daemon flow
                | (Self::Claimed, Self::ReviewStarted)
                | (Self::Claimed, Self::ReviewInProgress)
                | (Self::ReviewStarted, Self::Running)
                | (Self::ReviewStarted, Self::ReviewInProgress)
                | (Self::ReviewStarted, Self::Blocked)
                // Review agent execution
                | (Self::ReviewInProgress, Self::ReviewComplete)
                | (Self::ReviewInProgress, Self::Completed)
                | (Self::ReviewInProgress, Self::Blocked)
                // Legacy agent execution
                | (Self::Running, Self::Completed)
                | (Self::Running, Self::ReviewComplete)
                | (Self::Running, Self::Blocked)
                // Remediation decision from review complete
                | (Self::ReviewComplete, Self::FixAgentPending)
                | (Self::ReviewComplete, Self::Skipped)
                | (Self::ReviewComplete, Self::Completed)
                // Fix-agent lifecycle
                | (Self::FixAgentPending, Self::FixAgentInProgress)
                | (Self::FixAgentPending, Self::Blocked)
                | (Self::FixAgentInProgress, Self::FixAgentComplete)
                | (Self::FixAgentInProgress, Self::FixAgentFailed)
                | (Self::FixAgentInProgress, Self::Blocked)
                // Recovery / retry
                | (Self::FixAgentFailed, Self::FixAgentPending)
                | (Self::Blocked, Self::Claimed)
                | (Self::Blocked, Self::ReviewInProgress)
        )
    }
}

/// A review session for a single PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewSession {
    pub pr_number: u64,
    pub pr_title: String,
    #[serde(default)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub pr_author: Option<String>,
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default)]
    pub base_branch: Option<String>,
    #[serde(default)]
    pub linear_identifier: Option<String>,
    pub phase: ReviewPhase,
    pub summary: String,
    pub updated_at_epoch_seconds: u64,
    #[serde(default)]
    pub review_output: Option<String>,
    #[serde(default)]
    pub remediation_required: Option<bool>,
    #[serde(default)]
    pub remediation_pr_number: Option<u64>,
    #[serde(default)]
    pub remediation_pr_url: Option<String>,
}

impl ReviewSession {
    pub(super) fn pr_matches(&self, pr_number: u64) -> bool {
        self.pr_number == pr_number
    }

    pub(super) fn stage_label(&self) -> &'static str {
        self.phase.display_label()
    }

    pub(super) fn age_label(&self, now_epoch_seconds: u64) -> String {
        super::format_duration(now_epoch_seconds.saturating_sub(self.updated_at_epoch_seconds))
    }

    pub(super) fn remediation_label(&self) -> &'static str {
        match (self.remediation_required, self.phase) {
            (_, ReviewPhase::Skipped) => "skipped",
            (_, ReviewPhase::FixAgentComplete) => "done",
            (_, ReviewPhase::FixAgentFailed) => "failed",
            (_, phase) if phase.is_fix_agent_active() => "in progress",
            (Some(true), _) => "yes",
            (Some(false), _) => "no",
            (None, _) => "-",
        }
    }

    /// Returns `true` when this session is awaiting a user remediation
    /// decision (review finished with remediation required, no fix-agent
    /// action taken yet).
    pub(super) fn needs_remediation_decision(&self) -> bool {
        self.phase == ReviewPhase::ReviewComplete
            && self.remediation_required == Some(true)
            && self.remediation_pr_url.is_none()
    }
}

/// Container for all review sessions in a listener run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ReviewState {
    version: u8,
    pub(super) sessions: Vec<ReviewSession>,
}

impl Default for ReviewState {
    fn default() -> Self {
        Self {
            version: 1,
            sessions: Vec::new(),
        }
    }
}

impl ReviewState {
    #[cfg(test)]
    pub(super) fn from_sessions(sessions: Vec<ReviewSession>) -> Self {
        Self {
            version: 1,
            sessions,
        }
    }

    pub(super) fn blocks_pickup(&self, pr_number: u64) -> bool {
        self.sessions
            .iter()
            .any(|session| session.pr_matches(pr_number) && !session.phase.is_terminal())
    }

    pub(super) fn upsert(&mut self, session: ReviewSession) {
        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|existing| existing.pr_matches(session.pr_number))
        {
            *existing = session;
        } else {
            self.sessions.push(session);
        }
    }

    pub(super) fn prune_completed_sessions_older_than(
        &mut self,
        now_epoch_seconds: u64,
        ttl_seconds: u64,
    ) -> Vec<ReviewSession> {
        let mut removed = Vec::new();
        self.sessions.retain(|session| {
            if session.phase.is_completed()
                && now_epoch_seconds.saturating_sub(session.updated_at_epoch_seconds) > ttl_seconds
            {
                removed.push(session.clone());
                false
            } else {
                true
            }
        });
        removed
    }

    /// Look up a session by PR number.
    pub(super) fn find_session(&self, pr_number: u64) -> Option<&ReviewSession> {
        self.sessions
            .iter()
            .find(|session| session.pr_matches(pr_number))
    }

    pub(super) fn sorted_sessions(&self) -> Vec<ReviewSession> {
        let mut sessions = self.sessions.clone();
        sessions.sort_by(|left, right| {
            right
                .updated_at_epoch_seconds
                .cmp(&left.updated_at_epoch_seconds)
                .then_with(|| left.pr_number.cmp(&right.pr_number))
        });
        sessions
    }

    pub(super) fn remove_session(&mut self, pr_number: u64) -> bool {
        let original_len = self.sessions.len();
        self.sessions.retain(|session| !session.pr_matches(pr_number));
        self.sessions.len() != original_len
    }
}

#[cfg(test)]
mod tests {
    use super::{ReviewPhase, ReviewSession, ReviewState};

    fn session(pr_number: u64) -> ReviewSession {
        ReviewSession {
            pr_number,
            pr_title: format!("Test PR #{pr_number}"),
            pr_url: Some(format!("https://github.com/test/repo/pull/{pr_number}")),
            pr_author: Some("test-user".to_string()),
            head_branch: Some("feature-branch".to_string()),
            base_branch: Some("main".to_string()),
            linear_identifier: Some("MET-1".to_string()),
            phase: ReviewPhase::Running,
            summary: "Running".to_string(),
            updated_at_epoch_seconds: 1,
            review_output: None,
            remediation_required: None,
            remediation_pr_number: None,
            remediation_pr_url: None,
        }
    }

    #[test]
    fn blocks_pickup_for_active_sessions() {
        let state = ReviewState::from_sessions(vec![session(42)]);
        assert!(state.blocks_pickup(42));
        assert!(!state.blocks_pickup(99));
    }

    #[test]
    fn completed_sessions_do_not_block_pickup() {
        let mut s = session(42);
        s.phase = ReviewPhase::Completed;
        let state = ReviewState::from_sessions(vec![s]);
        assert!(!state.blocks_pickup(42));
    }

    #[test]
    fn upsert_replaces_existing_session() {
        let mut state = ReviewState::from_sessions(vec![session(42)]);
        let mut updated = session(42);
        updated.summary = "Updated".to_string();
        state.upsert(updated);
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].summary, "Updated");
    }

    #[test]
    fn prune_removes_old_completed_sessions() {
        let mut s = session(42);
        s.phase = ReviewPhase::Completed;
        s.updated_at_epoch_seconds = 100;
        let mut state = ReviewState::from_sessions(vec![s]);
        let pruned = state.prune_completed_sessions_older_than(100_000, 1000);
        assert_eq!(pruned.len(), 1);
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn remediation_label_shows_correct_values() {
        let mut s = session(1);
        assert_eq!(s.remediation_label(), "-");
        s.remediation_required = Some(false);
        assert_eq!(s.remediation_label(), "no");
        s.remediation_required = Some(true);
        assert_eq!(s.remediation_label(), "yes");
    }

    #[test]
    fn all_active_phases_block_pickup() {
        for phase in [
            ReviewPhase::Claimed,
            ReviewPhase::ReviewStarted,
            ReviewPhase::Running,
            ReviewPhase::Selected,
            ReviewPhase::ReviewInProgress,
            ReviewPhase::ReviewComplete,
            ReviewPhase::FixAgentPending,
            ReviewPhase::FixAgentInProgress,
        ] {
            let mut s = session(42);
            s.phase = phase;
            let state = ReviewState::from_sessions(vec![s]);
            assert!(state.blocks_pickup(42), "{:?} should block pickup", phase);
        }
    }

    #[test]
    fn terminal_phases_do_not_block_pickup() {
        for phase in [
            ReviewPhase::Completed,
            ReviewPhase::Blocked,
            ReviewPhase::FixAgentComplete,
            ReviewPhase::FixAgentFailed,
            ReviewPhase::Skipped,
        ] {
            let mut s = session(42);
            s.phase = phase;
            let state = ReviewState::from_sessions(vec![s]);
            assert!(
                !state.blocks_pickup(42),
                "{:?} should not block pickup",
                phase
            );
        }
    }

    // -- State transition validation tests --

    #[test]
    fn valid_review_to_fix_agent_transitions() {
        let happy_path = [
            (ReviewPhase::Selected, ReviewPhase::ReviewInProgress),
            (ReviewPhase::ReviewInProgress, ReviewPhase::ReviewComplete),
            (ReviewPhase::ReviewComplete, ReviewPhase::FixAgentPending),
            (
                ReviewPhase::FixAgentPending,
                ReviewPhase::FixAgentInProgress,
            ),
            (
                ReviewPhase::FixAgentInProgress,
                ReviewPhase::FixAgentComplete,
            ),
        ];
        for (from, to) in happy_path {
            assert!(
                from.can_transition_to(to),
                "{from:?} -> {to:?} should be valid"
            );
        }
    }

    #[test]
    fn valid_skip_transitions() {
        assert!(ReviewPhase::Selected.can_transition_to(ReviewPhase::Skipped));
        assert!(ReviewPhase::ReviewComplete.can_transition_to(ReviewPhase::Skipped));
    }

    #[test]
    fn valid_failure_transitions() {
        assert!(ReviewPhase::FixAgentInProgress.can_transition_to(ReviewPhase::FixAgentFailed));
        assert!(ReviewPhase::FixAgentInProgress.can_transition_to(ReviewPhase::Blocked));
        assert!(ReviewPhase::ReviewInProgress.can_transition_to(ReviewPhase::Blocked));
    }

    #[test]
    fn valid_recovery_transitions() {
        assert!(ReviewPhase::FixAgentFailed.can_transition_to(ReviewPhase::FixAgentPending));
        assert!(ReviewPhase::Blocked.can_transition_to(ReviewPhase::Claimed));
    }

    #[test]
    fn idempotent_transitions_allowed() {
        for phase in [
            ReviewPhase::Selected,
            ReviewPhase::ReviewInProgress,
            ReviewPhase::ReviewComplete,
            ReviewPhase::FixAgentPending,
            ReviewPhase::FixAgentInProgress,
            ReviewPhase::FixAgentComplete,
            ReviewPhase::Skipped,
            ReviewPhase::Completed,
            ReviewPhase::Blocked,
        ] {
            assert!(
                phase.can_transition_to(phase),
                "{phase:?} -> {phase:?} (self) should be valid"
            );
        }
    }

    #[test]
    fn invalid_backward_transitions_rejected() {
        let invalid = [
            (ReviewPhase::FixAgentComplete, ReviewPhase::ReviewInProgress),
            (ReviewPhase::Skipped, ReviewPhase::FixAgentPending),
            (ReviewPhase::Completed, ReviewPhase::ReviewInProgress),
            (ReviewPhase::FixAgentComplete, ReviewPhase::FixAgentPending),
            (ReviewPhase::Skipped, ReviewPhase::ReviewComplete),
            (ReviewPhase::ReviewComplete, ReviewPhase::ReviewInProgress),
        ];
        for (from, to) in invalid {
            assert!(
                !from.can_transition_to(to),
                "{from:?} -> {to:?} should be invalid"
            );
        }
    }

    #[test]
    fn legacy_listener_transitions_valid() {
        let legacy_path = [
            (ReviewPhase::Claimed, ReviewPhase::ReviewStarted),
            (ReviewPhase::ReviewStarted, ReviewPhase::Running),
            (ReviewPhase::Running, ReviewPhase::Completed),
            (ReviewPhase::Running, ReviewPhase::Blocked),
        ];
        for (from, to) in legacy_path {
            assert!(
                from.can_transition_to(to),
                "{from:?} -> {to:?} (legacy) should be valid"
            );
        }
    }

    #[test]
    fn completed_includes_fix_agent_and_skipped() {
        assert!(ReviewPhase::Completed.is_completed());
        assert!(ReviewPhase::FixAgentComplete.is_completed());
        assert!(ReviewPhase::Skipped.is_completed());
        assert!(!ReviewPhase::FixAgentFailed.is_completed());
        assert!(!ReviewPhase::Blocked.is_completed());
        assert!(!ReviewPhase::ReviewComplete.is_completed());
    }

    #[test]
    fn prune_removes_fix_agent_complete_and_skipped_sessions() {
        let mut s1 = session(1);
        s1.phase = ReviewPhase::FixAgentComplete;
        s1.updated_at_epoch_seconds = 100;
        let mut s2 = session(2);
        s2.phase = ReviewPhase::Skipped;
        s2.updated_at_epoch_seconds = 100;
        let mut s3 = session(3);
        s3.phase = ReviewPhase::FixAgentFailed;
        s3.updated_at_epoch_seconds = 100;

        let mut state = ReviewState::from_sessions(vec![s1, s2, s3]);
        let pruned = state.prune_completed_sessions_older_than(100_000, 1000);
        assert_eq!(pruned.len(), 2, "FixAgentComplete and Skipped should prune");
        assert_eq!(
            state.sessions.len(),
            1,
            "FixAgentFailed should remain (not completed)"
        );
        assert_eq!(state.sessions[0].pr_number, 3);
    }

    #[test]
    fn needs_remediation_decision_checks() {
        let mut s = session(1);
        s.phase = ReviewPhase::ReviewComplete;
        s.remediation_required = Some(true);
        assert!(s.needs_remediation_decision());

        s.remediation_required = Some(false);
        assert!(!s.needs_remediation_decision());

        s.remediation_required = Some(true);
        s.remediation_pr_url = Some("https://example.test/pull/99".to_string());
        assert!(!s.needs_remediation_decision());

        s.remediation_pr_url = None;
        s.phase = ReviewPhase::Completed;
        assert!(!s.needs_remediation_decision());
    }

    #[test]
    fn find_session_returns_matching_pr() {
        let state = ReviewState::from_sessions(vec![session(42), session(99)]);
        assert_eq!(state.find_session(42).unwrap().pr_number, 42);
        assert_eq!(state.find_session(99).unwrap().pr_number, 99);
        assert!(state.find_session(1).is_none());
    }
}

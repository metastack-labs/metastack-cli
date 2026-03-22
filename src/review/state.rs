use serde::{Deserialize, Serialize};

pub(super) const COMPLETED_SESSION_TTL_SECONDS: u64 = 24 * 60 * 60;

/// Phase of a PR review session in the listener lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewPhase {
    Claimed,
    ReviewStarted,
    Running,
    Completed,
    Blocked,
}

impl ReviewPhase {
    pub fn display_label(self) -> &'static str {
        match self {
            Self::Claimed => "Claimed",
            Self::ReviewStarted => "Review Started",
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Blocked => "Blocked",
        }
    }

    pub fn is_completed(self) -> bool {
        matches!(self, Self::Completed)
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
        match self.remediation_required {
            Some(true) => "yes",
            Some(false) => "no",
            None => "-",
        }
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
        self.sessions.iter().any(|session| {
            session.pr_matches(pr_number)
                && matches!(
                    session.phase,
                    ReviewPhase::Claimed | ReviewPhase::ReviewStarted | ReviewPhase::Running
                )
        })
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
        ] {
            let mut s = session(42);
            s.phase = phase;
            let state = ReviewState::from_sessions(vec![s]);
            assert!(state.blocks_pickup(42), "{:?} should block pickup", phase);
        }
    }

    #[test]
    fn terminal_phases_do_not_block_pickup() {
        for phase in [ReviewPhase::Completed, ReviewPhase::Blocked] {
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
}

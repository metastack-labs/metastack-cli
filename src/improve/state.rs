use serde::{Deserialize, Serialize};

/// Phase of an improve session in the improve lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImprovePhase {
    /// Session created, agent execution not yet started.
    Queued,
    /// Agent execution is actively running in the workspace.
    Running,
    /// Agent completed; publishing the stacked PR.
    Publishing,
    /// Stacked PR created or updated successfully.
    Completed,
    /// Execution or publication failed with an actionable error.
    Failed,
}

impl ImprovePhase {
    /// Human-readable label for display in dashboards and summaries.
    pub fn display_label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Publishing => "Publishing",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
        }
    }

    /// Returns `true` for terminal phases.
    #[allow(dead_code)]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }
}

/// Persisted metadata about the source PR that triggered the improve session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImproveSourcePr {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub head_branch: String,
    pub base_branch: String,
}

/// A persisted improve session record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImproveSession {
    pub session_id: String,
    pub source_pr: ImproveSourcePr,
    pub instructions: String,
    pub phase: ImprovePhase,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub improve_branch: Option<String>,
    #[serde(default)]
    pub stacked_pr_number: Option<u64>,
    #[serde(default)]
    pub stacked_pr_url: Option<String>,
    #[serde(default)]
    pub error_summary: Option<String>,
    pub created_at_epoch_seconds: u64,
    pub updated_at_epoch_seconds: u64,
}

impl ImproveSession {
    /// Human-readable age label relative to a reference epoch.
    pub fn age_label(&self, now_epoch_seconds: u64) -> String {
        format_duration(now_epoch_seconds.saturating_sub(self.updated_at_epoch_seconds))
    }
}

/// Container for all improve sessions in a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImproveState {
    version: u8,
    pub sessions: Vec<ImproveSession>,
}

impl Default for ImproveState {
    fn default() -> Self {
        Self {
            version: 1,
            sessions: Vec::new(),
        }
    }
}

impl ImproveState {
    #[cfg(test)]
    pub fn from_sessions(sessions: Vec<ImproveSession>) -> Self {
        Self {
            version: 1,
            sessions,
        }
    }

    /// Insert or replace a session by session ID.
    #[allow(dead_code)]
    pub fn upsert(&mut self, session: ImproveSession) {
        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|s| s.session_id == session.session_id)
        {
            *existing = session;
        } else {
            self.sessions.push(session);
        }
    }

    /// Look up a session by ID.
    #[allow(dead_code)]
    pub fn find_session(&self, session_id: &str) -> Option<&ImproveSession> {
        self.sessions.iter().find(|s| s.session_id == session_id)
    }

    /// Return sessions sorted by most recently updated first.
    pub fn sorted_sessions(&self) -> Vec<ImproveSession> {
        let mut sessions = self.sessions.clone();
        sessions.sort_by(|left, right| {
            right
                .updated_at_epoch_seconds
                .cmp(&left.updated_at_epoch_seconds)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        sessions
    }

    /// Active (non-terminal) sessions.
    #[allow(dead_code)]
    pub fn active_sessions(&self) -> Vec<ImproveSession> {
        self.sorted_sessions()
            .into_iter()
            .filter(|s| !s.phase.is_terminal())
            .collect()
    }

    /// Terminal (completed/failed) sessions.
    #[allow(dead_code)]
    pub fn completed_sessions(&self) -> Vec<ImproveSession> {
        self.sorted_sessions()
            .into_iter()
            .filter(|s| s.phase.is_terminal())
            .collect()
    }
}

fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}

/// Build the improve branch name from a source PR branch.
#[allow(dead_code)]
pub fn improve_branch_name(source_branch: &str) -> String {
    format!("improve/{source_branch}")
}

/// Build the stacked PR title from the source PR.
#[allow(dead_code)]
pub fn stacked_pr_title(source_pr_number: u64, instructions: &str) -> String {
    let summary = if instructions.len() > 60 {
        format!("{}...", &instructions[..60])
    } else {
        instructions.to_string()
    };
    let single_line = summary.replace('\n', " ");
    format!("improve(#{source_pr_number}): {single_line}")
}

/// Build the stacked PR body linking back to the source PR.
#[allow(dead_code)]
pub fn stacked_pr_body(source_pr: &ImproveSourcePr, instructions: &str) -> String {
    format!(
        "## Improvement\n\n\
         Stacked on #{} (`{}`)\n\n\
         **Source PR:** {}\n\n\
         ### Instructions\n\n\
         {}\n",
        source_pr.number, source_pr.head_branch, source_pr.url, instructions,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(id: &str, phase: ImprovePhase) -> ImproveSession {
        ImproveSession {
            session_id: id.to_string(),
            source_pr: ImproveSourcePr {
                number: 42,
                title: "Test PR #42".to_string(),
                url: "https://github.com/test/repo/pull/42".to_string(),
                author: "test-user".to_string(),
                head_branch: "feature-branch".to_string(),
                base_branch: "main".to_string(),
            },
            instructions: "Fix the tests".to_string(),
            phase,
            workspace_path: None,
            improve_branch: None,
            stacked_pr_number: None,
            stacked_pr_url: None,
            error_summary: None,
            created_at_epoch_seconds: 1000,
            updated_at_epoch_seconds: 1000,
        }
    }

    #[test]
    fn session_serialization_round_trip() {
        let session = test_session("sess-1", ImprovePhase::Queued);
        let json = serde_json::to_string_pretty(&session).expect("serialize");
        let parsed: ImproveSession = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.session_id, "sess-1");
        assert_eq!(parsed.phase, ImprovePhase::Queued);
        assert_eq!(parsed.source_pr.number, 42);
        assert_eq!(parsed.instructions, "Fix the tests");
    }

    #[test]
    fn state_serialization_round_trip() {
        let state = ImproveState::from_sessions(vec![
            test_session("sess-1", ImprovePhase::Queued),
            test_session("sess-2", ImprovePhase::Completed),
        ]);
        let json = serde_json::to_string_pretty(&state).expect("serialize");
        let parsed: ImproveState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed.sessions.len(), 2);
    }

    #[test]
    fn upsert_replaces_existing_session() {
        let mut state =
            ImproveState::from_sessions(vec![test_session("sess-1", ImprovePhase::Queued)]);
        let mut updated = test_session("sess-1", ImprovePhase::Running);
        updated.instructions = "Updated".to_string();
        state.upsert(updated);
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].phase, ImprovePhase::Running);
    }

    #[test]
    fn upsert_appends_new_session() {
        let mut state =
            ImproveState::from_sessions(vec![test_session("sess-1", ImprovePhase::Queued)]);
        state.upsert(test_session("sess-2", ImprovePhase::Running));
        assert_eq!(state.sessions.len(), 2);
    }

    #[test]
    fn active_and_completed_split() {
        let state = ImproveState::from_sessions(vec![
            test_session("a", ImprovePhase::Running),
            test_session("b", ImprovePhase::Completed),
            test_session("c", ImprovePhase::Queued),
            test_session("d", ImprovePhase::Failed),
        ]);
        assert_eq!(state.active_sessions().len(), 2);
        assert_eq!(state.completed_sessions().len(), 2);
    }

    #[test]
    fn terminal_phases() {
        assert!(ImprovePhase::Completed.is_terminal());
        assert!(ImprovePhase::Failed.is_terminal());
        assert!(!ImprovePhase::Queued.is_terminal());
        assert!(!ImprovePhase::Running.is_terminal());
        assert!(!ImprovePhase::Publishing.is_terminal());
    }

    #[test]
    fn improve_branch_name_prefixes_source() {
        assert_eq!(
            improve_branch_name("met-42-feature"),
            "improve/met-42-feature"
        );
    }

    #[test]
    fn stacked_pr_title_truncates_long_instructions() {
        let short = stacked_pr_title(42, "Fix tests");
        assert_eq!(short, "improve(#42): Fix tests");

        let long = stacked_pr_title(42, &"x".repeat(100));
        assert!(long.contains("..."));
        assert!(long.len() < 120);
    }

    #[test]
    fn stacked_pr_body_links_to_source() {
        let source = ImproveSourcePr {
            number: 42,
            title: "Test PR".to_string(),
            url: "https://github.com/test/repo/pull/42".to_string(),
            author: "alice".to_string(),
            head_branch: "feature-branch".to_string(),
            base_branch: "main".to_string(),
        };
        let body = stacked_pr_body(&source, "Fix the tests");
        assert!(body.contains("#42"));
        assert!(body.contains("feature-branch"));
        assert!(body.contains("https://github.com/test/repo/pull/42"));
        assert!(body.contains("Fix the tests"));
    }
}

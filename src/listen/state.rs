use serde::{Deserialize, Serialize};

use crate::linear::IssueSummary;

use super::{compact_identifier, format_duration, format_number};

pub(super) const COMPLETED_SESSION_TTL_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingIssue {
    pub identifier: String,
    pub title: String,
    pub project: Option<String>,
    pub team_key: String,
}

impl From<IssueSummary> for PendingIssue {
    fn from(value: IssueSummary) -> Self {
        Self {
            identifier: value.identifier,
            title: value.title,
            project: value.project.map(|project| project.name),
            team_key: value.team.key,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub input: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
}

impl TokenUsage {
    fn total(&self) -> Option<u64> {
        match (self.input, self.output) {
            (None, None) => None,
            (input, output) => Some(input.unwrap_or(0) + output.unwrap_or(0)),
        }
    }

    pub(super) fn display_compact(&self) -> String {
        self.total()
            .map(format_number)
            .unwrap_or_else(|| "n/a".to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    #[serde(default)]
    pub issue_id: Option<String>,
    pub issue_identifier: String,
    pub issue_title: String,
    pub project_name: Option<String>,
    pub team_key: String,
    pub issue_url: String,
    pub phase: SessionPhase,
    pub summary: String,
    pub brief_path: Option<String>,
    #[serde(default)]
    pub backlog_issue_identifier: Option<String>,
    #[serde(default)]
    pub backlog_issue_title: Option<String>,
    #[serde(default)]
    pub backlog_path: Option<String>,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub workpad_comment_id: Option<String>,
    pub updated_at_epoch_seconds: u64,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub turns: Option<u32>,
    #[serde(default)]
    pub tokens: TokenUsage,
    #[serde(default)]
    pub log_path: Option<String>,
}

impl AgentSession {
    pub(super) fn issue_matches(&self, identifier: &str) -> bool {
        self.issue_identifier.eq_ignore_ascii_case(identifier)
    }

    pub(super) fn stage_label(&self) -> &'static str {
        self.phase.display_label()
    }

    pub(super) fn pid_label(&self) -> String {
        self.pid
            .map(|pid| pid.to_string())
            .unwrap_or_else(|| "-".to_string())
    }

    pub(super) fn age_label(&self, now_epoch_seconds: u64) -> String {
        format_duration(now_epoch_seconds.saturating_sub(self.updated_at_epoch_seconds))
    }

    pub(super) fn tokens_label(&self) -> String {
        self.tokens.display_compact()
    }

    pub(super) fn session_label(&self) -> String {
        self.session_id
            .as_deref()
            .map(compact_identifier)
            .or_else(|| self.issue_id.as_deref().map(compact_identifier))
            .unwrap_or_else(|| "-".to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Claimed,
    BriefReady,
    Running,
    Completed,
    Blocked,
}

impl SessionPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::BriefReady => "brief-ready",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Claimed => "Claimed",
            Self::BriefReady => "Brief Ready",
            Self::Running => "Running",
            Self::Completed => "Completed",
            Self::Blocked => "Blocked",
        }
    }

    #[cfg(test)]
    pub fn html_class(self) -> &'static str {
        match self {
            Self::Claimed => "warning",
            Self::BriefReady => "active",
            Self::Running => "active",
            Self::Completed => "success",
            Self::Blocked => "danger",
        }
    }

    pub fn is_completed(self) -> bool {
        matches!(self, Self::Completed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ListenState {
    version: u8,
    pub(super) sessions: Vec<AgentSession>,
}

impl Default for ListenState {
    fn default() -> Self {
        Self {
            version: 1,
            sessions: Vec::new(),
        }
    }
}

impl ListenState {
    #[cfg(test)]
    pub(super) fn from_sessions(sessions: Vec<AgentSession>) -> Self {
        Self {
            version: 1,
            sessions,
        }
    }

    pub(super) fn blocks_pickup(&self, identifier: &str) -> bool {
        self.sessions.iter().any(|session| {
            session.issue_matches(identifier)
                && matches!(
                    session.phase,
                    SessionPhase::Claimed | SessionPhase::BriefReady | SessionPhase::Running
                )
        })
    }

    pub(super) fn upsert(&mut self, session: AgentSession) {
        if let Some(existing) = self
            .sessions
            .iter_mut()
            .find(|existing| existing.issue_matches(&session.issue_identifier))
        {
            *existing = session;
        } else {
            self.sessions.push(session);
        }
    }

    pub(super) fn remove_sessions<F>(&mut self, mut predicate: F) -> Vec<AgentSession>
    where
        F: FnMut(&AgentSession) -> bool,
    {
        let mut removed = Vec::new();
        self.sessions.retain(|session| {
            if predicate(session) {
                removed.push(session.clone());
                false
            } else {
                true
            }
        });
        removed
    }

    pub(super) fn prune_completed_sessions_older_than(
        &mut self,
        now_epoch_seconds: u64,
        ttl_seconds: u64,
    ) -> Vec<AgentSession> {
        self.remove_sessions(|session| {
            session.phase.is_completed()
                && now_epoch_seconds.saturating_sub(session.updated_at_epoch_seconds) > ttl_seconds
        })
    }

    pub(super) fn remove_issue(&mut self, identifier: &str) -> bool {
        let original_len = self.sessions.len();
        self.sessions
            .retain(|session| !session.issue_matches(identifier));
        self.sessions.len() != original_len
    }

    pub(super) fn sorted_sessions(&self) -> Vec<AgentSession> {
        let mut sessions = self.sessions.clone();
        sessions.sort_by(|left, right| {
            right
                .updated_at_epoch_seconds
                .cmp(&left.updated_at_epoch_seconds)
                .then_with(|| left.issue_identifier.cmp(&right.issue_identifier))
        });
        sessions
    }

    pub(super) fn latest_session(&self) -> Option<AgentSession> {
        self.sorted_sessions().into_iter().next()
    }
}

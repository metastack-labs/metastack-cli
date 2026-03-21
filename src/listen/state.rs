use serde::{Deserialize, Serialize};

use crate::linear::IssueSummary;

use super::{compact_identifier, format_duration, format_number};

pub(super) const COMPLETED_SESSION_TTL_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeProvider {
    Claude,
    Codex,
}

impl ResumeProvider {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestResumeHandle {
    pub provider: ResumeProvider,
    pub id: String,
}

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    #[serde(default)]
    pub input: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
}

impl TokenUsage {
    pub(super) fn total(&self) -> Option<u64> {
        match (self.input, self.output) {
            (None, None) => None,
            (input, output) => Some(input.unwrap_or(0) + output.unwrap_or(0)),
        }
    }

    pub(super) fn accumulate(&mut self, usage: &Self) {
        if let Some(input) = usage.input {
            self.input = Some(self.input.unwrap_or(0) + input);
        }
        if let Some(output) = usage.output {
            self.output = Some(self.output.unwrap_or(0) + output);
        }
    }

    pub(super) fn display_compact(&self) -> String {
        match (self.input, self.output, self.total()) {
            (None, None, _) => "n/a".to_string(),
            (input, output, Some(total)) => format!(
                "in {} | out {} | total {}",
                input
                    .map(format_number)
                    .unwrap_or_else(|| "n/a".to_string()),
                output
                    .map(format_number)
                    .unwrap_or_else(|| "n/a".to_string()),
                format_number(total)
            ),
            (_, _, None) => "n/a".to_string(),
        }
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
    pub latest_resume_handle: Option<LatestResumeHandle>,
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
        self.latest_resume_handle
            .as_ref()
            .map(|resume| compact_identifier(&resume.id))
            .unwrap_or_else(|| "-".to_string())
    }

    pub(super) fn latest_resume_provider_label(&self) -> String {
        self.latest_resume_handle
            .as_ref()
            .map(|resume| resume.provider.label().to_string())
            .unwrap_or_else(|| "-".to_string())
    }

    pub(super) fn latest_resume_id_label(&self) -> String {
        self.latest_resume_handle
            .as_ref()
            .map(|resume| resume.id.clone())
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

#[cfg(test)]
mod tests {
    use super::{AgentSession, LatestResumeHandle, ResumeProvider, SessionPhase, TokenUsage};

    fn session() -> AgentSession {
        AgentSession {
            issue_id: Some("issue-1".to_string()),
            issue_identifier: "ENG-10194".to_string(),
            issue_title: "Capture listen resume IDs".to_string(),
            project_name: Some("MetaStack CLI".to_string()),
            team_key: "ENG".to_string(),
            issue_url: "https://linear.app/issues/ENG-10194".to_string(),
            phase: SessionPhase::Running,
            summary: "Running".to_string(),
            brief_path: None,
            backlog_issue_identifier: None,
            backlog_issue_title: None,
            backlog_path: None,
            workspace_path: None,
            branch: None,
            workpad_comment_id: None,
            updated_at_epoch_seconds: 1,
            pid: None,
            session_id: Some("issue-1".to_string()),
            latest_resume_handle: None,
            turns: Some(1),
            tokens: TokenUsage::default(),
            log_path: None,
        }
    }

    #[test]
    fn session_label_uses_latest_resume_handle_only() {
        let mut session = session();
        assert_eq!(session.session_label(), "-");

        session.latest_resume_handle = Some(LatestResumeHandle {
            provider: ResumeProvider::Codex,
            id: "019cedb4-2293-7651-b0b4-dfac4af6a640".to_string(),
        });

        assert_eq!(session.session_label(), "019c...f6a640");
        assert_eq!(session.latest_resume_provider_label(), "codex");
        assert_eq!(
            session.latest_resume_id_label(),
            "019cedb4-2293-7651-b0b4-dfac4af6a640"
        );
    }
}

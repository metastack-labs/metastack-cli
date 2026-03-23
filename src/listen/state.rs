use serde::{Deserialize, Serialize};

use crate::linear::{AttachmentSummary, IssueSummary};

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

    pub(super) fn for_agent(agent: &str) -> Option<Self> {
        match agent {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestResumeHandle {
    pub provider: ResumeProvider,
    pub id: String,
}

impl LatestResumeHandle {
    pub(super) fn matches_agent(&self, agent: &str) -> bool {
        ResumeProvider::for_agent(agent) == Some(self.provider)
    }
}

pub(super) fn explicit_resume_provider_label(handle: Option<&LatestResumeHandle>) -> String {
    handle
        .map(|handle| handle.provider.label().to_string())
        .unwrap_or_else(|| "unavailable".to_string())
}

pub(super) fn explicit_resume_id_label(handle: Option<&LatestResumeHandle>) -> String {
    handle
        .map(|handle| handle.id.clone())
        .unwrap_or_else(|| "unavailable".to_string())
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

/// A Linear issue currently in `In Progress`, surfaced in the dashboard In Progress Issues pane.
///
/// This is a stable, dashboard-facing view model that contains only the data the TUI needs
/// to render each row and the drill-in detail view, without requiring additional service lookups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveIssue {
    pub identifier: String,
    pub title: String,
    pub assignee: Option<String>,
    pub state_name: String,
    pub has_open_pr: bool,
    pub pr_url: Option<String>,
    pub description: Option<String>,
    pub url: String,
    pub team_key: String,
    pub project: Option<String>,
}

impl ActiveIssue {
    /// Build an `ActiveIssue` from a Linear `IssueSummary`.
    ///
    /// GitHub enrichment considers only open attached PRs. An attachment is
    /// treated as an open GitHub PR when its URL matches the `github.com/.*/pull/`
    /// pattern and the attachment metadata does not indicate a `closed` or `merged` state.
    pub fn from_issue(issue: IssueSummary) -> Self {
        let (has_open_pr, pr_url) = detect_open_github_pr(&issue.attachments);
        Self {
            identifier: issue.identifier,
            title: issue.title,
            assignee: issue.assignee.map(|a| a.name),
            state_name: issue
                .state
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "Unknown".to_string()),
            has_open_pr,
            pr_url,
            description: issue.description,
            url: issue.url,
            team_key: issue.team.key,
            project: issue.project.map(|p| p.name),
        }
    }

    pub(super) fn short_title(&self, max_len: usize) -> String {
        if self.title.len() <= max_len {
            self.title.clone()
        } else {
            format!("{}...", &self.title[..max_len.saturating_sub(3)])
        }
    }

    pub(super) fn assignee_label(&self) -> &str {
        self.assignee.as_deref().unwrap_or("unassigned")
    }

    pub(super) fn pr_label(&self) -> &'static str {
        if self.has_open_pr { "PR" } else { "-" }
    }
}

/// Returns `(has_open_pr, pr_url)` by inspecting Linear issue attachments.
///
/// An attachment is considered an open GitHub PR when its URL contains
/// `github.com/.*/pull/` and the attachment metadata does not explicitly
/// mark the state as `closed` or `merged`.
fn detect_open_github_pr(attachments: &[AttachmentSummary]) -> (bool, Option<String>) {
    for attachment in attachments {
        if !is_github_pr_url(&attachment.url) {
            continue;
        }
        if is_attachment_closed_or_merged(attachment) {
            continue;
        }
        return (true, Some(attachment.url.clone()));
    }
    (false, None)
}

fn is_github_pr_url(url: &str) -> bool {
    url.contains("github.com/") && url.contains("/pull/")
}

fn is_attachment_closed_or_merged(attachment: &AttachmentSummary) -> bool {
    if let Some(state) = attachment.metadata.get("state").and_then(|v| v.as_str()) {
        let normalized = state.to_lowercase();
        return normalized == "closed" || normalized == "merged";
    }
    if let Some(status) = attachment.metadata.get("status").and_then(|v| v.as_str()) {
        let normalized = status.to_lowercase();
        return normalized == "closed" || normalized == "merged";
    }
    false
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

    pub(super) fn display_table_compact(&self) -> String {
        self.total()
            .map(format_number)
            .unwrap_or_else(|| "n/a".to_string())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestStatus {
    #[default]
    Unpublished,
    Draft,
    Ready,
}

impl PullRequestStatus {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Unpublished => "none",
            Self::Draft => "draft",
            Self::Ready => "ready",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestSummary {
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub status: PullRequestStatus,
}

impl PullRequestSummary {
    pub(super) fn compact_label(&self) -> String {
        match (self.status, self.number) {
            (PullRequestStatus::Unpublished, _) => "none".to_string(),
            (PullRequestStatus::Draft, Some(number)) => format!("draft #{number}"),
            (PullRequestStatus::Ready, Some(number)) => format!("ready #{number}"),
            (PullRequestStatus::Draft, None) => "draft".to_string(),
            (PullRequestStatus::Ready, None) => "ready".to_string(),
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
    pub pull_request: PullRequestSummary,
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

    pub(super) fn table_tokens_label(&self) -> String {
        self.tokens.display_table_compact()
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

    pub(super) fn pull_request_label(&self) -> String {
        self.pull_request.compact_label()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Claimed,
    BriefReady,
    Running,
    Paused,
    Completed,
    Blocked,
}

impl SessionPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Claimed => "claimed",
            Self::BriefReady => "brief-ready",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Blocked => "blocked",
        }
    }

    pub fn display_label(self) -> &'static str {
        match self {
            Self::Claimed => "Claimed",
            Self::BriefReady => "Brief Ready",
            Self::Running => "Running",
            Self::Paused => "Paused",
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
            Self::Paused => "warning",
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
                    SessionPhase::Claimed
                        | SessionPhase::BriefReady
                        | SessionPhase::Running
                        | SessionPhase::Paused
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
    use super::{
        AgentSession, LatestResumeHandle, PullRequestStatus, PullRequestSummary, ResumeProvider,
        SessionPhase, TokenUsage, explicit_resume_id_label, explicit_resume_provider_label,
    };

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
            pull_request: PullRequestSummary::default(),
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
            session
                .latest_resume_handle
                .as_ref()
                .map(|resume| resume.id.as_str()),
            Some("019cedb4-2293-7651-b0b4-dfac4af6a640")
        );
    }

    #[test]
    fn explicit_resume_labels_share_unavailable_and_full_id_formatting() {
        assert_eq!(explicit_resume_provider_label(None), "unavailable");
        assert_eq!(explicit_resume_id_label(None), "unavailable");

        let handle = LatestResumeHandle {
            provider: ResumeProvider::Claude,
            id: "provider-resume-123".to_string(),
        };
        assert_eq!(
            explicit_resume_provider_label(Some(&handle)),
            "claude".to_string()
        );
        assert_eq!(
            explicit_resume_id_label(Some(&handle)),
            "provider-resume-123".to_string()
        );
    }

    #[test]
    fn session_pull_request_label_stays_compact() {
        let mut session = session();
        assert_eq!(session.pull_request_label(), "none");

        session.pull_request = PullRequestSummary {
            number: Some(321),
            url: Some("https://github.com/metastack-labs/metastack-cli/pull/321".to_string()),
            status: PullRequestStatus::Draft,
        };
        assert_eq!(session.pull_request_label(), "draft #321");

        session.pull_request.status = PullRequestStatus::Ready;
        assert_eq!(session.pull_request_label(), "ready #321");
    }

    #[test]
    fn pull_request_summary_compact_label_surfaces_ready_status() {
        let pull_request = PullRequestSummary {
            number: Some(321),
            url: Some("https://github.com/metastack-labs/metastack-cli/pull/321".to_string()),
            status: PullRequestStatus::Ready,
        };

        assert_eq!(pull_request.compact_label(), "ready #321");
    }

    #[test]
    fn session_table_tokens_label_prefers_total_only() {
        let mut session = session();
        assert_eq!(session.table_tokens_label(), "n/a");

        session.tokens = TokenUsage {
            input: Some(12_300),
            output: Some(40),
        };
        assert_eq!(session.table_tokens_label(), "12,340");
    }

    #[test]
    fn active_issue_detects_open_github_pr_from_attachments() {
        use crate::linear::{AttachmentSummary, IssueSummary, TeamRef, WorkflowState};
        use serde_json::json;

        let issue = IssueSummary {
            id: "issue-1".to_string(),
            identifier: "MET-99".to_string(),
            title: "Active ticket with open PR".to_string(),
            description: Some("A description".to_string()),
            url: "https://linear.app/issues/MET-99".to_string(),
            priority: None,
            estimate: None,
            updated_at: "2026-03-21T00:00:00Z".to_string(),
            team: TeamRef {
                key: "MET".to_string(),
                id: "team-1".to_string(),
                name: "MetaStack".to_string(),
            },
            project: None,
            assignee: Some(crate::linear::UserRef {
                id: "user-1".to_string(),
                name: "Alice".to_string(),
                email: None,
            }),
            labels: vec![],
            comments: vec![],
            state: Some(WorkflowState {
                id: "state-1".to_string(),
                name: "In Progress".to_string(),
                kind: Some("started".to_string()),
            }),
            attachments: vec![AttachmentSummary {
                id: "att-1".to_string(),
                title: "PR #42".to_string(),
                url: "https://github.com/org/repo/pull/42".to_string(),
                source_type: Some("github".to_string()),
                metadata: json!({}),
            }],
            parent: None,
            children: vec![],
        };

        let active = super::ActiveIssue::from_issue(issue);
        assert!(active.has_open_pr);
        assert_eq!(
            active.pr_url.as_deref(),
            Some("https://github.com/org/repo/pull/42")
        );
        assert_eq!(active.assignee.as_deref(), Some("Alice"));
        assert_eq!(active.state_name, "In Progress");
    }

    #[test]
    fn active_issue_ignores_closed_pr_attachments() {
        use crate::linear::{AttachmentSummary, IssueSummary, TeamRef, WorkflowState};
        use serde_json::json;

        let issue = IssueSummary {
            id: "issue-2".to_string(),
            identifier: "MET-100".to_string(),
            title: "Issue with closed PR".to_string(),
            description: None,
            url: "https://linear.app/issues/MET-100".to_string(),
            priority: None,
            estimate: None,
            updated_at: "2026-03-21T00:00:00Z".to_string(),
            team: TeamRef {
                key: "MET".to_string(),
                id: "team-1".to_string(),
                name: "MetaStack".to_string(),
            },
            project: None,
            assignee: None,
            labels: vec![],
            comments: vec![],
            state: Some(WorkflowState {
                id: "state-1".to_string(),
                name: "In Progress".to_string(),
                kind: Some("started".to_string()),
            }),
            attachments: vec![AttachmentSummary {
                id: "att-2".to_string(),
                title: "PR #43".to_string(),
                url: "https://github.com/org/repo/pull/43".to_string(),
                source_type: Some("github".to_string()),
                metadata: json!({"state": "closed"}),
            }],
            parent: None,
            children: vec![],
        };

        let active = super::ActiveIssue::from_issue(issue);
        assert!(!active.has_open_pr);
        assert!(active.pr_url.is_none());
        assert_eq!(active.assignee_label(), "unassigned");
    }

    #[test]
    fn active_issue_ignores_merged_pr_attachments() {
        use crate::linear::{AttachmentSummary, IssueSummary, TeamRef, WorkflowState};
        use serde_json::json;

        let issue = IssueSummary {
            id: "issue-3".to_string(),
            identifier: "MET-101".to_string(),
            title: "Issue with merged PR".to_string(),
            description: None,
            url: "https://linear.app/issues/MET-101".to_string(),
            priority: None,
            estimate: None,
            updated_at: "2026-03-21T00:00:00Z".to_string(),
            team: TeamRef {
                key: "MET".to_string(),
                id: "team-1".to_string(),
                name: "MetaStack".to_string(),
            },
            project: None,
            assignee: None,
            labels: vec![],
            comments: vec![],
            state: Some(WorkflowState {
                id: "state-1".to_string(),
                name: "In Progress".to_string(),
                kind: Some("started".to_string()),
            }),
            attachments: vec![AttachmentSummary {
                id: "att-3".to_string(),
                title: "PR #44".to_string(),
                url: "https://github.com/org/repo/pull/44".to_string(),
                source_type: Some("github".to_string()),
                metadata: json!({"state": "merged"}),
            }],
            parent: None,
            children: vec![],
        };

        let active = super::ActiveIssue::from_issue(issue);
        assert!(!active.has_open_pr);
        assert!(active.pr_url.is_none());
    }

    #[test]
    fn active_issue_short_title_truncates_long_titles() {
        let active = super::ActiveIssue {
            identifier: "MET-1".to_string(),
            title: "A very long title that should be truncated for the table view".to_string(),
            assignee: None,
            state_name: "In Progress".to_string(),
            has_open_pr: false,
            pr_url: None,
            description: None,
            url: "https://linear.app/issues/MET-1".to_string(),
            team_key: "MET".to_string(),
            project: None,
        };
        let short = active.short_title(20);
        assert!(short.len() <= 20);
        assert!(short.ends_with("..."));
    }

    #[test]
    fn active_issue_pr_label_shows_presence() {
        let mut active = super::ActiveIssue {
            identifier: "MET-1".to_string(),
            title: "Test".to_string(),
            assignee: None,
            state_name: "In Progress".to_string(),
            has_open_pr: true,
            pr_url: Some("https://github.com/org/repo/pull/1".to_string()),
            description: None,
            url: "https://linear.app/issues/MET-1".to_string(),
            team_key: "MET".to_string(),
            project: None,
        };
        assert_eq!(active.pr_label(), "PR");

        active.has_open_pr = false;
        assert_eq!(active.pr_label(), "-");
    }
}

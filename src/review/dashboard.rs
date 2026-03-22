use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};

use super::ReviewDashboardData;
use super::state::ReviewSession;

/// Browser state for the review dashboard TUI.
#[derive(Debug, Clone)]
pub(super) struct ReviewBrowserState {
    pub(super) view: ReviewListView,
    pub(super) selected_active: usize,
    pub(super) selected_completed: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReviewListView {
    Active,
    Completed,
}

/// Input actions for the review dashboard.
#[derive(Debug, Clone, Copy)]
pub(super) enum ReviewBrowserAction {
    Up,
    Down,
    Tab,
    Enter,
    Back,
    Esc,
    PageUp,
    PageDown,
}

impl Default for ReviewBrowserState {
    fn default() -> Self {
        Self {
            view: ReviewListView::Active,
            selected_active: 0,
            selected_completed: 0,
        }
    }
}

impl ReviewBrowserState {
    /// Apply a navigation action to the browser state.
    ///
    /// Adjusts the selected index or switches the active/completed view.
    pub(super) fn apply_action(&mut self, action: ReviewBrowserAction, data: &ReviewDashboardData) {
        match action {
            ReviewBrowserAction::Tab => {
                self.view = match self.view {
                    ReviewListView::Active => ReviewListView::Completed,
                    ReviewListView::Completed => ReviewListView::Active,
                };
            }
            ReviewBrowserAction::Up => {
                let selected = self.selected_mut();
                *selected = selected.saturating_sub(1);
            }
            ReviewBrowserAction::Down => {
                let count = data.sessions_for_view(self.view).len();
                let selected = self.selected_mut();
                if count > 0 && *selected < count - 1 {
                    *selected += 1;
                }
            }
            ReviewBrowserAction::PageUp => {
                let selected = self.selected_mut();
                *selected = selected.saturating_sub(5);
            }
            ReviewBrowserAction::PageDown => {
                let count = data.sessions_for_view(self.view).len();
                let selected = self.selected_mut();
                *selected = (*selected + 5).min(count.saturating_sub(1));
            }
            ReviewBrowserAction::Enter | ReviewBrowserAction::Back | ReviewBrowserAction::Esc => {}
        }
    }

    fn selected_mut(&mut self) -> &mut usize {
        match self.view {
            ReviewListView::Active => &mut self.selected_active,
            ReviewListView::Completed => &mut self.selected_completed,
        }
    }

    fn selected(&self) -> usize {
        match self.view {
            ReviewListView::Active => self.selected_active,
            ReviewListView::Completed => self.selected_completed,
        }
    }
}

/// Render a deterministic snapshot of the review dashboard for testing.
///
/// Returns an error when the terminal backend cannot render.
pub(super) fn render_review_dashboard_snapshot(
    width: u16,
    height: u16,
    data: &ReviewDashboardData,
    state: &ReviewBrowserState,
) -> anyhow::Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render(frame, data, state))?;
    Ok(format!("{}", terminal.backend()))
}

/// Core render function usable by both live terminal and snapshot paths.
///
/// Draws the review dashboard into the provided frame.
pub(super) fn render(
    frame: &mut Frame<'_>,
    data: &ReviewDashboardData,
    state: &ReviewBrowserState,
) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(area);

    render_header(frame, chunks[0], data);
    render_sessions(frame, chunks[1], data, state);
    render_footer(frame, chunks[2], data, state);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, data: &ReviewDashboardData) {
    let lines = vec![
        Line::from(vec![
            Span::styled(
                "Review Dashboard",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::raw(&data.scope),
        ]),
        Line::from(vec![
            Span::raw("Cycle: "),
            Span::raw(&data.cycle_summary),
            Span::raw("  "),
            Span::raw(format!("Watching: {} labeled PRs", data.eligible_prs)),
        ]),
    ];
    let block = Block::default().borders(Borders::BOTTOM);
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_sessions(
    frame: &mut Frame<'_>,
    area: Rect,
    data: &ReviewDashboardData,
    state: &ReviewBrowserState,
) {
    let sections = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(area);
    render_session_list(frame, sections[0], data, state);
    render_session_details(frame, sections[1], data, state);
}

fn render_session_list(
    frame: &mut Frame<'_>,
    area: Rect,
    data: &ReviewDashboardData,
    state: &ReviewBrowserState,
) {
    let sessions = data.sessions_for_view(state.view);
    if sessions.is_empty() {
        let empty_text = match state.view {
            ReviewListView::Active => {
                "No active PR review sessions. Watching for open PRs with the `metastack` label."
            }
            ReviewListView::Completed => "No completed PR review sessions.",
        };
        let paragraph = Paragraph::new(empty_text).wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
        return;
    }

    let header = Row::new(vec![
        Cell::from("PR"),
        Cell::from("Title"),
        Cell::from("Stage"),
        Cell::from("Age"),
        Cell::from("Remediation"),
        Cell::from("Linear"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let now = data.now_epoch_seconds;
    let rows: Vec<Row> = sessions
        .iter()
        .enumerate()
        .map(|(idx, session)| {
            let style = if idx == state.selected() {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            Row::new(vec![
                Cell::from(format!("#{}", session.pr_number)),
                Cell::from(truncate(&session.pr_title, 40)),
                Cell::from(session.stage_label()),
                Cell::from(session.age_label(now)),
                Cell::from(session.remediation_label()),
                Cell::from(
                    session
                        .linear_identifier
                        .as_deref()
                        .unwrap_or("-")
                        .to_string(),
                ),
            ])
            .style(style)
        })
        .collect();

    let widths = [
        Constraint::Length(8),
        Constraint::Min(20),
        Constraint::Length(16),
        Constraint::Length(8),
        Constraint::Length(13),
        Constraint::Length(12),
    ];

    let table = Table::new(rows, widths).header(header);
    frame.render_widget(table, area);
}

fn render_session_details(
    frame: &mut Frame<'_>,
    area: Rect,
    data: &ReviewDashboardData,
    state: &ReviewBrowserState,
) {
    let sessions = data.sessions_for_view(state.view);
    let selected = sessions.get(state.selected()).copied();
    let block = Block::default().borders(Borders::LEFT).title("Details");
    let paragraph = Paragraph::new(detail_text(data, selected))
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    data: &ReviewDashboardData,
    state: &ReviewBrowserState,
) {
    let active_count = data
        .sessions
        .iter()
        .filter(|s| !s.phase.is_completed())
        .count();
    let completed_count = data
        .sessions
        .iter()
        .filter(|s| s.phase.is_completed())
        .count();
    let tab_label = match state.view {
        ReviewListView::Active => {
            format!("[Active ({active_count})] Completed ({completed_count})")
        }
        ReviewListView::Completed => {
            format!("Active ({active_count}) [Completed ({completed_count})]")
        }
    };
    let footer = Line::from(vec![
        Span::raw(tab_label),
        Span::raw("  Tab to switch  q to quit"),
    ]);
    frame.render_widget(Paragraph::new(footer), area);
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

fn detail_text(data: &ReviewDashboardData, session: Option<&ReviewSession>) -> Text<'static> {
    let mut lines = Vec::new();
    if let Some(session) = session {
        lines.push(Line::from(vec![
            Span::styled(
                format!("PR #{}", session.pr_number),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::raw(session.stage_label()),
        ]));
        lines.push(Line::from(session.pr_title.clone()));
        lines.push(Line::from(String::new()));
        lines.push(Line::from(format!("Summary: {}", session.summary)));
        if let Some(url) = &session.pr_url {
            lines.push(Line::from(format!("URL: {url}")));
        }
        if let Some(author) = &session.pr_author {
            lines.push(Line::from(format!("Author: {author}")));
        }
        if session.head_branch.is_some() || session.base_branch.is_some() {
            lines.push(Line::from(format!(
                "Branch: {} -> {}",
                session.head_branch.as_deref().unwrap_or("?"),
                session.base_branch.as_deref().unwrap_or("?")
            )));
        }
        lines.push(Line::from(format!(
            "Linear: {}",
            session.linear_identifier.as_deref().unwrap_or("-")
        )));
        lines.push(Line::from(format!(
            "Remediation: {}",
            session.remediation_label()
        )));
        if let Some(url) = &session.remediation_pr_url {
            lines.push(Line::from(format!("Remediation PR: {url}")));
        }
        if session.needs_remediation_decision() {
            lines.push(Line::from(String::new()));
            lines.push(Line::from(
                "Actions: [a] Create fix PR  [n] Skip remediation",
            ));
        }
    } else {
        lines.push(Line::from("No session selected."));
    }

    if !data.notes.is_empty() {
        lines.push(Line::from(String::new()));
        lines.push(Line::from(Span::styled(
            "Recent Notes",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for note in data.notes.iter().take(4) {
            lines.push(Line::from(format!("- {note}")));
        }
    }

    Text::from(lines)
}

#[cfg(test)]
mod tests {
    use super::{ReviewBrowserState, render_review_dashboard_snapshot};
    use crate::review::ReviewDashboardData;
    use crate::review::state::{ReviewPhase, ReviewSession};

    #[test]
    fn snapshot_shows_selected_session_summary_and_notes() {
        let data = ReviewDashboardData {
            scope: "origin/main".to_string(),
            cycle_summary: "Reviewing PR #42".to_string(),
            eligible_prs: 1,
            sessions: vec![ReviewSession {
                pr_number: 42,
                pr_title: "MET-74 add review dashboard".to_string(),
                pr_url: Some("https://example.test/pull/42".to_string()),
                pr_author: Some("metasudo".to_string()),
                head_branch: Some("met-74-review".to_string()),
                base_branch: Some("main".to_string()),
                linear_identifier: Some("MET-74".to_string()),
                phase: ReviewPhase::Running,
                summary: "Running agent review with codex".to_string(),
                updated_at_epoch_seconds: 1,
                review_output: None,
                remediation_required: None,
                remediation_pr_number: None,
                remediation_pr_url: None,
            }],
            now_epoch_seconds: 5,
            notes: vec!["Starting dashboard before the first review poll completes.".to_string()],
            state_file: "/tmp/review-session.json".to_string(),
        };

        let snapshot =
            render_review_dashboard_snapshot(120, 32, &data, &ReviewBrowserState::default())
                .expect("snapshot should render");

        assert!(snapshot.contains("Running agent review with codex"));
        assert!(snapshot.contains("Recent Notes"));
        assert!(snapshot.contains("MET-74"));
    }
}

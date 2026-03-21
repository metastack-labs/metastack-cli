use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};

use super::ReviewDashboardData;

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

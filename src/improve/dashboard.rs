use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::tui::theme::{emphasis_style, empty_state, muted_style, panel_title};

use super::state::ImproveSession;

/// Dashboard data for the improve TUI.
#[derive(Debug, Clone)]
pub(super) struct ImproveDashboardData {
    pub(super) scope: String,
    pub(super) prs: Vec<ImprovePrEntry>,
    pub(super) sessions: Vec<ImproveSession>,
    pub(super) now_epoch_seconds: u64,
    pub(super) state_file: String,
}

/// A discovered open PR available for improvement.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(super) struct ImprovePrEntry {
    pub(super) number: u64,
    pub(super) title: String,
    pub(super) url: String,
    pub(super) author: String,
    pub(super) head_branch: String,
    pub(super) base_branch: String,
    pub(super) body_preview: String,
}

/// TUI view mode for the improve dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImproveView {
    PrList,
    Sessions,
    PrDetail,
    SessionDetail,
}

/// Input actions for the improve dashboard.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(super) enum ImproveAction {
    Up,
    Down,
    Tab,
    Enter,
    Back,
    Esc,
}

/// Browser state for the improve dashboard TUI.
#[derive(Debug, Clone)]
pub(super) struct ImproveBrowserState {
    pub(super) view: ImproveView,
    pub(super) selected_pr: usize,
    pub(super) selected_session: usize,
}

impl Default for ImproveBrowserState {
    fn default() -> Self {
        Self {
            view: ImproveView::PrList,
            selected_pr: 0,
            selected_session: 0,
        }
    }
}

impl ImproveBrowserState {
    /// Apply a navigation action to the browser state.
    pub(super) fn apply_action(&mut self, action: ImproveAction, data: &ImproveDashboardData) {
        match action {
            ImproveAction::Tab => match self.view {
                ImproveView::PrList => self.view = ImproveView::Sessions,
                ImproveView::Sessions => self.view = ImproveView::PrList,
                ImproveView::PrDetail | ImproveView::SessionDetail => {}
            },
            ImproveAction::Up => {
                if let Some(selected) = self.selected_mut() {
                    *selected = selected.saturating_sub(1);
                }
            }
            ImproveAction::Down => {
                if let Some(count) = self.item_count(data) {
                    if let Some(selected) = self.selected_mut() {
                        if count > 0 && *selected < count - 1 {
                            *selected += 1;
                        }
                    }
                }
            }
            ImproveAction::Enter => match self.view {
                ImproveView::PrList if self.selected_pr < data.prs.len() => {
                    self.view = ImproveView::PrDetail;
                }
                ImproveView::Sessions if self.selected_session < data.sessions.len() => {
                    self.view = ImproveView::SessionDetail;
                }
                _ => {}
            },
            ImproveAction::Back | ImproveAction::Esc => match self.view {
                ImproveView::PrDetail => self.view = ImproveView::PrList,
                ImproveView::SessionDetail => self.view = ImproveView::Sessions,
                _ => {}
            },
        }
    }

    fn selected_mut(&mut self) -> Option<&mut usize> {
        match self.view {
            ImproveView::PrList => Some(&mut self.selected_pr),
            ImproveView::Sessions => Some(&mut self.selected_session),
            ImproveView::PrDetail | ImproveView::SessionDetail => None,
        }
    }

    fn item_count(&self, data: &ImproveDashboardData) -> Option<usize> {
        match self.view {
            ImproveView::PrList => Some(data.prs.len()),
            ImproveView::Sessions => Some(data.sessions.len()),
            ImproveView::PrDetail | ImproveView::SessionDetail => None,
        }
    }
}

#[allow(dead_code)]
impl ImproveDashboardData {
    fn active_sessions(&self) -> Vec<&ImproveSession> {
        self.sessions
            .iter()
            .filter(|s| !s.phase.is_terminal())
            .collect()
    }

    fn completed_sessions(&self) -> Vec<&ImproveSession> {
        self.sessions
            .iter()
            .filter(|s| s.phase.is_terminal())
            .collect()
    }
}

/// Render a deterministic snapshot of the improve dashboard for testing.
///
/// Returns an error when the terminal backend cannot render.
pub(super) fn render_improve_dashboard_snapshot(
    width: u16,
    height: u16,
    data: &ImproveDashboardData,
    state: &ImproveBrowserState,
) -> anyhow::Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render(frame, data, state))?;
    Ok(format!("{}", terminal.backend()))
}

/// Core render function usable by both live terminal and snapshot paths.
pub(super) fn render(
    frame: &mut Frame<'_>,
    data: &ImproveDashboardData,
    state: &ImproveBrowserState,
) {
    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Length(2),
        ])
        .split(area);

    render_header(frame, chunks[0], data);
    render_body(frame, chunks[1], data, state);
    render_footer(frame, chunks[2], state);
}

fn render_header(frame: &mut Frame<'_>, area: Rect, data: &ImproveDashboardData) {
    let title = format!(" Improve: {} ", data.scope);
    let pr_count = data.prs.len();
    let session_count = data.sessions.len();
    let summary = format!("{} open PR(s), {} session(s)", pr_count, session_count);

    let header = Paragraph::new(Line::from(vec![Span::styled(summary, emphasis_style())])).block(
        Block::default()
            .title(panel_title(&title, true))
            .borders(Borders::ALL),
    );
    frame.render_widget(header, area);
}

fn render_body(
    frame: &mut Frame<'_>,
    area: Rect,
    data: &ImproveDashboardData,
    state: &ImproveBrowserState,
) {
    match state.view {
        ImproveView::PrDetail => {
            if let Some(pr) = data.prs.get(state.selected_pr) {
                render_pr_detail(frame, area, pr);
            }
            return;
        }
        ImproveView::SessionDetail => {
            if let Some(session) = data.sessions.get(state.selected_session) {
                render_session_detail(frame, area, session, data.now_epoch_seconds);
            }
            return;
        }
        _ => {}
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    render_pr_list(
        frame,
        chunks[0],
        data,
        state.view == ImproveView::PrList,
        state.selected_pr,
    );
    render_session_list(
        frame,
        chunks[1],
        data,
        state.view == ImproveView::Sessions,
        state.selected_session,
    );
}

fn render_pr_list(
    frame: &mut Frame<'_>,
    area: Rect,
    data: &ImproveDashboardData,
    focused: bool,
    selected: usize,
) {
    let title = if focused {
        " Open PRs [active] "
    } else {
        " Open PRs "
    };

    let border_style = if focused {
        emphasis_style()
    } else {
        muted_style()
    };

    if data.prs.is_empty() {
        let empty = empty_state("No open PRs discovered.", "");
        let block = Block::default()
            .title(panel_title(title, focused))
            .borders(Borders::ALL)
            .border_style(border_style);
        let p = Paragraph::new(empty).block(block);
        frame.render_widget(p, area);
        return;
    }

    let items: Vec<Line<'_>> = data
        .prs
        .iter()
        .enumerate()
        .map(|(i, pr)| {
            let marker = if focused && i == selected { ">" } else { " " };
            let style = if focused && i == selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(vec![Span::styled(
                format!("{marker} #{} {} ({})", pr.number, pr.title, pr.author),
                style,
            )])
        })
        .collect();

    let block = Block::default()
        .title(panel_title(title, focused))
        .borders(Borders::ALL)
        .border_style(border_style);

    let p = Paragraph::new(Text::from(items))
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

fn render_session_list(
    frame: &mut Frame<'_>,
    area: Rect,
    data: &ImproveDashboardData,
    focused: bool,
    selected: usize,
) {
    let title = if focused {
        " Sessions [active] "
    } else {
        " Sessions "
    };

    let border_style = if focused {
        emphasis_style()
    } else {
        muted_style()
    };

    if data.sessions.is_empty() {
        let empty = empty_state("No improve sessions.", "");
        let block = Block::default()
            .title(panel_title(title, focused))
            .borders(Borders::ALL)
            .border_style(border_style);
        let p = Paragraph::new(empty).block(block);
        frame.render_widget(p, area);
        return;
    }

    let items: Vec<Line<'_>> = data
        .sessions
        .iter()
        .enumerate()
        .map(|(i, session)| {
            let marker = if focused && i == selected { ">" } else { " " };
            let style = if focused && i == selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let phase = session.phase.display_label();
            let age = session.age_label(data.now_epoch_seconds);
            Line::from(vec![Span::styled(
                format!(
                    "{marker} #{} [{}] {} ({})",
                    session.source_pr.number, phase, session.source_pr.title, age
                ),
                style,
            )])
        })
        .collect();

    let block = Block::default()
        .title(panel_title(title, focused))
        .borders(Borders::ALL)
        .border_style(border_style);

    let p = Paragraph::new(Text::from(items))
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

fn render_pr_detail(frame: &mut Frame<'_>, area: Rect, pr: &ImprovePrEntry) {
    let title = format!(" PR #{}: {} ", pr.number, pr.title);
    let lines = vec![
        Line::from(vec![
            Span::styled("Author: ", emphasis_style()),
            Span::raw(&pr.author),
        ]),
        Line::from(vec![
            Span::styled("Branch: ", emphasis_style()),
            Span::raw(format!("{} -> {}", pr.head_branch, pr.base_branch)),
        ]),
        Line::from(vec![
            Span::styled("URL:    ", emphasis_style()),
            Span::raw(&pr.url),
        ]),
        Line::from(""),
        Line::from(Span::styled("Body Preview:", emphasis_style())),
        Line::from(if pr.body_preview.is_empty() {
            "(no description)".to_string()
        } else {
            pr.body_preview.clone()
        }),
    ];

    let block = Block::default()
        .title(panel_title(&title, true))
        .borders(Borders::ALL)
        .border_style(emphasis_style());

    let p = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

fn render_session_detail(
    frame: &mut Frame<'_>,
    area: Rect,
    session: &ImproveSession,
    now_epoch_seconds: u64,
) {
    let title = format!(
        " Session: #{} [{}] ",
        session.source_pr.number,
        session.phase.display_label()
    );
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Session ID:  ", emphasis_style()),
            Span::raw(&session.session_id),
        ]),
        Line::from(vec![
            Span::styled("Source PR:   ", emphasis_style()),
            Span::raw(format!(
                "#{} {} ({})",
                session.source_pr.number, session.source_pr.title, session.source_pr.author
            )),
        ]),
        Line::from(vec![
            Span::styled("Branch:      ", emphasis_style()),
            Span::raw(format!(
                "{} -> {}",
                session.source_pr.head_branch, session.source_pr.base_branch
            )),
        ]),
        Line::from(vec![
            Span::styled("Phase:       ", emphasis_style()),
            Span::raw(session.phase.display_label()),
        ]),
        Line::from(vec![
            Span::styled("Age:         ", emphasis_style()),
            Span::raw(session.age_label(now_epoch_seconds)),
        ]),
    ];

    if let Some(branch) = &session.improve_branch {
        lines.push(Line::from(vec![
            Span::styled("Imp. Branch: ", emphasis_style()),
            Span::raw(branch),
        ]));
    }
    if let Some(pr_url) = &session.stacked_pr_url {
        lines.push(Line::from(vec![
            Span::styled("Stacked PR:  ", emphasis_style()),
            Span::raw(pr_url),
        ]));
    }
    if let Some(err) = &session.error_summary {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Error:", emphasis_style())));
        lines.push(Line::from(err.as_str()));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Instructions:", emphasis_style())));
    lines.push(Line::from(session.instructions.as_str()));

    let block = Block::default()
        .title(panel_title(&title, true))
        .borders(Borders::ALL)
        .border_style(emphasis_style());

    let p = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(p, area);
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, state: &ImproveBrowserState) {
    let hints = match state.view {
        ImproveView::PrList => "Tab: Sessions | Up/Down: Navigate | Enter: Select PR | q: Quit",
        ImproveView::Sessions => "Tab: PRs | Up/Down: Navigate | Enter: View Session | q: Quit",
        ImproveView::PrDetail => "Backspace: Back to PRs | q: Quit",
        ImproveView::SessionDetail => "Backspace: Back to Sessions | q: Quit",
    };
    let footer = Paragraph::new(Line::from(Span::styled(hints, muted_style())));
    frame.render_widget(footer, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::improve::state::{ImprovePhase, ImproveSession, ImproveSourcePr};

    fn test_pr(number: u64) -> ImprovePrEntry {
        ImprovePrEntry {
            number,
            title: format!("Test PR #{number}"),
            url: format!("https://example.test/pull/{number}"),
            author: "alice".to_string(),
            head_branch: format!("feature-{number}"),
            base_branch: "main".to_string(),
            body_preview: "Test body".to_string(),
        }
    }

    fn test_session(id: &str, phase: ImprovePhase) -> ImproveSession {
        ImproveSession {
            session_id: id.to_string(),
            source_pr: ImproveSourcePr {
                number: 42,
                title: "Test PR #42".to_string(),
                url: "https://example.test/pull/42".to_string(),
                author: "alice".to_string(),
                head_branch: "feature".to_string(),
                base_branch: "main".to_string(),
            },
            instructions: "Fix tests".to_string(),
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

    fn test_data() -> ImproveDashboardData {
        ImproveDashboardData {
            scope: "test/repo".to_string(),
            prs: vec![test_pr(1), test_pr(2), test_pr(3)],
            sessions: vec![
                test_session("s1", ImprovePhase::Running),
                test_session("s2", ImprovePhase::Completed),
            ],
            now_epoch_seconds: 2000,
            state_file: format!(
                "{}/agents/improve/sessions/state.json",
                crate::branding::PROJECT_DIR
            ),
        }
    }

    #[test]
    fn render_once_empty_state() {
        let data = ImproveDashboardData {
            scope: "test/repo".to_string(),
            prs: vec![],
            sessions: vec![],
            now_epoch_seconds: 1000,
            state_file: "state.json".to_string(),
        };
        let state = ImproveBrowserState::default();
        let snapshot =
            render_improve_dashboard_snapshot(80, 20, &data, &state).expect("render should work");
        assert!(snapshot.contains("Improve: test/repo"));
        assert!(snapshot.contains("No open PRs"));
        assert!(snapshot.contains("No improve sessions"));
    }

    #[test]
    fn render_once_with_prs_and_sessions() {
        let data = test_data();
        let state = ImproveBrowserState::default();
        let snapshot =
            render_improve_dashboard_snapshot(100, 24, &data, &state).expect("render should work");
        assert!(snapshot.contains("Improve: test/repo"));
        assert!(snapshot.contains("#1"));
        assert!(snapshot.contains("#2"));
        assert!(snapshot.contains("#3"));
        assert!(snapshot.contains("Running"));
        assert!(snapshot.contains("Completed"));
    }

    #[test]
    fn browser_tab_switches_view() {
        let data = test_data();
        let mut state = ImproveBrowserState::default();
        assert_eq!(state.view, ImproveView::PrList);
        state.apply_action(ImproveAction::Tab, &data);
        assert_eq!(state.view, ImproveView::Sessions);
        state.apply_action(ImproveAction::Tab, &data);
        assert_eq!(state.view, ImproveView::PrList);
    }

    #[test]
    fn browser_up_down_navigation() {
        let data = test_data();
        let mut state = ImproveBrowserState::default();

        state.apply_action(ImproveAction::Down, &data);
        assert_eq!(state.selected_pr, 1);

        state.apply_action(ImproveAction::Down, &data);
        assert_eq!(state.selected_pr, 2);

        state.apply_action(ImproveAction::Down, &data);
        assert_eq!(state.selected_pr, 2); // clamped

        state.apply_action(ImproveAction::Up, &data);
        assert_eq!(state.selected_pr, 1);
    }

    #[test]
    fn enter_on_pr_list_opens_detail() {
        let data = test_data();
        let mut state = ImproveBrowserState::default();
        assert_eq!(state.view, ImproveView::PrList);

        state.apply_action(ImproveAction::Enter, &data);
        assert_eq!(state.view, ImproveView::PrDetail);
    }

    #[test]
    fn back_from_pr_detail_returns_to_list() {
        let data = test_data();
        let mut state = ImproveBrowserState::default();
        state.apply_action(ImproveAction::Enter, &data);
        assert_eq!(state.view, ImproveView::PrDetail);

        state.apply_action(ImproveAction::Back, &data);
        assert_eq!(state.view, ImproveView::PrList);
    }

    #[test]
    fn enter_on_session_list_opens_detail() {
        let data = test_data();
        let mut state = ImproveBrowserState::default();
        state.apply_action(ImproveAction::Tab, &data);
        assert_eq!(state.view, ImproveView::Sessions);

        state.apply_action(ImproveAction::Enter, &data);
        assert_eq!(state.view, ImproveView::SessionDetail);
    }

    #[test]
    fn back_from_session_detail_returns_to_list() {
        let data = test_data();
        let mut state = ImproveBrowserState::default();
        state.apply_action(ImproveAction::Tab, &data);
        state.apply_action(ImproveAction::Enter, &data);
        assert_eq!(state.view, ImproveView::SessionDetail);

        state.apply_action(ImproveAction::Back, &data);
        assert_eq!(state.view, ImproveView::Sessions);
    }

    #[test]
    fn tab_does_nothing_in_detail_views() {
        let data = test_data();
        let mut state = ImproveBrowserState::default();
        state.apply_action(ImproveAction::Enter, &data);
        assert_eq!(state.view, ImproveView::PrDetail);

        state.apply_action(ImproveAction::Tab, &data);
        assert_eq!(state.view, ImproveView::PrDetail);
    }

    #[test]
    fn enter_on_empty_list_does_nothing() {
        let data = ImproveDashboardData {
            scope: "test/repo".to_string(),
            prs: vec![],
            sessions: vec![],
            now_epoch_seconds: 1000,
            state_file: "state.json".to_string(),
        };
        let mut state = ImproveBrowserState::default();
        state.apply_action(ImproveAction::Enter, &data);
        assert_eq!(state.view, ImproveView::PrList);
    }

    #[test]
    fn render_pr_detail_snapshot() {
        let data = test_data();
        let mut state = ImproveBrowserState::default();
        state.apply_action(ImproveAction::Enter, &data);

        let snapshot =
            render_improve_dashboard_snapshot(100, 24, &data, &state).expect("render should work");
        assert!(snapshot.contains("PR #1"));
        assert!(snapshot.contains("Author:"));
        assert!(snapshot.contains("alice"));
        assert!(snapshot.contains("feature-1"));
        assert!(snapshot.contains("Back to PRs"));
    }

    #[test]
    fn render_session_detail_snapshot() {
        let data = test_data();
        let mut state = ImproveBrowserState::default();
        state.apply_action(ImproveAction::Tab, &data);
        state.apply_action(ImproveAction::Enter, &data);

        let snapshot =
            render_improve_dashboard_snapshot(100, 24, &data, &state).expect("render should work");
        assert!(snapshot.contains("Session:"));
        assert!(snapshot.contains("Running"));
        assert!(snapshot.contains("Session ID:"));
        assert!(snapshot.contains("Instructions:"));
        assert!(snapshot.contains("Back to Sessions"));
    }
}

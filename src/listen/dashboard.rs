use anyhow::Result;
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Cell, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::{Frame, Terminal};

use super::{ActiveIssue, ListenDashboardData, ListenSessionDetail, SessionListView, SessionPhase};
use crate::tui::scroll::{clamp_offset, plain_text, wrapped_rows};
use crate::tui::spaced_list::spaced_list_item;
use crate::tui::theme::{Tone, badge, content_panel, empty_state, key_hints, panel, panel_title};

/// Which pane currently owns keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FocusPane {
    #[default]
    Sessions,
    ActiveIssues,
}

impl FocusPane {
    pub(crate) fn toggle(self) -> Self {
        match self {
            Self::Sessions => Self::ActiveIssues,
            Self::ActiveIssues => Self::Sessions,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionBrowserState {
    pub(crate) focus: FocusPane,
    pub(crate) view: SessionListView,
    pub(crate) selected_active: usize,
    pub(crate) selected_completed: usize,
    pub(crate) selected_active_issue: usize,
    pub(crate) detail_mode: bool,
    pub(crate) detail_scroll: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionBrowserAction {
    Up,
    Down,
    Tab,
    Left,
    Right,
    Enter,
    Pause,
    Back,
    PageUp,
    PageDown,
}

impl Default for SessionBrowserState {
    fn default() -> Self {
        Self {
            focus: FocusPane::default(),
            view: SessionListView::Active,
            selected_active: 0,
            selected_completed: 0,
            selected_active_issue: 0,
            detail_mode: false,
            detail_scroll: 0,
        }
    }
}

impl SessionBrowserState {
    pub(crate) fn normalize(&mut self, data: &ListenDashboardData) {
        let active_len = data.sessions_for_view(SessionListView::Active).len();
        let completed_len = data.sessions_for_view(SessionListView::Completed).len();
        self.selected_active = clamp_index(self.selected_active, active_len);
        self.selected_completed = clamp_index(self.selected_completed, completed_len);
        self.selected_active_issue =
            clamp_index(self.selected_active_issue, data.active_issues.len());
        if !data.show_active_issues && matches!(self.focus, FocusPane::ActiveIssues) {
            self.focus = FocusPane::Sessions;
        }
        if self.detail_mode && self.has_no_detail_target(data) {
            self.detail_mode = false;
            self.detail_scroll = 0;
        }
    }

    fn has_no_detail_target(&self, data: &ListenDashboardData) -> bool {
        match self.focus {
            FocusPane::Sessions => self.selected_session(data).is_none(),
            FocusPane::ActiveIssues => self.selected_active_issue(data).is_none(),
        }
    }

    pub(crate) fn clamp_detail_scroll(
        &mut self,
        data: &ListenDashboardData,
        width: u16,
        height: u16,
    ) {
        let Some((viewport_height, content_rows)) =
            detail_scroll_metrics(data, self, width, height)
        else {
            self.detail_scroll = 0;
            return;
        };
        self.detail_scroll = clamp_offset(self.detail_scroll, viewport_height, content_rows);
    }

    pub(crate) fn select_previous(&mut self, data: &ListenDashboardData) {
        match self.focus {
            FocusPane::Sessions => match self.view {
                SessionListView::Active => {
                    self.selected_active = self.selected_active.saturating_sub(1);
                }
                SessionListView::Completed => {
                    self.selected_completed = self.selected_completed.saturating_sub(1);
                }
            },
            FocusPane::ActiveIssues => {
                self.selected_active_issue = self.selected_active_issue.saturating_sub(1);
            }
        }
        self.detail_scroll = 0;
        self.normalize(data);
    }

    pub(crate) fn select_next(&mut self, data: &ListenDashboardData) {
        match self.focus {
            FocusPane::Sessions => match self.view {
                SessionListView::Active => {
                    self.selected_active = self.selected_active.saturating_add(1);
                }
                SessionListView::Completed => {
                    self.selected_completed = self.selected_completed.saturating_add(1);
                }
            },
            FocusPane::ActiveIssues => {
                self.selected_active_issue = self.selected_active_issue.saturating_add(1);
            }
        }
        self.detail_scroll = 0;
        self.normalize(data);
    }

    pub(crate) fn selected_session<'a>(
        &self,
        data: &'a ListenDashboardData,
    ) -> Option<&'a super::AgentSession> {
        let sessions = data.sessions_for_view(self.view);
        let index = match self.view {
            SessionListView::Active => self.selected_active,
            SessionListView::Completed => self.selected_completed,
        };
        sessions.get(index).copied()
    }

    pub(crate) fn selected_active_issue<'a>(
        &self,
        data: &'a ListenDashboardData,
    ) -> Option<&'a ActiveIssue> {
        data.active_issues.get(self.selected_active_issue)
    }

    pub(crate) fn apply_action(
        &mut self,
        data: &ListenDashboardData,
        action: SessionBrowserAction,
    ) {
        match action {
            SessionBrowserAction::Up => {
                if self.detail_mode {
                    self.detail_scroll = self.detail_scroll.saturating_sub(1);
                } else {
                    self.select_previous(data);
                }
            }
            SessionBrowserAction::Down => {
                if self.detail_mode {
                    self.detail_scroll = self.detail_scroll.saturating_add(1);
                } else {
                    self.select_next(data);
                }
            }
            SessionBrowserAction::Tab => {
                if data.show_active_issues {
                    self.focus = self.focus.toggle();
                    self.detail_mode = false;
                    self.detail_scroll = 0;
                } else {
                    self.view = self.view.toggle();
                    self.detail_scroll = 0;
                }
            }
            SessionBrowserAction::Left => {
                if matches!(self.focus, FocusPane::Sessions) {
                    self.view = SessionListView::Active;
                }
                self.detail_scroll = 0;
            }
            SessionBrowserAction::Right => {
                if matches!(self.focus, FocusPane::Sessions) {
                    self.view = SessionListView::Completed;
                }
                self.detail_scroll = 0;
            }
            SessionBrowserAction::Enter => {
                if data.show_preview {
                    self.detail_mode = !self.detail_mode;
                    self.detail_scroll = 0;
                }
            }
            SessionBrowserAction::Pause => {}
            SessionBrowserAction::Back => {
                self.detail_mode = false;
                self.detail_scroll = 0;
            }
            SessionBrowserAction::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(5);
            }
            SessionBrowserAction::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(5);
            }
        }
        self.normalize(data);
    }
}

pub fn render_dashboard(data: &ListenDashboardData, width: u16, height: u16) -> Result<String> {
    render_dashboard_with_state(data, width, height, SessionBrowserState::default())
}

#[cfg(test)]
fn render_dashboard_with_view(
    data: &ListenDashboardData,
    width: u16,
    height: u16,
    view: SessionListView,
) -> Result<String> {
    let state = SessionBrowserState {
        view,
        ..SessionBrowserState::default()
    };
    render_dashboard_with_state(data, width, height, state)
}

pub(crate) fn render_dashboard_with_state(
    data: &ListenDashboardData,
    width: u16,
    height: u16,
    mut state: SessionBrowserState,
) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    state.normalize(data);
    state.clamp_detail_scroll(data, width, height);
    terminal.draw(|frame| render(frame, data, &state))?;
    Ok(snapshot(terminal.backend()))
}

pub(crate) fn render(
    frame: &mut Frame<'_>,
    data: &ListenDashboardData,
    state: &SessionBrowserState,
) {
    let area = frame.area();
    let footer_height = if area.width >= 110 && area.height >= 30 {
        8
    } else {
        0
    };
    let header_height = if area.width >= 120 { 11 } else { 12 };
    let show_active_issues = data.show_active_issues && !data.active_issues.is_empty();
    let active_issue_detail =
        state.detail_mode && matches!(state.focus, FocusPane::ActiveIssues) && data.show_preview;
    let active_issues_height: u16 = if show_active_issues {
        let rows = data.active_issues.len() as u16;
        if active_issue_detail {
            (rows + 4).clamp(16, 24)
        } else {
            (rows + 4).min(16)
        }
    } else {
        0
    };
    let mut constraints = vec![Constraint::Length(header_height), Constraint::Min(8)];
    if active_issues_height > 0 {
        constraints.push(Constraint::Length(active_issues_height));
    }
    if footer_height > 0 {
        constraints.push(Constraint::Length(footer_height));
    }
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    render_header(frame, data, sections[0]);
    render_sessions(frame, data, sections[1], state);

    let mut next_section = 2;
    if active_issues_height > 0 {
        render_active_issues(frame, data, sections[next_section], state);
        next_section += 1;
    }
    if footer_height > 0 && next_section < sections.len() {
        render_footer(frame, data, sections[next_section]);
    }
}

fn render_header(frame: &mut Frame<'_>, data: &ListenDashboardData, area: Rect) {
    if area.width < 110 {
        let compact = Paragraph::new(Text::from(vec![
            Line::from(vec![
                badge("listen", Tone::Accent),
                Span::raw(" "),
                Span::raw(data.title.clone()),
            ]),
            Line::from(Span::styled(
                data.scope.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(vec![
                Span::styled("Watching: ", label_style()),
                Span::styled(data.watch_scope.clone(), value_style(Color::LightGreen)),
            ]),
            Line::from(Span::styled(
                data.cycle_summary.clone(),
                Style::default().fg(Color::Gray),
            )),
            runtime_line(
                "Execution agent",
                data.resolved_agent.as_deref().unwrap_or("unresolved"),
                Color::LightCyan,
            ),
            runtime_line("Agents", &data.runtime.agents, Color::Green),
            runtime_line("Throughput", &data.runtime.throughput, Color::Cyan),
            runtime_line("Runtime", &data.runtime.runtime, Color::Yellow),
            runtime_line("Tokens", &data.runtime.tokens, Color::Magenta),
            runtime_line("Rate Limits", &data.runtime.rate_limits, Color::LightBlue),
            runtime_line("Dashboard", &data.runtime.dashboard, Color::LightCyan),
            runtime_line(
                "Terminal refresh",
                &data.runtime.dashboard_refresh,
                Color::Yellow,
            ),
            runtime_line(
                "Linear refresh",
                &data.runtime.linear_refresh,
                Color::LightYellow,
            ),
            key_hints(&[
                ("Tab", "toggle view"),
                (
                    if data.vim_mode {
                        "←/→/h/l"
                    } else {
                        "←/→"
                    },
                    "switch tabs",
                ),
                ("p", "pause running"),
                ("r", "resume paused / retry blocked"),
                ("q", "exit"),
            ]),
        ]))
        .wrap(Wrap { trim: true })
        .block(panel(panel_title("Listen Status", false)));
        frame.render_widget(compact, area);
        return;
    }

    let direction = if area.width >= 110 {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };
    let chunks = Layout::default()
        .direction(direction)
        .constraints(match direction {
            Direction::Horizontal => vec![Constraint::Percentage(36), Constraint::Percentage(64)],
            Direction::Vertical => vec![Constraint::Length(5), Constraint::Min(6)],
        })
        .split(area);

    let hero = Paragraph::new(Text::from(vec![
        Line::from(vec![
            badge("listen", Tone::Accent),
            Span::raw(" "),
            Span::raw(data.title.clone()),
        ]),
        Line::from(Span::styled(
            data.scope.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("Watching: ", label_style()),
            Span::styled(data.watch_scope.clone(), value_style(Color::LightGreen)),
        ]),
        Line::from(Span::styled(
            data.cycle_summary.clone(),
            Style::default().fg(Color::Gray),
        )),
        Line::from(vec![
            Span::styled("State file: ", label_style()),
            Span::styled(data.state_file.clone(), value_style(Color::Green)),
        ]),
        key_hints(&[
            (
                "Tab",
                if data.show_active_issues {
                    "switch pane"
                } else {
                    "toggle view"
                },
            ),
            (
                if data.vim_mode {
                    "←/→/h/l"
                } else {
                    "←/→"
                },
                "switch views",
            ),
            ("p", "pause running"),
            ("r", "resume paused / retry blocked"),
            ("q", "exit"),
        ]),
    ]))
    .wrap(Wrap { trim: true })
    .block(panel(panel_title("Listen Status", false)));
    frame.render_widget(hero, chunks[0]);

    let runtime_lines = vec![
        runtime_line(
            "Execution agent",
            data.resolved_agent.as_deref().unwrap_or("unresolved"),
            Color::LightCyan,
        ),
        runtime_line("Agents", &data.runtime.agents, Color::Green),
        runtime_line("Throughput", &data.runtime.throughput, Color::Cyan),
        runtime_line("Runtime", &data.runtime.runtime, Color::Yellow),
        runtime_line("Tokens", &data.runtime.tokens, Color::Magenta),
        runtime_line("Rate Limits", &data.runtime.rate_limits, Color::LightBlue),
        runtime_line("Project", &data.runtime.project, Color::White),
        runtime_line("Watching", &data.watch_scope, Color::LightGreen),
        runtime_line("Dashboard", &data.runtime.dashboard, Color::LightCyan),
        runtime_line(
            "Terminal refresh",
            &data.runtime.dashboard_refresh,
            Color::Yellow,
        ),
        runtime_line(
            "Linear refresh",
            &data.runtime.linear_refresh,
            Color::LightYellow,
        ),
    ];
    let runtime = Paragraph::new(Text::from(runtime_lines))
        .wrap(Wrap { trim: true })
        .block(panel(panel_title("Runtime", false)));
    frame.render_widget(runtime, chunks[1]);
}

fn render_sessions(
    frame: &mut Frame<'_>,
    data: &ListenDashboardData,
    area: Rect,
    state: &SessionBrowserState,
) {
    let view = state.view;
    let counts = data.session_counts();
    let sessions = data.sessions_for_view(view);
    let is_focused = matches!(state.focus, FocusPane::Sessions);
    let block = panel(panel_title("Agent Sessions", is_focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(inner);

    let tab_hint = if data.show_active_issues {
        "Tab pane"
    } else {
        "Tab view"
    };
    let nav_hint = if data.vim_mode {
        format!(
            "  {tab_hint}  ←/→/h/l views  ↑/↓/j/k select  Enter detail  p pause  r resume/retry"
        )
    } else {
        format!("  {tab_hint}  ←/→ views  ↑/↓ select  Enter detail  p pause  r resume/retry")
    };
    let controls = Paragraph::new(Line::from(vec![
        Span::styled("Views: ", label_style()),
        session_view_badge(SessionListView::Active, view, counts.active),
        Span::raw(" "),
        session_view_badge(SessionListView::Completed, view, counts.completed),
        Span::styled(nav_hint, Style::default().fg(Color::DarkGray)),
    ]))
    .wrap(Wrap { trim: true });
    frame.render_widget(controls, sections[0]);

    if sessions.is_empty() {
        let empty = Paragraph::new(match view {
            SessionListView::Active => empty_state(
                "No active agent sessions are currently tracked.",
                "New claimed tickets will appear here once workers start.",
            ),
            SessionListView::Completed => empty_state(
                "No completed agent sessions are currently tracked.",
                "Completed workers will move into this view after they exit.",
            ),
        })
        .wrap(Wrap { trim: true });
        frame.render_widget(empty, sections[1]);
        return;
    }

    let show_session_detail = state.detail_mode && is_focused && data.show_preview;
    let layout = if show_session_detail {
        if sections[1].width >= 150 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                .split(sections[1])
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(10), Constraint::Min(0)])
                .split(sections[1])
        }
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0)])
            .split(sections[1])
    };

    render_session_table(frame, data, layout[0], view, sessions, state);

    if show_session_detail && layout.len() > 1 {
        if let Some(session) = state.selected_session(data) {
            render_session_detail(
                frame,
                session,
                data.detail_for_session(&session.issue_identifier),
                layout[1],
                clamp_detail_scroll_for_area(
                    session,
                    data.detail_for_session(&session.issue_identifier),
                    layout[1],
                    state.detail_scroll,
                ),
            );
        }
    }
}

fn render_active_issues(
    frame: &mut Frame<'_>,
    data: &ListenDashboardData,
    area: Rect,
    state: &SessionBrowserState,
) {
    let is_focused = matches!(state.focus, FocusPane::ActiveIssues);
    let block = panel(panel_title("In Progress Issues - All Users", is_focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if data.active_issues.is_empty() {
        let empty = Paragraph::new(empty_state(
            "No In Progress issues found.",
            "Issues moved to In Progress will appear here.",
        ))
        .wrap(Wrap { trim: true });
        frame.render_widget(empty, inner);
        return;
    }

    let show_detail = state.detail_mode && is_focused && data.show_preview;
    let layout = if show_detail {
        if inner.width >= 150 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(inner)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(4), Constraint::Min(0)])
                .split(inner)
        }
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0)])
            .split(inner)
    };

    let selected_index = state.selected_active_issue;
    let max_title_len = if layout[0].width > 60 { 50 } else { 30 };
    let rows = data.active_issues.iter().enumerate().map(|(index, issue)| {
        Row::new(vec![
            Cell::from(if index == selected_index && is_focused {
                ">"
            } else {
                " "
            }),
            Cell::from(issue.identifier.clone()),
            Cell::from(issue.short_title(max_title_len)),
            Cell::from(issue.assignee_label().to_string()),
            Cell::from(issue.pr_label()),
        ])
    });
    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("ID"),
        Cell::from("TITLE"),
        Cell::from("ASSIGNEE"),
        Cell::from("PR"),
    ])
    .style(
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(1);
    let constraints = vec![
        Constraint::Length(2),
        Constraint::Length(9),
        Constraint::Min(20),
        Constraint::Length(16),
        Constraint::Length(4),
    ];
    let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(1);
    frame.render_widget(table, layout[0]);

    if show_detail && layout.len() > 1 {
        if let Some(issue) = state.selected_active_issue(data) {
            render_active_issue_detail(frame, issue, layout[1], state.detail_scroll);
        }
    }
}

fn render_active_issue_detail(frame: &mut Frame<'_>, issue: &ActiveIssue, area: Rect, scroll: u16) {
    let content = render_active_issue_detail_text(issue);
    let detail = Paragraph::new(content)
        .wrap(Wrap { trim: true })
        .scroll((scroll, 0))
        .block(content_panel(panel_title("Issue Detail", false)));
    frame.render_widget(detail, area);
}

fn render_active_issue_detail_text(issue: &ActiveIssue) -> Text<'static> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", issue.identifier),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                issue.state_name.clone(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            issue.title.clone(),
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        detail_line("Assignee", issue.assignee_label()),
        detail_line(
            "PR",
            if issue.has_open_pr {
                issue.pr_url.as_deref().unwrap_or("open")
            } else {
                "none"
            },
        ),
        detail_line("URL", &issue.url),
    ];

    if let Some(project) = issue.project.as_deref() {
        lines.push(detail_line("Project", project));
    }

    if let Some(description) = issue.description.as_deref() {
        lines.push(Line::from(""));
        lines.push(section_header("Description"));
        for desc_line in description.lines() {
            lines.push(Line::from(Span::styled(
                desc_line.to_string(),
                Style::default().fg(Color::White),
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Esc/Backspace close detail. PgUp/PgDn scroll.",
        Style::default().fg(Color::DarkGray),
    )));

    Text::from(lines)
}

fn render_session_table(
    frame: &mut Frame<'_>,
    data: &ListenDashboardData,
    area: Rect,
    view: SessionListView,
    sessions: Vec<&super::AgentSession>,
    state: &SessionBrowserState,
) {
    let is_focused = matches!(state.focus, FocusPane::Sessions);
    let selected_index = match view {
        SessionListView::Active => state.selected_active,
        SessionListView::Completed => state.selected_completed,
    };
    let rows = sessions.into_iter().enumerate().map(|(index, session)| {
        let mut issue_lines = vec![Line::from(session.issue_identifier.clone())];
        if let Some(backlog_issue_identifier) = session.backlog_issue_identifier.as_deref()
            && !backlog_issue_identifier.eq_ignore_ascii_case(&session.issue_identifier)
        {
            issue_lines.push(Line::from(Span::styled(
                format!("backlog {backlog_issue_identifier}"),
                Style::default().fg(Color::Gray),
            )));
        }
        Row::new(vec![
            Cell::from(if index == selected_index && is_focused {
                ">"
            } else {
                " "
            }),
            Cell::from(Text::from(issue_lines)),
            Cell::from(Span::styled(
                session.stage_label(),
                phase_style(session.phase),
            )),
            Cell::from(session.pid_label()),
            Cell::from(session.age_label(data.runtime.current_epoch_seconds)),
            Cell::from(session.pull_request_label()),
            Cell::from(session.table_tokens_label()),
            Cell::from(session.latest_resume_provider_label()),
            Cell::from(session.session_label()),
            Cell::from(session.summary.clone()),
        ])
    });
    let header = Row::new(vec![
        Cell::from(""),
        Cell::from("ID"),
        Cell::from("STAGE"),
        Cell::from("PID"),
        Cell::from("AGE"),
        Cell::from("PR"),
        Cell::from("TOKENS"),
        Cell::from("PROVIDER"),
        Cell::from("SESSION"),
        Cell::from("PROGRESS"),
    ])
    .style(
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(1);
    let constraints = if area.width >= 175 {
        vec![
            Constraint::Length(2),
            Constraint::Length(10),
            Constraint::Length(13),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(13),
            Constraint::Min(52),
        ]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(9),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Length(10),
            Constraint::Length(9),
            Constraint::Length(8),
            Constraint::Length(13),
            Constraint::Min(29),
        ]
    };
    let table = Table::new(rows, constraints)
        .header(header)
        .column_spacing(1);
    frame.render_widget(table, area);
}

fn render_session_detail(
    frame: &mut Frame<'_>,
    session: &super::AgentSession,
    detail: Option<&ListenSessionDetail>,
    area: Rect,
    scroll: u16,
) {
    let content = match detail {
        Some(detail) => render_session_detail_text(session, detail),
        None => empty_state(
            "No structured detail artifact is available yet.",
            "The worker will populate session detail as soon as it refreshes this session.",
        ),
    };
    let detail = Paragraph::new(content)
        .wrap(Wrap { trim: true })
        .scroll((scroll, 0))
        .block(content_panel(panel_title("Selected Session", false)));
    frame.render_widget(detail, area);
}

fn detail_scroll_metrics(
    data: &ListenDashboardData,
    state: &SessionBrowserState,
    width: u16,
    height: u16,
) -> Option<(u16, usize)> {
    if !state.detail_mode {
        return None;
    }
    if matches!(state.focus, FocusPane::ActiveIssues) {
        return None;
    }
    let session = state.selected_session(data)?;
    let area = Rect::new(0, 0, width, height);
    let footer_height = if area.width >= 110 && area.height >= 30 {
        8
    } else {
        0
    };
    let header_height = if area.width >= 120 { 10 } else { 12 };
    let constraints = if footer_height > 0 {
        vec![
            Constraint::Length(header_height),
            Constraint::Min(8),
            Constraint::Length(footer_height),
        ]
    } else {
        vec![Constraint::Length(header_height), Constraint::Min(8)]
    };
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let sessions_block = panel(panel_title("Agent Sessions", false));
    let sessions_inner = sessions_block.inner(sections[1]);
    if sessions_inner.width == 0 || sessions_inner.height == 0 {
        return None;
    }
    let sessions_sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(sessions_inner);
    let detail_area = detail_panel_area(sessions_sections[1], state.detail_mode)?;
    let detail = data.detail_for_session(&session.issue_identifier);
    Some(detail_content_metrics(session, detail, detail_area))
}

fn detail_panel_area(area: Rect, detail_mode: bool) -> Option<Rect> {
    if !detail_mode {
        return None;
    }
    let layout = if area.width >= 150 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(10), Constraint::Min(0)])
            .split(area)
    };
    (layout.len() > 1).then_some(layout[1])
}

fn clamp_detail_scroll_for_area(
    session: &super::AgentSession,
    detail: Option<&ListenSessionDetail>,
    area: Rect,
    scroll: u16,
) -> u16 {
    let (viewport_height, content_rows) = detail_content_metrics(session, detail, area);
    clamp_offset(scroll, viewport_height, content_rows)
}

fn detail_content_metrics(
    session: &super::AgentSession,
    detail: Option<&ListenSessionDetail>,
    area: Rect,
) -> (u16, usize) {
    let inner = content_panel(panel_title("Selected Session", false)).inner(area);
    let content = match detail {
        Some(detail) => render_session_detail_text(session, detail),
        None => empty_state(
            "No structured detail artifact is available yet.",
            "The worker will populate session detail as soon as it refreshes this session.",
        ),
    };
    let content_rows = wrapped_rows(&plain_text(&content), inner.width.max(1));
    (inner.height.max(1), content_rows)
}

fn render_session_detail_text(
    session: &super::AgentSession,
    detail: &ListenSessionDetail,
) -> Text<'static> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                format!("{} ", session.issue_identifier),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(session.stage_label(), phase_style(session.phase)),
        ]),
        Line::from(Span::styled(
            session.issue_title.clone(),
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        detail_line("Summary", &detail.summary),
        detail_line("Turns", &detail.turns.unwrap_or(0).to_string()),
        detail_line("Tokens", &detail.tokens.display_compact()),
        detail_line("PR", &detail.pull_request.compact_label()),
    ];

    if let Some(url) = detail.pull_request.url.as_deref() {
        lines.push(detail_line("PR URL", url));
    } else if let Some(number) = detail.pull_request.number {
        lines.push(detail_line("PR Ref", &format!("#{number}")));
    }
    if let Some(branch) = detail.references.branch.as_deref() {
        lines.push(detail_line("Branch", branch));
    }
    if let Some(workspace) = detail.references.workspace_path.as_deref() {
        lines.push(detail_line("Workspace", workspace));
    }
    if let Some(backlog) = detail.references.backlog_path.as_deref() {
        lines.push(detail_line("Backlog", backlog));
    }
    if let Some(brief) = detail.references.brief_path.as_deref() {
        lines.push(detail_line("Brief", brief));
    }
    if let Some(workpad_comment_id) = detail.references.workpad_comment_id.as_deref() {
        lines.push(detail_line("Workpad", workpad_comment_id));
    }
    if let Some(log_path) = detail.references.log_path.as_deref() {
        lines.push(detail_line("Log", log_path));
    }

    if !detail.prompt_context.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_header("Prompt Context"));
        for reference in &detail.prompt_context {
            lines.push(detail_bullet(&format!(
                "{}: {}",
                reference.label, reference.value
            )));
        }
    }

    if !detail.milestones.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_header("Milestones"));
        for milestone in detail.milestones.iter().rev().take(5).rev() {
            let pr_suffix = match milestone.pull_request_number {
                Some(number) => format!(" | {} #{number}", milestone.pull_request_status.label()),
                None if milestone.pull_request_status != super::PullRequestStatus::Unpublished => {
                    format!(" | {}", milestone.pull_request_status.label())
                }
                None => String::new(),
            };
            lines.push(detail_bullet(&format!(
                "{} · {}{}",
                milestone.phase.display_label(),
                milestone.summary,
                pr_suffix
            )));
        }
    }

    if !detail.log_excerpts.is_empty() {
        lines.push(Line::from(""));
        lines.push(section_header("Recent Log Excerpts"));
        for excerpt in &detail.log_excerpts {
            lines.push(detail_bullet(&format!(
                "L{} {}",
                excerpt.line_number, excerpt.text
            )));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Esc/Backspace close detail. PgUp/PgDn scroll.",
        Style::default().fg(Color::DarkGray),
    )));

    Text::from(lines)
}

fn render_footer(frame: &mut Frame<'_>, data: &ListenDashboardData, area: Rect) {
    let direction = if area.width >= 110 {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };
    let chunks = Layout::default()
        .direction(direction)
        .constraints(match direction {
            Direction::Horizontal => vec![Constraint::Percentage(38), Constraint::Percentage(62)],
            Direction::Vertical => vec![Constraint::Length(3), Constraint::Min(3)],
        })
        .split(area);

    let pending_items = if data.pending_issues.is_empty() {
        vec![ListItem::new(Line::from(Span::styled(
            "No queued Todo tickets",
            Style::default().fg(Color::Gray),
        )))]
    } else {
        data.pending_issues
            .iter()
            .map(|issue| {
                let project = issue.project.as_deref().unwrap_or("No project");
                spaced_list_item(vec![
                    Line::from(vec![
                        Span::styled(
                            issue.identifier.clone(),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(" [{}]", issue.team_key),
                            Style::default().fg(Color::Cyan),
                        ),
                    ]),
                    Line::from(Span::styled(
                        format!("{project} · {}", issue.title),
                        Style::default().fg(Color::Gray),
                    )),
                ])
            })
            .collect::<Vec<_>>()
    };
    let pending = List::new(pending_items).block(panel(panel_title("Todo Queue", false)));
    frame.render_widget(pending, chunks[0]);

    let notes = if data.notes.is_empty() {
        "No daemon notes were recorded for this cycle.".to_string()
    } else {
        data.notes.join("\n")
    };
    let notes = Paragraph::new(notes)
        .wrap(Wrap { trim: true })
        .block(panel(panel_title("Notes", false)));
    frame.render_widget(notes, chunks[1]);
}

fn runtime_line(label: &str, value: &str, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), label_style()),
        Span::styled(value.to_string(), value_style(color)),
    ])
}

fn label_style() -> Style {
    Style::default()
        .fg(Color::Gray)
        .add_modifier(Modifier::BOLD)
}

fn value_style(color: Color) -> Style {
    Style::default().fg(color)
}

fn clamp_index(index: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else {
        index.min(len.saturating_sub(1))
    }
}

fn detail_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}: "), label_style()),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ])
}

fn detail_bullet(value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("- ", Style::default().fg(Color::DarkGray)),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ])
}

fn section_header(label: &str) -> Line<'static> {
    Line::from(Span::styled(
        label.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ))
}

fn session_view_badge(
    candidate: SessionListView,
    active_view: SessionListView,
    count: usize,
) -> Span<'static> {
    let style = if candidate == active_view {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD)
    };
    Span::styled(format!(" {} ({count}) ", candidate.label()), style)
}

fn phase_style(phase: SessionPhase) -> Style {
    match phase {
        SessionPhase::Claimed => Style::default().fg(Color::Yellow),
        SessionPhase::BriefReady => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        SessionPhase::Running => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        SessionPhase::Paused => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        SessionPhase::Completed => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        SessionPhase::Blocked => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn snapshot(backend: &TestBackend) -> String {
    let buffer = backend.buffer();
    let mut lines = Vec::new();

    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_string());
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use ratatui::text::Line;

    use super::{
        FocusPane, SessionBrowserAction, SessionBrowserState, detail_scroll_metrics,
        render_active_issue_detail_text, render_dashboard, render_dashboard_with_state,
        render_dashboard_with_view, render_session_detail_text,
    };
    use crate::listen::{
        DashboardRuntimeContext, ListenCycleData, SessionListView, SessionPhase,
        build_dashboard_data,
    };
    use crate::tui::scroll::clamp_offset;

    fn demo_cycle() -> ListenCycleData {
        ListenCycleData::demo(
            Path::new("."),
            ".metastack/agents/sessions/listen-state.json".to_string(),
        )
    }

    #[test]
    fn snapshot_contains_runtime_summary_and_agent_columns() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard(&data, 140, 36).expect("snapshot should render");

        assert!(snapshot.contains("Runtime"));
        assert!(snapshot.contains("Execution agent"));
        assert!(snapshot.contains("codex"));
        assert!(snapshot.contains("Agent Sessions"));
        assert!(snapshot.contains("Active (2)"));
        assert!(snapshot.contains("Completed (0)"));
        assert!(snapshot.contains("PROVIDER"));
        assert!(snapshot.contains("SESSION"));
        assert!(snapshot.contains("PROGRESS"));
        assert!(snapshot.contains("MET-13"));
    }

    #[test]
    fn completed_view_renders_only_completed_sessions() {
        let mut cycle = demo_cycle();
        let mut completed = cycle
            .sessions
            .first()
            .expect("demo cycle should include a session")
            .clone();
        completed.issue_identifier = "MET-99".to_string();
        completed.issue_title = "Completed ticket".to_string();
        completed.phase = SessionPhase::Completed;
        completed.summary = "Complete | moved to `Human Review`".to_string();
        cycle.sessions.push(completed);

        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard_with_view(&data, 140, 36, SessionListView::Completed)
            .expect("completed snapshot should render");

        assert!(snapshot.contains("Completed (1)"));
        assert!(snapshot.contains("MET-99"));
        assert!(!snapshot.contains("MET-17"));
    }

    #[test]
    fn snapshot_keeps_unknown_tokens_and_compact_session_ids() {
        let mut cycle = demo_cycle();
        let session = cycle
            .sessions
            .first_mut()
            .expect("demo cycle should include a session");
        session.tokens = Default::default();
        session.summary = "Progress text stays clean".to_string();

        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard(&data, 140, 36).expect("snapshot should render");

        assert!(snapshot.contains("n/a"));
        assert!(snapshot.contains("019c...e1bf2a"));
        assert!(snapshot.contains("Progress text stays clean"));
    }

    #[test]
    fn snapshot_shows_explicit_runtime_and_session_token_breakdown() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard(&data, 180, 36).expect("snapshot should render");

        assert!(snapshot.contains("Tokens"));
        assert!(snapshot.contains("in 17,995,071 | out 58,080 | total 18,053,151"));
        assert!(snapshot.contains("9,622,232"));
        assert!(!snapshot.contains("in 9,614,112 | out 8,120 | total 9,622,232"));
        assert!(snapshot.contains("draft #321"));
    }

    #[test]
    fn snapshot_keeps_stage_labels_readable_in_medium_width_layout() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard(&data, 140, 36).expect("snapshot should render");

        assert!(snapshot.contains("Brief Ready"));
        assert!(snapshot.contains("draft #321"));
        assert!(snapshot.contains("9,622,232"));
        assert!(snapshot.contains("PROVIDER"));
        assert!(snapshot.contains("Brief ready | backlog MET-14"));
    }

    #[test]
    fn snapshot_surfaces_empty_completed_state_in_compact_layout() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard_with_view(&data, 96, 28, SessionListView::Completed)
            .expect("completed snapshot should render");

        assert!(snapshot.contains("No completed agent sessions are currently tracked."));
    }

    #[test]
    fn snapshot_surfaces_vim_view_switch_hints_when_enabled() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: true,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard(&data, 140, 36).expect("snapshot should render");

        assert!(snapshot.contains("←/→/h/l"));
    }

    #[test]
    fn render_once_surfaces_selected_session_detail_panel() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard_with_state(
            &data,
            200,
            56,
            SessionBrowserState {
                detail_mode: true,
                ..SessionBrowserState::default()
            },
        )
        .expect("detail snapshot should render");

        assert!(snapshot.contains("Selected Session"));
        assert!(snapshot.contains("PR: draft #321"));
        assert!(snapshot.contains("Prompt Context"));
    }

    #[test]
    fn render_once_detail_panel_surfaces_ready_pull_request_status() {
        let mut cycle = demo_cycle();
        let session = cycle
            .sessions
            .first_mut()
            .expect("demo data should include a session");
        session.pull_request.status = crate::listen::PullRequestStatus::Ready;
        let detail = cycle
            .session_details
            .get_mut(&session.issue_identifier)
            .expect("demo data should include detail for the selected session");
        detail.pull_request.status = crate::listen::PullRequestStatus::Ready;
        if let Some(milestone) = detail.milestones.last_mut() {
            milestone.pull_request_status = crate::listen::PullRequestStatus::Ready;
        }

        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let state = SessionBrowserState {
            detail_mode: true,
            ..SessionBrowserState::default()
        };
        let session = state
            .selected_session(&data)
            .expect("demo data should include a selected session");
        let detail = data
            .detail_for_session(&session.issue_identifier)
            .expect("demo data should include detail for the selected session");
        let text = render_session_detail_text(session, detail);
        let rendered = text
            .lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("PR: ready #321"));
        assert!(
            rendered.contains(
                "Brief Ready · Brief ready | backlog MET-14 | worker active | ready #321"
            )
        );
    }

    #[test]
    fn render_once_detail_scroll_reveals_log_excerpts() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard_with_state(
            &data,
            200,
            64,
            SessionBrowserState {
                detail_mode: true,
                detail_scroll: 10,
                ..SessionBrowserState::default()
            },
        )
        .expect("detail snapshot should render");

        assert!(snapshot.contains("Recent Log Excerpts"));
    }

    #[test]
    fn detail_scroll_is_clamped_to_visible_content() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );
        let mut state = SessionBrowserState {
            detail_mode: true,
            detail_scroll: u16::MAX,
            ..SessionBrowserState::default()
        };

        state.normalize(&data);
        state.clamp_detail_scroll(&data, 200, 56);

        let (viewport_height, content_rows) =
            detail_scroll_metrics(&data, &state, 200, 56).expect("detail metrics should exist");
        assert_eq!(
            state.detail_scroll,
            clamp_offset(u16::MAX, viewport_height, content_rows)
        );
    }

    #[test]
    fn selected_session_detail_text_mentions_backspace_close_alias() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let state = SessionBrowserState {
            detail_mode: true,
            ..SessionBrowserState::default()
        };
        let session = state
            .selected_session(&data)
            .expect("demo data should include a selected session");
        let detail = data
            .detail_for_session(&session.issue_identifier)
            .expect("demo data should include detail for the selected session");
        let text = render_session_detail_text(session, detail);
        let rendered = text
            .lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Esc/Backspace close detail. PgUp/PgDn scroll."));
    }

    #[test]
    fn selected_session_detail_text_shows_pull_request_url_without_number() {
        let mut cycle = demo_cycle();
        let session = cycle
            .sessions
            .first_mut()
            .expect("demo data should include a session");
        session.pull_request.number = None;
        let detail = cycle
            .session_details
            .get_mut(&session.issue_identifier)
            .expect("demo data should include detail for the selected session");
        detail.pull_request.number = None;
        detail.pull_request.url =
            Some("https://github.com/metastack-labs/metastack-cli/pull/321".to_string());

        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let state = SessionBrowserState {
            detail_mode: true,
            ..SessionBrowserState::default()
        };
        let session = state
            .selected_session(&data)
            .expect("demo data should include a selected session");
        let detail = data
            .detail_for_session(&session.issue_identifier)
            .expect("demo data should include detail for the selected session");
        let text = render_session_detail_text(session, detail);
        let rendered = text
            .lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            rendered.contains("PR URL: https://github.com/metastack-labs/metastack-cli/pull/321")
        );
    }

    #[test]
    fn selected_session_detail_text_shows_pull_request_ref_without_url() {
        let mut cycle = demo_cycle();
        let session = cycle
            .sessions
            .first_mut()
            .expect("demo data should include a session");
        session.pull_request.url = None;
        let detail = cycle
            .session_details
            .get_mut(&session.issue_identifier)
            .expect("demo data should include detail for the selected session");
        detail.pull_request.url = None;

        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let state = SessionBrowserState {
            detail_mode: true,
            ..SessionBrowserState::default()
        };
        let session = state
            .selected_session(&data)
            .expect("demo data should include a selected session");
        let detail = data
            .detail_for_session(&session.issue_identifier)
            .expect("demo data should include detail for the selected session");
        let text = render_session_detail_text(session, detail);
        let rendered = text
            .lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("PR Ref: #321"));
        assert!(!rendered.contains("PR URL:"));
    }

    #[test]
    fn snapshot_renders_active_issues_pane_with_demo_data() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard(&data, 140, 48).expect("snapshot should render");

        assert!(snapshot.contains("In Progress Issues - All Users"));
        assert!(snapshot.contains("MET-22"));
        assert!(snapshot.contains("MET-25"));
        assert!(snapshot.contains("MET-30"));
        assert!(snapshot.contains("Alice Chen"));
        assert!(snapshot.contains("Bob Taylor"));
        assert!(snapshot.contains("unassigned"));
        assert!(snapshot.contains("PR"));
    }

    #[test]
    fn active_issues_pane_hidden_when_config_disabled() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: false,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard(&data, 140, 48).expect("snapshot should render");

        assert!(!snapshot.contains("In Progress Issues - All Users"));
        assert!(snapshot.contains("Agent Sessions"));
    }

    #[test]
    fn tab_toggles_focus_between_sessions_and_active_issues() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let mut state = SessionBrowserState::default();
        assert_eq!(state.focus, FocusPane::Sessions);

        state.apply_action(&data, SessionBrowserAction::Tab);
        assert_eq!(state.focus, FocusPane::ActiveIssues);

        state.apply_action(&data, SessionBrowserAction::Tab);
        assert_eq!(state.focus, FocusPane::Sessions);
    }

    #[test]
    fn tab_toggles_session_view_when_active_issues_disabled() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: false,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let mut state = SessionBrowserState::default();
        assert_eq!(state.view, SessionListView::Active);

        state.apply_action(&data, SessionBrowserAction::Tab);
        assert_eq!(state.view, SessionListView::Completed);
        assert_eq!(state.focus, FocusPane::Sessions);
    }

    #[test]
    fn active_issue_selection_navigates_with_up_down() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let mut state = SessionBrowserState {
            focus: FocusPane::ActiveIssues,
            ..SessionBrowserState::default()
        };
        assert_eq!(state.selected_active_issue, 0);

        state.apply_action(&data, SessionBrowserAction::Down);
        assert_eq!(state.selected_active_issue, 1);

        state.apply_action(&data, SessionBrowserAction::Down);
        assert_eq!(state.selected_active_issue, 2);

        state.apply_action(&data, SessionBrowserAction::Up);
        assert_eq!(state.selected_active_issue, 1);
    }

    #[test]
    fn active_issue_enter_opens_detail_when_preview_enabled() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let mut state = SessionBrowserState {
            focus: FocusPane::ActiveIssues,
            ..SessionBrowserState::default()
        };
        state.apply_action(&data, SessionBrowserAction::Enter);
        assert!(state.detail_mode);
    }

    #[test]
    fn active_issue_detail_pane_renders_description() {
        let cycle = demo_cycle();
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_label: "terminal dashboard (TUI)",
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                vim_mode: false,
                show_active_issues: true,
                show_preview: true,
                resolved_agent: Some("codex".to_string()),
            },
        );

        let snapshot = render_dashboard_with_state(
            &data,
            200,
            56,
            SessionBrowserState {
                focus: FocusPane::ActiveIssues,
                detail_mode: true,
                ..SessionBrowserState::default()
            },
        )
        .expect("active issue detail snapshot should render");

        assert!(snapshot.contains("Issue Detail"));
        assert!(snapshot.contains("MET-22"));
        assert!(snapshot.contains("Description"));
    }

    #[test]
    fn active_issue_detail_text_includes_pr_url_and_assignee() {
        use crate::listen::ActiveIssue;

        let issue = ActiveIssue {
            identifier: "MET-42".to_string(),
            title: "Test issue".to_string(),
            assignee: Some("Charlie".to_string()),
            state_name: "In Progress".to_string(),
            has_open_pr: true,
            pr_url: Some("https://github.com/org/repo/pull/99".to_string()),
            description: Some("A detailed description".to_string()),
            url: "https://linear.app/issues/MET-42".to_string(),
            team_key: "MET".to_string(),
            project: Some("MyProject".to_string()),
        };

        let text = render_active_issue_detail_text(&issue);
        let rendered = text
            .lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Assignee: Charlie"));
        assert!(rendered.contains("PR: https://github.com/org/repo/pull/99"));
        assert!(rendered.contains("Project: MyProject"));
        assert!(rendered.contains("A detailed description"));
    }

    #[test]
    fn active_issue_detail_text_handles_no_assignee_no_pr() {
        use crate::listen::ActiveIssue;

        let issue = ActiveIssue {
            identifier: "MET-43".to_string(),
            title: "Minimal issue".to_string(),
            assignee: None,
            state_name: "In Progress".to_string(),
            has_open_pr: false,
            pr_url: None,
            description: None,
            url: "https://linear.app/issues/MET-43".to_string(),
            team_key: "MET".to_string(),
            project: None,
        };

        let text = render_active_issue_detail_text(&issue);
        let rendered = text
            .lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Assignee: unassigned"));
        assert!(rendered.contains("PR: none"));
        assert!(!rendered.contains("Description"));
        assert!(!rendered.contains("Project:"));
    }
}

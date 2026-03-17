use anyhow::Result;
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, Wrap};
use ratatui::{Frame, Terminal};

use super::{ListenDashboardData, SessionListView, SessionPhase};

pub fn render_dashboard(data: &ListenDashboardData, width: u16, height: u16) -> Result<String> {
    render_dashboard_with_view(data, width, height, SessionListView::Active)
}

fn render_dashboard_with_view(
    data: &ListenDashboardData,
    width: u16,
    height: u16,
    view: SessionListView,
) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render(frame, data, view))?;
    Ok(snapshot(terminal.backend()))
}

pub(crate) fn render(frame: &mut Frame<'_>, data: &ListenDashboardData, view: SessionListView) {
    let area = frame.area();
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

    render_header(frame, data, sections[0]);
    render_sessions(frame, data, sections[1], view);

    if footer_height > 0 {
        render_footer(frame, data, sections[2]);
    }
}

fn render_header(frame: &mut Frame<'_>, data: &ListenDashboardData, area: Rect) {
    if area.width < 110 {
        let compact = Paragraph::new(Text::from(vec![
            Line::from(Span::styled(
                data.title.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                data.scope.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                data.cycle_summary.clone(),
                Style::default().fg(Color::Gray),
            )),
            runtime_line("Agents", &data.runtime.agents, Color::Green),
            runtime_line("Throughput", &data.runtime.throughput, Color::Cyan),
            runtime_line("Runtime", &data.runtime.runtime, Color::Yellow),
            runtime_line("Tokens", &data.runtime.tokens, Color::Magenta),
            runtime_line("Rate Limits", &data.runtime.rate_limits, Color::LightBlue),
            runtime_line("Dashboard", &data.runtime.dashboard, Color::LightCyan),
            runtime_line(
                "Dashboard refresh",
                &data.runtime.dashboard_refresh,
                Color::Yellow,
            ),
            runtime_line(
                "Linear refresh",
                &data.runtime.linear_refresh,
                Color::LightYellow,
            ),
        ]))
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title("Listen Status"),
        );
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
        Line::from(Span::styled(
            data.title.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            data.scope.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            data.cycle_summary.clone(),
            Style::default().fg(Color::Gray),
        )),
        Line::from(vec![
            Span::styled("State file: ", label_style()),
            Span::styled(data.state_file.clone(), value_style(Color::Green)),
        ]),
    ]))
    .wrap(Wrap { trim: true })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title("Listen Status"),
    );
    frame.render_widget(hero, chunks[0]);

    let runtime_lines = vec![
        runtime_line("Agents", &data.runtime.agents, Color::Green),
        runtime_line("Throughput", &data.runtime.throughput, Color::Cyan),
        runtime_line("Runtime", &data.runtime.runtime, Color::Yellow),
        runtime_line("Tokens", &data.runtime.tokens, Color::Magenta),
        runtime_line("Rate Limits", &data.runtime.rate_limits, Color::LightBlue),
        runtime_line("Project", &data.runtime.project, Color::White),
        runtime_line("Dashboard", &data.runtime.dashboard, Color::LightCyan),
        runtime_line(
            "Dashboard refresh",
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
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray))
                .title("Runtime"),
        );
    frame.render_widget(runtime, chunks[1]);
}

fn render_sessions(
    frame: &mut Frame<'_>,
    data: &ListenDashboardData,
    area: Rect,
    view: SessionListView,
) {
    let counts = data.session_counts();
    let sessions = data.sessions_for_view(view);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title("Agent Sessions");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(2), Constraint::Min(0)])
        .split(inner);

    let controls = Paragraph::new(Line::from(vec![
        Span::styled("Views: ", label_style()),
        session_view_badge(SessionListView::Active, view, counts.active),
        Span::raw(" "),
        session_view_badge(SessionListView::Completed, view, counts.completed),
        Span::styled("  Tab/←/→ toggles", Style::default().fg(Color::DarkGray)),
    ]))
    .wrap(Wrap { trim: true });
    frame.render_widget(controls, sections[0]);

    if sessions.is_empty() {
        let empty = Paragraph::new(match view {
            SessionListView::Active => "No active agent sessions are currently tracked.",
            SessionListView::Completed => "No completed agent sessions are currently tracked.",
        })
        .wrap(Wrap { trim: true });
        frame.render_widget(empty, sections[1]);
        return;
    }

    let rows = sessions.into_iter().map(|session| {
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
            Cell::from(Text::from(issue_lines)),
            Cell::from(Span::styled(
                session.stage_label(),
                phase_style(session.phase),
            )),
            Cell::from(session.pid_label()),
            Cell::from(session.age_label(data.runtime.current_epoch_seconds)),
            Cell::from(session.tokens_label()),
            Cell::from(session.session_label()),
            Cell::from(session.summary.clone()),
        ])
    });
    let header = Row::new(vec![
        Cell::from("ID"),
        Cell::from("STAGE"),
        Cell::from("PID"),
        Cell::from("AGE"),
        Cell::from("TOKENS"),
        Cell::from("SESSION"),
        Cell::from("PROGRESS"),
    ])
    .style(
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    )
    .bottom_margin(1);
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(13),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Min(24),
        ],
    )
    .header(header)
    .column_spacing(1);
    frame.render_widget(table, sections[1]);
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
                ListItem::new(vec![
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
    let pending = List::new(pending_items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title("Todo Queue"),
    );
    frame.render_widget(pending, chunks[0]);

    let notes = if data.notes.is_empty() {
        "No daemon notes were recorded for this cycle.".to_string()
    } else {
        data.notes.join("\n")
    };
    let notes = Paragraph::new(notes).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title("Notes"),
    );
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

    use super::{render_dashboard, render_dashboard_with_view};
    use crate::listen::{
        DashboardRuntimeContext, ListenCycleData, SessionListView, SessionPhase,
        build_dashboard_data,
    };

    #[test]
    fn snapshot_contains_runtime_summary_and_agent_columns() {
        let cycle = ListenCycleData::demo(Path::new("."));
        let data = build_dashboard_data(
            &cycle,
            &DashboardRuntimeContext {
                started_at_epoch_seconds: 1_773_568_249,
                now_epoch_seconds: 1_773_575_600,
                poll_interval_seconds: 7,
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                dashboard_url: Some("http://127.0.0.1:4000/".to_string()),
            },
        );

        let snapshot = render_dashboard(&data, 140, 36).expect("snapshot should render");

        assert!(snapshot.contains("Runtime"));
        assert!(snapshot.contains("Agent Sessions"));
        assert!(snapshot.contains("Active (2)"));
        assert!(snapshot.contains("Completed (0)"));
        assert!(snapshot.contains("SESSION"));
        assert!(snapshot.contains("PROGRESS"));
        assert!(snapshot.contains("MET-13"));
    }

    #[test]
    fn completed_view_renders_only_completed_sessions() {
        let mut cycle = ListenCycleData::demo(Path::new("."));
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
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                dashboard_url: Some("http://127.0.0.1:4000/".to_string()),
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
        let mut cycle = ListenCycleData::demo(Path::new("."));
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
                dashboard_refresh_seconds: 1,
                linear_refresh_seconds: 15,
                dashboard_url: Some("http://127.0.0.1:4000/".to_string()),
            },
        );

        let snapshot = render_dashboard(&data, 140, 36).expect("snapshot should render");

        assert!(snapshot.contains("n/a"));
        assert!(snapshot.contains("019c...e1bf2a"));
        assert!(snapshot.contains("Progress text stays clean"));
    }
}

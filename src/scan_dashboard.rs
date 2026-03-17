use std::io::{self, IsTerminal};

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
#[cfg(test)]
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanItemState {
    Pending,
    Running,
    Complete,
    Failed,
}

impl ScanItemState {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Complete => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ScanDashboardRow {
    pub(crate) label: String,
    pub(crate) detail: String,
    pub(crate) state: ScanItemState,
}

#[derive(Debug, Clone)]
pub(crate) struct ScanDashboardData {
    pub(crate) title: String,
    pub(crate) status_line: String,
    pub(crate) steps: Vec<ScanDashboardRow>,
    pub(crate) files: Vec<ScanDashboardRow>,
    pub(crate) log_path: String,
}

pub(crate) struct ScanDashboard {
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
}

impl ScanDashboard {
    pub(crate) fn start() -> Result<Self> {
        if !io::stdout().is_terminal() {
            return Ok(Self { terminal: None });
        }

        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide)?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self {
            terminal: Some(terminal),
        })
    }

    pub(crate) fn draw(&mut self, data: &ScanDashboardData) -> Result<()> {
        let Some(terminal) = self.terminal.as_mut() else {
            return Ok(());
        };

        terminal.draw(|frame| render_dashboard(frame, data))?;
        Ok(())
    }
}

impl Drop for ScanDashboard {
    fn drop(&mut self) {
        let Some(terminal) = self.terminal.as_mut() else {
            return;
        };

        let _ = execute!(terminal.backend_mut(), Show, LeaveAlternateScreen);
    }
}

#[cfg(test)]
pub(crate) fn render_snapshot(data: &ScanDashboardData, width: u16, height: u16) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render_dashboard(frame, data))?;
    Ok(snapshot(terminal.backend()))
}

fn render_dashboard(frame: &mut Frame<'_>, data: &ScanDashboardData) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(outer[1]);

    let header = Paragraph::new(Text::from(vec![
        Line::from(data.title.clone()),
        Line::from(data.status_line.clone()),
        Line::from("Live agent output is hidden while the scan runs."),
        Line::from("The generated file list updates as each codebase document lands."),
    ]))
    .wrap(Wrap { trim: true })
    .block(Block::default().borders(Borders::ALL).title("meta scan"));
    frame.render_widget(header, outer[0]);

    let step_items = data
        .steps
        .iter()
        .map(render_row)
        .collect::<Vec<ListItem<'static>>>();
    let step_list = List::new(step_items)
        .block(Block::default().borders(Borders::ALL).title("Steps"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(step_list, body[0]);

    let file_items = data
        .files
        .iter()
        .map(render_row)
        .collect::<Vec<ListItem<'static>>>();
    let file_list = List::new(file_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Generated Files"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(file_list, body[1]);

    let footer = Paragraph::new(format!(
        "Agent log: {} (kept out of the main scan UI; inspect only for failures)",
        data.log_path
    ))
    .wrap(Wrap { trim: true })
    .block(Block::default().borders(Borders::ALL).title("Notes"));
    frame.render_widget(footer, outer[2]);
}

fn render_row(row: &ScanDashboardRow) -> ListItem<'static> {
    ListItem::new(Text::from(vec![
        Line::from(format!("[{}] {}", row.state.label(), row.label)),
        Line::from(row.detail.clone()),
    ]))
}

#[cfg(test)]
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
    use super::*;

    #[test]
    fn scan_dashboard_snapshot_surfaces_steps_and_generated_files() {
        let data = ScanDashboardData {
            title: "Codebase scan for demo-cli".to_string(),
            status_line: "Refreshing reusable planning docs".to_string(),
            steps: vec![
                ScanDashboardRow {
                    label: "Collect repository facts".to_string(),
                    detail: ".metastack/codebase/SCAN.md is ready".to_string(),
                    state: ScanItemState::Complete,
                },
                ScanDashboardRow {
                    label: "Refresh codebase docs with `scan-stub`".to_string(),
                    detail: "Waiting for the remaining planning documents".to_string(),
                    state: ScanItemState::Running,
                },
            ],
            files: vec![
                ScanDashboardRow {
                    label: ".metastack/codebase/SCAN.md".to_string(),
                    detail: "deterministic fact base".to_string(),
                    state: ScanItemState::Complete,
                },
                ScanDashboardRow {
                    label: ".metastack/codebase/ARCHITECTURE.md".to_string(),
                    detail: "agent-authored context".to_string(),
                    state: ScanItemState::Running,
                },
                ScanDashboardRow {
                    label: ".metastack/codebase/TESTING.md".to_string(),
                    detail: "agent-authored context".to_string(),
                    state: ScanItemState::Pending,
                },
            ],
            log_path: ".metastack/agents/sessions/scan.log".to_string(),
        };

        let snapshot = render_snapshot(&data, 120, 28).expect("snapshot should render");

        assert!(snapshot.contains("meta scan"));
        assert!(snapshot.contains("Collect repository facts"));
        assert!(snapshot.contains("Refresh codebase docs with `scan-stub`"));
        assert!(snapshot.contains(".metastack/codebase/ARCHITECTURE.md"));
        assert!(snapshot.contains("Agent log: .metastack/agents/sessions/scan.log"));
    }
}

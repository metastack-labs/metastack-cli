use std::io::{self, IsTerminal};

use anyhow::Result;
use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
#[cfg(test)]
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::ListItem;
use ratatui::{Frame, Terminal};

use crate::tui::theme::{Tone, badge, empty_state, key_hints, list, panel_title, paragraph};

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
    let narrow = frame.area().width < 100;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if narrow { 6 } else { 5 }),
            Constraint::Min(10),
            Constraint::Length(if narrow { 4 } else { 3 }),
        ])
        .split(frame.area());
    let body = Layout::default()
        .direction(if narrow {
            Direction::Vertical
        } else {
            Direction::Horizontal
        })
        .constraints(if narrow {
            vec![Constraint::Percentage(45), Constraint::Percentage(55)]
        } else {
            vec![Constraint::Percentage(42), Constraint::Percentage(58)]
        })
        .split(outer[1]);

    let header = paragraph(
        Text::from(vec![
            Line::from(data.title.clone()),
            Line::from(data.status_line.clone()),
            Line::from("Live agent output stays hidden while the scan runs."),
            key_hints(&[("Ctrl-C", "exit"), ("Files", "update live")]),
        ]),
        panel_title("meta scan", false),
    );
    frame.render_widget(header, outer[0]);

    let step_items = if data.steps.is_empty() {
        vec![ListItem::new(empty_state(
            "No scan phases have reported yet.",
            "The first agent update will populate this list.",
        ))]
    } else {
        data.steps
            .iter()
            .map(render_row)
            .collect::<Vec<ListItem<'static>>>()
    };
    let step_list = list(step_items, panel_title("Steps", false));
    frame.render_widget(step_list, body[0]);

    let file_items = if data.files.is_empty() {
        vec![ListItem::new(empty_state(
            "No generated files have landed yet.",
            "The file list will populate as each scan artifact is written.",
        ))]
    } else {
        data.files
            .iter()
            .map(render_row)
            .collect::<Vec<ListItem<'static>>>()
    };
    let file_list = list(file_items, panel_title("Generated Files", false));
    frame.render_widget(file_list, body[1]);

    let footer = paragraph(
        format!(
            "Agent log: {} (kept out of the main scan UI; inspect only for failures)",
            data.log_path
        ),
        panel_title("Notes", false),
    );
    frame.render_widget(footer, outer[2]);
}

fn render_row(row: &ScanDashboardRow) -> ListItem<'static> {
    ListItem::new(Text::from(vec![
        Line::from(vec![
            badge(row.state.label(), scan_row_tone(row.state)),
            Span::raw(" "),
            Span::raw(row.label.clone()),
        ]),
        Line::from(row.detail.clone()),
    ]))
}

fn scan_row_tone(state: ScanItemState) -> Tone {
    match state {
        ScanItemState::Pending => Tone::Muted,
        ScanItemState::Running => Tone::Info,
        ScanItemState::Complete => Tone::Success,
        ScanItemState::Failed => Tone::Danger,
    }
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

    #[test]
    fn scan_dashboard_snapshot_handles_empty_narrow_state() {
        let data = ScanDashboardData {
            title: "Codebase scan for demo-cli".to_string(),
            status_line: "Waiting for the first agent update".to_string(),
            steps: Vec::new(),
            files: Vec::new(),
            log_path: ".metastack/agents/sessions/scan.log".to_string(),
        };

        let snapshot = render_snapshot(&data, 84, 28).expect("snapshot should render");

        assert!(snapshot.contains("No scan phases have reported yet."));
        assert!(snapshot.contains("No generated files have landed yet."));
    }
}

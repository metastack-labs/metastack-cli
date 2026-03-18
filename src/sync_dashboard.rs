use std::io::{self, IsTerminal};
use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::backlog::BacklogSyncStatus;
use crate::linear::IssueSummary;

#[derive(Debug, Clone)]
pub struct SyncDashboardData {
    pub title: String,
    pub issues: Vec<SyncDashboardIssue>,
}

#[derive(Debug, Clone)]
pub struct SyncDashboardIssue {
    pub issue: IssueSummary,
    pub local_status: BacklogSyncStatus,
}

#[derive(Debug, Clone)]
pub struct SyncDashboardOptions {
    pub render_once: bool,
    pub width: u16,
    pub height: u16,
    pub actions: Vec<SyncDashboardAction>,
}

#[derive(Debug, Clone, Copy)]
pub enum SyncDashboardAction {
    Up,
    Down,
    Enter,
    Back,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncSelectionAction {
    Pull,
    Push,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSelection {
    pub issue_identifier: String,
    pub action: SyncSelectionAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncDashboardExit {
    Snapshot(String),
    Cancelled,
    Selected(SyncSelection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Issues,
    Actions,
}

#[derive(Debug, Clone)]
struct SyncDashboardApp {
    data: SyncDashboardData,
    focus: Focus,
    issue_index: usize,
    action_index: usize,
    completed: Option<SyncSelection>,
}

const ACTIONS: [SyncSelectionAction; 2] = [SyncSelectionAction::Pull, SyncSelectionAction::Push];

pub fn run_sync_dashboard(
    data: SyncDashboardData,
    options: SyncDashboardOptions,
) -> Result<SyncDashboardExit> {
    if options.render_once {
        return render_once(data, options).map(SyncDashboardExit::Snapshot);
    }

    if !io::stdout().is_terminal() {
        bail!(
            "the interactive sync dashboard requires a TTY; use `meta sync pull <ISSUE>` or `meta sync push <ISSUE>` for scripted runs"
        );
    }

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = SyncDashboardApp::new(data);

    loop {
        terminal.draw(|frame| render_dashboard(frame, &app))?;

        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let action = match key.code {
                KeyCode::Char('q') => return Ok(SyncDashboardExit::Cancelled),
                KeyCode::Up => Some(SyncDashboardAction::Up),
                KeyCode::Down => Some(SyncDashboardAction::Down),
                KeyCode::Enter => Some(SyncDashboardAction::Enter),
                KeyCode::Esc | KeyCode::Backspace => Some(SyncDashboardAction::Back),
                _ => None,
            };

            if let Some(action) = action
                && let Some(selection) = app.apply(action)
            {
                return Ok(SyncDashboardExit::Selected(selection));
            }
        }
    }
}

fn render_once(data: SyncDashboardData, options: SyncDashboardOptions) -> Result<String> {
    let backend = TestBackend::new(options.width, options.height);
    let mut terminal = Terminal::new(backend)?;
    let mut app = SyncDashboardApp::new(data);
    for action in options.actions {
        let _ = app.apply(action);
    }

    terminal.draw(|frame| render_dashboard(frame, &app))?;
    Ok(snapshot(terminal.backend()))
}

fn render_dashboard(frame: &mut Frame<'_>, app: &SyncDashboardApp) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(outer[1]);
    let details = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(58),
            Constraint::Length(8),
            Constraint::Min(6),
        ])
        .split(body[1]);

    let header = Paragraph::new(Text::from(vec![
        Line::from(app.data.title.clone()),
        Line::from(app.summary_line()),
        Line::from("Keys: Up/Down moves selection, Enter advances, Esc goes back, q exits"),
    ]))
    .wrap(Wrap { trim: true })
    .block(Block::default().borders(Borders::ALL).title("meta sync"));
    frame.render_widget(header, outer[0]);

    render_issue_list(frame, body[0], app);
    render_issue_preview(frame, details[0], app);
    render_action_list(frame, details[1], app);
    render_status(frame, details[2], app);
}

fn render_issue_list(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &SyncDashboardApp) {
    let title = if app.focus == Focus::Issues {
        format!("Project Issues [focus] ({})", app.data.issues.len())
    } else {
        format!("Project Issues ({})", app.data.issues.len())
    };
    let items = if app.data.issues.is_empty() {
        vec![ListItem::new(
            "No issues found for the configured default project.",
        )]
    } else {
        app.data
            .issues
            .iter()
            .map(render_issue_list_item)
            .collect::<Vec<_>>()
    };

    let mut state = ListState::default();
    if app.data.issues.is_empty() {
        state.select(Some(0));
    } else {
        state.select(Some(app.issue_index.min(app.data.issues.len() - 1)));
    }

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_issue_preview(
    frame: &mut Frame<'_>,
    area: ratatui::layout::Rect,
    app: &SyncDashboardApp,
) {
    let preview = Paragraph::new(app.preview_text())
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Issue Preview"),
        );
    frame.render_widget(preview, area);
}

fn render_action_list(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &SyncDashboardApp) {
    let title = if app.focus == Focus::Actions {
        "Sync Action [focus]"
    } else {
        "Sync Action"
    };

    let items = ACTIONS
        .iter()
        .map(|action| {
            ListItem::new(Text::from(vec![
                Line::from(action.label()),
                Line::from(action.description()),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(app.action_index.min(ACTIONS.len() - 1)));

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_status(frame: &mut Frame<'_>, area: ratatui::layout::Rect, app: &SyncDashboardApp) {
    let status = Paragraph::new(app.status_text())
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Selection"));
    frame.render_widget(status, area);
}

fn render_issue_list_item(issue: &SyncDashboardIssue) -> ListItem<'static> {
    ListItem::new(Text::from(vec![
        Line::from(format!("{}  {}", issue.issue.identifier, issue.issue.title)),
        Line::from(format!(
            "{} • local: {} • {}",
            issue_state_label(&issue.issue),
            issue.local_status.as_str(),
            issue.issue.updated_at
        )),
    ]))
}

impl SyncDashboardApp {
    fn new(data: SyncDashboardData) -> Self {
        Self {
            data,
            focus: Focus::Issues,
            issue_index: 0,
            action_index: 0,
            completed: None,
        }
    }

    fn apply(&mut self, action: SyncDashboardAction) -> Option<SyncSelection> {
        self.completed = None;

        match action {
            SyncDashboardAction::Up => match self.focus {
                Focus::Issues => shift_index(&mut self.issue_index, self.data.issues.len(), -1),
                Focus::Actions => shift_index(&mut self.action_index, ACTIONS.len(), -1),
            },
            SyncDashboardAction::Down => match self.focus {
                Focus::Issues => shift_index(&mut self.issue_index, self.data.issues.len(), 1),
                Focus::Actions => shift_index(&mut self.action_index, ACTIONS.len(), 1),
            },
            SyncDashboardAction::Back => {
                if self.focus == Focus::Actions {
                    self.focus = Focus::Issues;
                    self.action_index = 0;
                }
            }
            SyncDashboardAction::Enter => match self.focus {
                Focus::Issues => {
                    if self.selected_issue().is_some() {
                        self.focus = Focus::Actions;
                    }
                }
                Focus::Actions => {
                    let selection = SyncSelection {
                        issue_identifier: self.selected_issue()?.issue.identifier.clone(),
                        action: ACTIONS[self.action_index],
                    };
                    self.completed = Some(selection.clone());
                    return Some(selection);
                }
            },
        }

        None
    }

    fn selected_issue(&self) -> Option<&SyncDashboardIssue> {
        self.data.issues.get(self.issue_index)
    }

    fn summary_line(&self) -> String {
        if let Some(selection) = &self.completed {
            return format!(
                "Ready to {} {}",
                selection.action.verb(),
                selection.issue_identifier
            );
        }

        match self.focus {
            Focus::Issues => {
                if self.data.issues.is_empty() {
                    "No issues matched the configured default project.".to_string()
                } else {
                    format!(
                        "{} issues loaded from the repo default project. Press Enter to choose push or pull.",
                        self.data.issues.len()
                    )
                }
            }
            Focus::Actions => match self.selected_issue() {
                Some(issue) => {
                    format!("Choose whether to pull or push {}.", issue.issue.identifier)
                }
                None => "No issue is available to sync.".to_string(),
            },
        }
    }

    fn preview_text(&self) -> String {
        let Some(issue) = self.selected_issue() else {
            return "No issue is available for the configured default project.".to_string();
        };

        let project = issue
            .issue
            .project
            .as_ref()
            .map(|project| project.name.clone())
            .unwrap_or_else(|| "No project".to_string());
        let description = issue
            .issue
            .description
            .as_deref()
            .filter(|description| !description.trim().is_empty())
            .unwrap_or("No description provided.");

        format!(
            "{}\n{}\n\nProject: {project}\nState: {}\nLocal sync: {}\nUpdated: {}\n\n{}",
            issue.issue.identifier,
            issue.issue.title,
            issue_state_label(&issue.issue),
            issue.local_status.as_str(),
            issue.issue.updated_at,
            description,
        )
    }

    fn status_text(&self) -> String {
        if let Some(selection) = &self.completed {
            return format!(
                "Ready to {} {}.\nRender-once stops at the chosen state; interactive mode executes this selection immediately.",
                selection.action.verb(),
                selection.issue_identifier,
            );
        }

        match self.focus {
            Focus::Issues => {
                if self.data.issues.is_empty() {
                    "Configure `.metastack/meta.json` with a default project that has Linear issues, then rerun `meta sync`."
                        .to_string()
                } else {
                    "Step 1 of 2: choose an issue from the default project list.".to_string()
                }
            }
            Focus::Actions => "Step 2 of 2: choose pull to refresh local files or push to sync managed attachments. `index.md` only updates the Linear description when you run push with `--update-description`.".to_string(),
        }
    }
}

impl SyncSelectionAction {
    fn label(self) -> &'static str {
        match self {
            Self::Pull => "Pull",
            Self::Push => "Push",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Pull => "Refresh `.metastack/backlog/<ISSUE>/` from the Linear issue.",
            Self::Push => {
                "Sync CLI-managed attachment files; `index.md` stays local unless you run `meta backlog sync push <ISSUE> --update-description`."
            }
        }
    }

    fn verb(self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Push => "push",
        }
    }
}

fn issue_state_label(issue: &IssueSummary) -> String {
    issue
        .state
        .as_ref()
        .map(|state| state.name.clone())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn shift_index(index: &mut usize, len: usize, delta: isize) {
    if len == 0 {
        *index = 0;
        return;
    }

    let mut next = *index as isize + delta;
    if next < 0 {
        next = len.saturating_sub(1) as isize;
    } else if next >= len as isize {
        next = 0;
    }
    *index = next as usize;
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

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Focus, SyncDashboardAction, SyncDashboardApp, SyncDashboardData, SyncDashboardExit,
        SyncDashboardIssue, SyncDashboardOptions, run_sync_dashboard,
    };
    use crate::backlog::BacklogSyncStatus;
    use crate::linear::DashboardData;

    fn demo_data() -> SyncDashboardData {
        let demo = DashboardData::demo();
        SyncDashboardData {
            title: demo.title,
            issues: demo
                .issues
                .into_iter()
                .enumerate()
                .map(|(index, issue)| SyncDashboardIssue {
                    issue,
                    local_status: match index {
                        0 => BacklogSyncStatus::Synced,
                        1 => BacklogSyncStatus::Diverged,
                        _ => BacklogSyncStatus::Unlinked,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn render_once_previews_selected_sync_action() {
        let exit = run_sync_dashboard(
            demo_data(),
            SyncDashboardOptions {
                render_once: true,
                width: 120,
                height: 32,
                actions: vec![
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Enter,
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Enter,
                ],
            },
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };
        assert!(snapshot.contains("Ready to push MET-12"));
        assert!(snapshot.contains("Sync Action [focus]"));
        assert!(snapshot.contains("local: diverged"));
        assert!(snapshot.contains("Local sync: diverged"));
    }

    #[test]
    fn back_returns_focus_to_issue_list() {
        let mut app = SyncDashboardApp::new(demo_data());

        assert_eq!(app.focus, Focus::Issues);
        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Actions);
        app.apply(SyncDashboardAction::Back);
        assert_eq!(app.focus, Focus::Issues);
    }
}

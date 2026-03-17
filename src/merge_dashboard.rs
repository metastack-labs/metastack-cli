use std::collections::BTreeSet;
use std::io::{self, IsTerminal};
use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeDashboardPullRequest {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub head_ref: String,
    pub updated_at: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeDashboardData {
    pub title: String,
    pub repo_label: String,
    pub base_branch: String,
    pub pull_requests: Vec<MergeDashboardPullRequest>,
}

#[derive(Debug, Clone)]
pub struct MergeDashboardOptions {
    pub render_once: bool,
    pub width: u16,
    pub height: u16,
    pub actions: Vec<MergeDashboardAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeDashboardAction {
    Up,
    Down,
    Toggle,
    Enter,
    Back,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeDashboardExit {
    Snapshot(String),
    Cancelled,
    Selected(Vec<u64>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    PullRequests,
    Confirm,
}

#[derive(Debug, Clone)]
struct MergeDashboardApp {
    data: MergeDashboardData,
    focus: Focus,
    pr_index: usize,
    selected: BTreeSet<usize>,
    completed: Option<Vec<u64>>,
}

pub fn run_merge_dashboard(
    data: MergeDashboardData,
    options: MergeDashboardOptions,
) -> Result<MergeDashboardExit> {
    if options.render_once {
        return render_once(data, options).map(MergeDashboardExit::Snapshot);
    }

    if !io::stdout().is_terminal() {
        bail!(
            "the interactive merge dashboard requires a TTY; use `meta merge --json` for discovery or `meta merge --no-interactive --pull-request <NUMBER>` for scripted runs"
        );
    }

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = MergeDashboardApp::new(data);

    loop {
        terminal.draw(|frame| render_dashboard(frame, &app))?;

        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let action = match key.code {
                KeyCode::Char('q') => return Ok(MergeDashboardExit::Cancelled),
                KeyCode::Up => Some(MergeDashboardAction::Up),
                KeyCode::Down => Some(MergeDashboardAction::Down),
                KeyCode::Char(' ') => Some(MergeDashboardAction::Toggle),
                KeyCode::Enter => Some(MergeDashboardAction::Enter),
                KeyCode::Esc | KeyCode::Backspace => Some(MergeDashboardAction::Back),
                _ => None,
            };

            if let Some(action) = action
                && let Some(selection) = app.apply(action)
            {
                return Ok(MergeDashboardExit::Selected(selection));
            }
        }
    }
}

fn render_once(data: MergeDashboardData, options: MergeDashboardOptions) -> Result<String> {
    let backend = TestBackend::new(options.width, options.height);
    let mut terminal = Terminal::new(backend)?;
    let mut app = MergeDashboardApp::new(data);

    for action in options.actions {
        if let Some(selection) = app.apply(action) {
            app.completed = Some(selection);
            break;
        }
    }

    terminal.draw(|frame| render_dashboard(frame, &app))?;
    Ok(snapshot(terminal.backend()))
}

fn render_dashboard(frame: &mut Frame<'_>, app: &MergeDashboardApp) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(0)])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(outer[1]);
    let sidebar = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(0)])
        .split(body[1]);

    let header = Paragraph::new(Text::from(vec![
        Line::from(app.data.title.clone()),
        Line::from(app.summary_line()),
        Line::from(
            "Keys: Up/Down moves, Space selects PRs, Enter advances, Esc goes back, q exits",
        ),
    ]))
    .wrap(Wrap { trim: true })
    .block(Block::default().borders(Borders::ALL).title("meta merge"));
    frame.render_widget(header, outer[0]);

    render_pr_list(frame, body[0], app);
    render_selection_summary(frame, sidebar[0], app);
    render_details(frame, sidebar[1], app);
}

fn render_pr_list(frame: &mut Frame<'_>, area: Rect, app: &MergeDashboardApp) {
    let title = if app.focus == Focus::PullRequests {
        format!(
            "Open Pull Requests [focus] ({})",
            app.data.pull_requests.len()
        )
    } else {
        format!("Open Pull Requests ({})", app.data.pull_requests.len())
    };
    let items = if app.data.pull_requests.is_empty() {
        vec![ListItem::new(
            "No open pull requests are available for this repository.",
        )]
    } else {
        app.data
            .pull_requests
            .iter()
            .enumerate()
            .map(|(index, pr)| {
                let marker = if app.selected.contains(&index) {
                    "[x]"
                } else {
                    "[ ]"
                };
                ListItem::new(Text::from(vec![
                    Line::from(format!("{marker} #{} {}", pr.number, pr.title)),
                    Line::from(format!(
                        "{} • {} • {}",
                        pr.author, pr.head_ref, pr.updated_at
                    )),
                ]))
            })
            .collect::<Vec<_>>()
    };

    let mut state = ListState::default();
    state.select(Some(
        app.pr_index
            .min(app.data.pull_requests.len().saturating_sub(1)),
    ));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_selection_summary(frame: &mut Frame<'_>, area: Rect, app: &MergeDashboardApp) {
    let title = if app.focus == Focus::Confirm {
        "Selected Batch [focus]"
    } else {
        "Selected Batch"
    };
    let summary = Paragraph::new(app.selection_text())
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(summary, area);
}

fn render_details(frame: &mut Frame<'_>, area: Rect, app: &MergeDashboardApp) {
    let details = Paragraph::new(app.detail_text())
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Planner Input Preview"),
        );
    frame.render_widget(details, area);
}

impl MergeDashboardApp {
    fn new(data: MergeDashboardData) -> Self {
        Self {
            data,
            focus: Focus::PullRequests,
            pr_index: 0,
            selected: BTreeSet::new(),
            completed: None,
        }
    }

    fn apply(&mut self, action: MergeDashboardAction) -> Option<Vec<u64>> {
        self.completed = None;

        match action {
            MergeDashboardAction::Up => {
                if self.focus == Focus::PullRequests {
                    shift_index(&mut self.pr_index, self.data.pull_requests.len(), -1);
                }
            }
            MergeDashboardAction::Down => {
                if self.focus == Focus::PullRequests {
                    shift_index(&mut self.pr_index, self.data.pull_requests.len(), 1);
                }
            }
            MergeDashboardAction::Toggle => {
                if self.focus == Focus::PullRequests
                    && !self.data.pull_requests.is_empty()
                    && !self.selected.insert(self.pr_index)
                {
                    self.selected.remove(&self.pr_index);
                }
            }
            MergeDashboardAction::Enter => match self.focus {
                Focus::PullRequests => {
                    if !self.selected.is_empty() {
                        self.focus = Focus::Confirm;
                    }
                }
                Focus::Confirm => {
                    let selection = self.selected_numbers();
                    self.completed = Some(selection.clone());
                    return Some(selection);
                }
            },
            MergeDashboardAction::Back => match self.focus {
                Focus::PullRequests => return Some(Vec::new()),
                Focus::Confirm => self.focus = Focus::PullRequests,
            },
        }

        None
    }

    fn selected_numbers(&self) -> Vec<u64> {
        self.selected
            .iter()
            .filter_map(|index| self.data.pull_requests.get(*index))
            .map(|pr| pr.number)
            .collect()
    }

    fn selected_prs(&self) -> Vec<&MergeDashboardPullRequest> {
        self.selected
            .iter()
            .filter_map(|index| self.data.pull_requests.get(*index))
            .collect()
    }

    fn summary_line(&self) -> String {
        if let Some(selected) = &self.completed {
            if selected.is_empty() {
                return "Merge canceled before a batch was launched.".to_string();
            }
            return format!(
                "One-shot batch ready: {} pull request(s) will be handed to the merge agent.",
                selected.len()
            );
        }

        match self.focus {
            Focus::PullRequests => {
                if self.data.pull_requests.is_empty() {
                    "The GitHub repository currently has no open pull requests.".to_string()
                } else if self.selected.is_empty() {
                    format!(
                        "{} open pull request(s) discovered for {}. Select one or more entries, then press Enter.",
                        self.data.pull_requests.len(),
                        self.data.repo_label
                    )
                } else {
                    format!(
                        "{} pull request(s) selected. Press Enter to review the one-shot batch summary.",
                        self.selected.len()
                    )
                }
            }
            Focus::Confirm => format!(
                "Review the one-shot batch summary for {} before the merge run starts.",
                self.data.repo_label
            ),
        }
    }

    fn selection_text(&self) -> String {
        if self.data.pull_requests.is_empty() {
            return "No one-shot batch can be created until open pull requests exist on the repository."
                .to_string();
        }

        let selected = self.selected_prs();
        if selected.is_empty() {
            return format!(
                "Step 1 of 2: choose the PRs to batch for `{}`.\n\nNothing is selected yet.",
                self.data.repo_label
            );
        }

        let mut lines = vec![
            format!(
                "Step 2 of 2: this batch is one-shot and will merge into `{}` once launched.",
                self.data.base_branch
            ),
            format!(
                "{} pull request(s) will be handed to the merge agent:",
                selected.len()
            ),
        ];

        for pr in selected {
            lines.push(format!("- #{} {}", pr.number, pr.title));
        }

        if self.focus == Focus::Confirm {
            lines.push(String::new());
            lines.push(
                "Press Enter to start the merge run, or Esc to return to the PR list.".to_string(),
            );
        }

        lines.join("\n")
    }

    fn detail_text(&self) -> String {
        let Some(pr) = self.data.pull_requests.get(self.pr_index) else {
            return format!(
                "Repository: {}\nBase branch: {}\n\nOpen PR discovery is empty, so there is nothing to preview.",
                self.data.repo_label, self.data.base_branch
            );
        };

        format!(
            "Repository: {}\nBase branch: {}\n\nSelected PR preview:\n#{} {}\nAuthor: {}\nHead ref: {}\nUpdated: {}\nURL: {}\n\nThe merge planner receives the selected PR metadata, chooses an explicit merge order, and calls out likely conflict hotspots before execution.",
            self.data.repo_label,
            self.data.base_branch,
            pr.number,
            pr.title,
            pr.author,
            pr.head_ref,
            pr.updated_at,
            pr.url
        )
    }
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
        Focus, MergeDashboardAction, MergeDashboardApp, MergeDashboardData, MergeDashboardExit,
        MergeDashboardOptions, MergeDashboardPullRequest, run_merge_dashboard,
    };

    fn demo_data() -> MergeDashboardData {
        MergeDashboardData {
            title: "meta merge".to_string(),
            repo_label: "metastack-systems/metastack-cli".to_string(),
            base_branch: "main".to_string(),
            pull_requests: vec![
                MergeDashboardPullRequest {
                    number: 101,
                    title: "Add merge transport".to_string(),
                    author: "kames".to_string(),
                    head_ref: "feature/transport".to_string(),
                    updated_at: "2026-03-16T18:30:00Z".to_string(),
                    url: "https://example.com/101".to_string(),
                },
                MergeDashboardPullRequest {
                    number: 102,
                    title: "Add merge dashboard".to_string(),
                    author: "kames".to_string(),
                    head_ref: "feature/dashboard".to_string(),
                    updated_at: "2026-03-16T18:45:00Z".to_string(),
                    url: "https://example.com/102".to_string(),
                },
            ],
        }
    }

    #[test]
    fn render_once_handles_empty_state() {
        let exit = run_merge_dashboard(
            MergeDashboardData {
                title: "meta merge".to_string(),
                repo_label: "demo/repo".to_string(),
                base_branch: "main".to_string(),
                pull_requests: Vec::new(),
            },
            MergeDashboardOptions {
                render_once: true,
                width: 120,
                height: 32,
                actions: Vec::new(),
            },
        )
        .expect("render_once should succeed");

        let MergeDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should produce a snapshot");
        };
        assert!(snapshot.contains("No open pull requests are available"));
        assert!(snapshot.contains("one-shot"));
    }

    #[test]
    fn render_once_shows_single_selection_summary() {
        let exit = run_merge_dashboard(
            demo_data(),
            MergeDashboardOptions {
                render_once: true,
                width: 120,
                height: 32,
                actions: vec![MergeDashboardAction::Toggle, MergeDashboardAction::Enter],
            },
        )
        .expect("render_once should succeed");

        let MergeDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should produce a snapshot");
        };
        assert!(snapshot.contains("#101 Add merge transport"));
        assert!(snapshot.contains("1 pull request(s) will be handed to the merge agent"));
    }

    #[test]
    fn render_once_shows_multi_selection_summary() {
        let exit = run_merge_dashboard(
            demo_data(),
            MergeDashboardOptions {
                render_once: true,
                width: 120,
                height: 32,
                actions: vec![
                    MergeDashboardAction::Toggle,
                    MergeDashboardAction::Down,
                    MergeDashboardAction::Toggle,
                    MergeDashboardAction::Enter,
                ],
            },
        )
        .expect("render_once should succeed");

        let MergeDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should produce a snapshot");
        };
        assert!(snapshot.contains("#101 Add merge transport"));
        assert!(snapshot.contains("#102 Add merge dashboard"));
        assert!(snapshot.contains("2 pull request(s) will be handed to the merge agent"));
    }

    #[test]
    fn back_from_confirm_returns_to_pr_list() {
        let mut app = MergeDashboardApp::new(demo_data());
        assert_eq!(app.focus, Focus::PullRequests);
        app.apply(MergeDashboardAction::Toggle);
        app.apply(MergeDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Confirm);
        app.apply(MergeDashboardAction::Back);
        assert_eq!(app.focus, Focus::PullRequests);
    }

    #[test]
    fn back_from_pr_list_cancels_the_dashboard() {
        let mut app = MergeDashboardApp::new(demo_data());
        let exit = app.apply(MergeDashboardAction::Back);
        assert_eq!(exit, Some(Vec::new()));
    }
}

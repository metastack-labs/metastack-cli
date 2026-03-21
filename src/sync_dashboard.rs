use std::io::{self, IsTerminal};
use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::{CrosstermBackend, TestBackend};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, ListItem, ListState, Wrap};
use ratatui::{Frame, Terminal};

use crate::backlog::BacklogSyncStatus;
use crate::linear::IssueSummary;
use crate::linear::browser::{
    IssueSearchResult, empty_search_result, render_issue_preview as render_linear_issue_preview,
    render_issue_row, search_issues,
};
use crate::tui::fields::InputFieldState;
use crate::tui::scroll::{ScrollState, plain_text, scrollable_paragraph, wrapped_rows};
use crate::tui::theme::{Tone, badge, empty_state, key_hints, list, panel_title, paragraph};

#[derive(Debug, Clone)]
pub struct SyncDashboardData {
    pub title: String,
    pub issues: Vec<SyncDashboardIssue>,
}

#[derive(Debug, Clone)]
pub struct SyncDashboardIssue {
    pub entry_slug: String,
    pub issue: IssueSummary,
    pub linked_issue_identifier: Option<String>,
    pub local_status: BacklogSyncStatus,
}

#[derive(Debug, Clone)]
pub struct SyncDashboardOptions {
    pub render_once: bool,
    pub width: u16,
    pub height: u16,
    pub actions: Vec<SyncDashboardAction>,
    pub vim_mode: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum SyncDashboardAction {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
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
    Preview,
    Actions,
}

#[derive(Debug, Clone)]
struct SyncDashboardApp {
    data: SyncDashboardData,
    focus: Focus,
    query: InputFieldState,
    issue_index: usize,
    action_index: usize,
    completed: Option<SyncSelection>,
    preview_scroll: ScrollState,
}

const ACTIONS: [SyncSelectionAction; 2] = [SyncSelectionAction::Pull, SyncSelectionAction::Push];

pub fn run_sync_dashboard(
    data: SyncDashboardData,
    options: SyncDashboardOptions,
) -> Result<SyncDashboardExit> {
    let _ = options.vim_mode;
    if options.render_once {
        return render_once(data, options).map(SyncDashboardExit::Snapshot);
    }

    if !io::stdout().is_terminal() {
        bail!(
            "the interactive sync dashboard requires a TTY; use `meta backlog sync pull <ISSUE>` or `meta backlog sync push <ISSUE>` for scripted runs"
        );
    }

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = SyncDashboardApp::new(data);

    loop {
        terminal.draw(|frame| render_dashboard(frame, &app))?;

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    let action = match key.code {
                        KeyCode::Char('q') => return Ok(SyncDashboardExit::Cancelled),
                        KeyCode::Up => Some(SyncDashboardAction::Up),
                        KeyCode::Down => Some(SyncDashboardAction::Down),
                        KeyCode::PageUp => Some(SyncDashboardAction::PageUp),
                        KeyCode::PageDown => Some(SyncDashboardAction::PageDown),
                        KeyCode::Home => Some(SyncDashboardAction::Home),
                        KeyCode::End => Some(SyncDashboardAction::End),
                        KeyCode::Tab => Some(SyncDashboardAction::Tab),
                        KeyCode::Enter => Some(SyncDashboardAction::Enter),
                        KeyCode::Esc => Some(SyncDashboardAction::Back),
                        _ => None,
                    };

                    if let Some(action) = action
                        && let Some(selection) =
                            app.apply_in_viewport(action, preview_viewport(terminal.size()?.into()))
                    {
                        return Ok(SyncDashboardExit::Selected(selection));
                    } else {
                        let _ = app.handle_query_key(key);
                    }
                }
                Event::Mouse(mouse)
                    if app.focus == Focus::Preview
                        && matches!(
                            mouse.kind,
                            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                        ) =>
                {
                    let viewport = preview_viewport(terminal.size()?.into());
                    let _ = app.handle_preview_mouse(mouse, viewport);
                }
                _ => {}
            }
        }
    }
}

fn render_once(data: SyncDashboardData, options: SyncDashboardOptions) -> Result<String> {
    let backend = TestBackend::new(options.width, options.height);
    let mut terminal = Terminal::new(backend)?;
    let mut app = SyncDashboardApp::new(data);
    for action in options.actions {
        let _ = app.apply_in_viewport(
            action,
            preview_viewport(Rect::new(0, 0, options.width, options.height)),
        );
    }

    terminal.draw(|frame| render_dashboard(frame, &app))?;
    Ok(snapshot(terminal.backend()))
}

fn render_dashboard(frame: &mut Frame<'_>, app: &SyncDashboardApp) {
    let narrow = frame.area().width < 104;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if narrow { 5 } else { 4 }),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(frame.area());
    let body = Layout::default()
        .direction(if narrow {
            Direction::Vertical
        } else {
            Direction::Horizontal
        })
        .constraints(if narrow {
            vec![Constraint::Percentage(42), Constraint::Percentage(58)]
        } else {
            vec![Constraint::Percentage(46), Constraint::Percentage(54)]
        })
        .split(outer[2]);
    let details = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(if narrow { 50 } else { 58 }),
            Constraint::Length(8),
            Constraint::Min(6),
        ])
        .split(body[1]);

    let header = paragraph(
        Text::from(vec![
            Line::from(app.data.title.clone()),
            Line::from(app.summary_line()),
            key_hints(&[
                ("Type", "search"),
                ("Tab", "focus"),
                ("Up/Down", "move"),
                ("PgUp/PgDn", "scroll preview"),
                ("Wheel", "scroll preview"),
                ("Enter", "advance"),
                ("Esc", "back"),
                ("q", "exit"),
            ]),
        ]),
        panel_title("meta sync", false),
    );
    frame.render_widget(header, outer[0]);

    let rendered_query = app.query.render(
        "Search by backlog slug, linked identifier, title, state, project, or description...",
        app.focus == Focus::Issues,
    );
    let query_block = Block::default()
        .borders(Borders::ALL)
        .title(if app.focus == Focus::Issues {
            "Backlog Search [active]"
        } else {
            "Backlog Search"
        });
    let query_inner = query_block.inner(outer[1]);
    let query = rendered_query.paragraph(query_block);
    frame.render_widget(query, outer[1]);
    rendered_query.set_cursor(frame, query_inner);

    render_issue_list(frame, body[0], app);
    render_issue_preview(frame, details[0], app);
    render_action_list(frame, details[1], app);
    render_status(frame, details[2], app);
}

fn render_issue_list(frame: &mut Frame<'_>, area: Rect, app: &SyncDashboardApp) {
    let results = app.visible_issue_results();
    let title = panel_title(
        format!(
            "Backlog Entries ({}/{})",
            results.len(),
            app.data.issues.len()
        ),
        app.focus == Focus::Issues,
    );
    let items = if app.data.issues.is_empty() {
        vec![ListItem::new(empty_state(
            "No backlog entries were found under `.metastack/backlog/`.",
            "Create or link a backlog entry, then rerun `meta backlog sync`.",
        ))]
    } else if results.is_empty() {
        vec![ListItem::new(empty_state(
            "No backlog entries match the current search.",
            "Clear or broaden the query to choose a sync target.",
        ))]
    } else {
        results
            .iter()
            .filter_map(|result| {
                app.data.issues.get(result.issue_index).map(|issue| {
                    render_issue_row(
                        &issue.issue,
                        Some(result),
                        Some(issue.local_status.as_str()),
                    )
                })
            })
            .collect::<Vec<_>>()
    };

    let mut state = ListState::default();
    if results.is_empty() {
        state.select(Some(0));
    } else {
        state.select(Some(app.issue_index.min(results.len() - 1)));
    }

    let list = list(items, title);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_issue_preview(frame: &mut Frame<'_>, area: Rect, app: &SyncDashboardApp) {
    let preview = scrollable_paragraph(
        app.preview_text(),
        panel_title("Entry Preview", app.focus == Focus::Preview),
        &app.preview_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(preview, area);
}

fn render_action_list(frame: &mut Frame<'_>, area: Rect, app: &SyncDashboardApp) {
    let title = panel_title("Sync Action", app.focus == Focus::Actions);

    let items = ACTIONS
        .iter()
        .map(|action| {
            let enabled = app.action_enabled(*action);
            ListItem::new(Text::from(vec![
                Line::from(app.action_badges(*action, enabled)),
                Line::from(app.action_description(*action, enabled)),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(app.action_index.min(ACTIONS.len() - 1)));

    let list = list(items, title);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &SyncDashboardApp) {
    let status = paragraph(app.status_text(), panel_title("Selection", false));
    frame.render_widget(status, area);
}

impl SyncDashboardApp {
    fn new(data: SyncDashboardData) -> Self {
        Self {
            data,
            focus: Focus::Issues,
            query: InputFieldState::default(),
            issue_index: 0,
            action_index: 0,
            completed: None,
            preview_scroll: ScrollState::default(),
        }
    }

    #[cfg(test)]
    fn apply(&mut self, action: SyncDashboardAction) -> Option<SyncSelection> {
        self.apply_in_viewport(action, preview_viewport(Rect::new(0, 0, 120, 32)))
    }

    fn apply_in_viewport(
        &mut self,
        action: SyncDashboardAction,
        preview_viewport: Rect,
    ) -> Option<SyncSelection> {
        self.completed = None;

        match action {
            SyncDashboardAction::Up => match self.focus {
                Focus::Issues => {
                    let len = self.visible_issue_results().len();
                    shift_index(&mut self.issue_index, len, -1);
                    self.preview_scroll.reset();
                }
                Focus::Preview => self.scroll_preview_key(KeyCode::Up, preview_viewport),
                Focus::Actions => shift_index(&mut self.action_index, ACTIONS.len(), -1),
            },
            SyncDashboardAction::Down => match self.focus {
                Focus::Issues => {
                    let len = self.visible_issue_results().len();
                    shift_index(&mut self.issue_index, len, 1);
                    self.preview_scroll.reset();
                }
                Focus::Preview => self.scroll_preview_key(KeyCode::Down, preview_viewport),
                Focus::Actions => shift_index(&mut self.action_index, ACTIONS.len(), 1),
            },
            SyncDashboardAction::PageUp => {
                if self.focus == Focus::Preview {
                    self.scroll_preview_key(KeyCode::PageUp, preview_viewport);
                }
            }
            SyncDashboardAction::PageDown => {
                if self.focus == Focus::Preview {
                    self.scroll_preview_key(KeyCode::PageDown, preview_viewport);
                }
            }
            SyncDashboardAction::Home => {
                if self.focus == Focus::Preview {
                    self.scroll_preview_key(KeyCode::Home, preview_viewport);
                }
            }
            SyncDashboardAction::End => {
                if self.focus == Focus::Preview {
                    self.scroll_preview_key(KeyCode::End, preview_viewport);
                }
            }
            SyncDashboardAction::Tab => {
                self.focus = match self.focus {
                    Focus::Issues => Focus::Preview,
                    Focus::Preview => Focus::Actions,
                    Focus::Actions => Focus::Issues,
                };
            }
            SyncDashboardAction::Back => {
                if self.focus == Focus::Actions {
                    self.focus = Focus::Preview;
                } else if self.focus == Focus::Preview {
                    self.focus = Focus::Issues;
                }
                self.action_index = 0;
            }
            SyncDashboardAction::Enter => match self.focus {
                Focus::Issues => {
                    if self.selected_issue().is_some() {
                        self.focus = Focus::Preview;
                    }
                }
                Focus::Preview => {
                    if self.selected_issue().is_some() {
                        self.focus = Focus::Actions;
                    }
                }
                Focus::Actions => {
                    let issue = self.selected_issue()?;
                    let issue_identifier = issue.linked_issue_identifier.clone()?;
                    let selection = SyncSelection {
                        issue_identifier,
                        action: ACTIONS[self.action_index],
                    };
                    self.completed = Some(selection.clone());
                    return Some(selection);
                }
            },
        }

        None
    }

    fn handle_query_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.focus != Focus::Issues {
            return false;
        }
        if self.query.handle_key(key) {
            self.issue_index = 0;
            self.preview_scroll.reset();
            return true;
        }
        false
    }

    fn visible_issue_results(&self) -> Vec<IssueSearchResult> {
        if self.query.value().trim().is_empty() {
            return (0..self.data.issues.len())
                .map(empty_search_result)
                .collect();
        }

        let issues = self
            .data
            .issues
            .iter()
            .map(SyncDashboardIssue::search_issue)
            .collect::<Vec<_>>();
        search_issues(&issues, self.query.value().trim())
    }

    fn selected_issue(&self) -> Option<&SyncDashboardIssue> {
        self.visible_issue_results()
            .get(self.issue_index)
            .and_then(|result| self.data.issues.get(result.issue_index))
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
                    "No backlog entries were discovered under `.metastack/backlog/`.".to_string()
                } else {
                    format!(
                        "{} backlog entries loaded from local `.metastack/backlog/`. Search narrows the list before you choose pull or push.",
                        self.visible_issue_results().len()
                    )
                }
            }
            Focus::Preview => {
                "Review the selected backlog preview. PgUp/PgDn/Home/End or the mouse wheel scroll when the panel overflows.".to_string()
            }
            Focus::Actions => match self.selected_issue() {
                Some(issue) if issue.is_linked() => format!(
                    "Choose whether to pull or push {}.",
                    issue.linked_issue_identifier.as_deref().unwrap_or_default()
                ),
                Some(issue) => format!(
                    "{} is local-only. Link it before pull or push becomes available.",
                    issue.entry_slug
                ),
                None => "No backlog entry is available to sync.".to_string(),
            },
        }
    }

    fn preview_text(&self) -> Text<'static> {
        let results = self.visible_issue_results();
        let Some(result) = results.get(self.issue_index) else {
            return Text::from("No backlog entry is available for the current search.");
        };
        let issue = &self.data.issues[result.issue_index];
        issue.preview_text(Some(result))
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
                    "Create or link backlog entries under `.metastack/backlog/`, then rerun `meta backlog sync`."
                        .to_string()
                } else {
                    "Step 1 of 3: search or choose a backlog entry sourced from local `.metastack/backlog/`."
                        .to_string()
                }
            }
            Focus::Preview => "Step 2 of 3: review or scroll the selected backlog preview with PgUp/PgDn/Home/End or the mouse wheel before choosing a sync action.".to_string(),
            Focus::Actions => match self.selected_issue() {
                Some(issue) if issue.is_linked() => "Step 3 of 3: choose pull to refresh local files or push to sync managed attachments. `index.md` only updates the Linear description when you run push with `--update-description`.".to_string(),
                Some(issue) => format!(
                    "This backlog entry is unlinked. Run `meta backlog sync link <ISSUE> --entry {}` before pull or push becomes available.",
                    issue.entry_slug
                ),
                None => "No backlog entry is selected.".to_string(),
            },
        }
    }

    fn preview_content_rows(&self, width: u16) -> usize {
        wrapped_rows(&plain_text(&self.preview_text()), width.max(1))
    }

    fn scroll_preview_key(&mut self, key: KeyCode, viewport: Rect) {
        let _ = self.preview_scroll.apply_key_code_in_viewport(
            key,
            viewport,
            self.preview_content_rows(viewport.width.max(1)),
        );
    }

    fn handle_preview_mouse(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        viewport: Rect,
    ) -> bool {
        self.preview_scroll.apply_mouse_in_viewport(
            mouse,
            viewport,
            self.preview_content_rows(viewport.width.max(1)),
        )
    }

    fn action_enabled(&self, _action: SyncSelectionAction) -> bool {
        self.selected_issue()
            .is_some_and(SyncDashboardIssue::is_linked)
    }

    fn action_badges(&self, action: SyncSelectionAction, enabled: bool) -> Vec<Span<'static>> {
        let mut badges = vec![badge(
            action.label(),
            if enabled { Tone::Accent } else { Tone::Muted },
        )];
        if !enabled {
            badges.push(Span::raw(" "));
            badges.push(badge("link required", Tone::Muted));
        }
        badges
    }

    fn action_description(&self, action: SyncSelectionAction, enabled: bool) -> &'static str {
        if enabled {
            action.description()
        } else {
            "Link this backlog entry first; remote sync actions stay disabled until `.linear.json` points at a Linear issue."
        }
    }
}

impl SyncDashboardIssue {
    fn is_linked(&self) -> bool {
        self.linked_issue_identifier.is_some()
    }

    fn search_issue(&self) -> IssueSummary {
        let mut issue = self.issue.clone();
        let entry_context = format!("Backlog entry: {}", self.entry_slug);
        issue.description = Some(match issue.description.as_deref() {
            Some(description) if !description.trim().is_empty() => {
                format!("{entry_context}\n\n{description}")
            }
            _ => entry_context,
        });
        issue
    }

    fn preview_text(&self, result: Option<&IssueSearchResult>) -> Text<'static> {
        let mut lines = vec![
            Line::from(vec![
                Span::raw("entry "),
                Span::raw(self.entry_slug.clone()),
            ]),
            Line::from(vec![
                Span::raw("link "),
                Span::raw(
                    self.linked_issue_identifier
                        .clone()
                        .unwrap_or_else(|| "unlinked".to_string()),
                ),
            ]),
            Line::from(""),
        ];

        if self.is_linked() {
            let mut preview = render_linear_issue_preview(
                &self.issue,
                result,
                Some(self.local_status.as_str()),
                "No description provided.",
            );
            lines.append(&mut preview.lines);
            return Text::from(lines);
        }

        lines.extend([
            Line::from(vec![Span::raw(format!("title {}", self.issue.title))]),
            Line::from(vec![Span::raw("state local-only")]),
            Line::from(vec![Span::raw(format!(
                "sync {}",
                self.local_status.as_str()
            ))]),
            Line::from(vec![Span::raw(format!(
                "path .metastack/backlog/{}",
                self.entry_slug
            ))]),
            Line::from(""),
            Line::from("This backlog entry is not linked to Linear yet."),
            Line::from(format!(
                "Run `meta backlog sync link <ISSUE> --entry {}` to enable pull and push.",
                self.entry_slug
            )),
        ]);
        Text::from(lines)
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
        let _ = execute!(stdout, DisableMouseCapture, LeaveAlternateScreen);
    }
}

fn preview_viewport(area: Rect) -> Rect {
    let narrow = area.width < 104;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if narrow { 5 } else { 4 }),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);
    let body = Layout::default()
        .direction(if narrow {
            Direction::Vertical
        } else {
            Direction::Horizontal
        })
        .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(outer[2]);
    let details = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(10),
            Constraint::Length(8),
            Constraint::Min(6),
        ])
        .split(body[1]);
    Rect::new(
        details[0].x.saturating_add(1),
        details[0].y.saturating_add(1),
        details[0].width.saturating_sub(2).max(1),
        details[0].height.saturating_sub(2).max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Focus, SyncDashboardAction, SyncDashboardApp, SyncDashboardData, SyncDashboardExit,
        SyncDashboardIssue, SyncDashboardOptions, preview_viewport, run_sync_dashboard,
    };
    use crate::backlog::BacklogSyncStatus;
    use crate::linear::{DashboardData, IssueSummary, ProjectRef, WorkflowState};
    use crate::tui::fields::InputFieldState;
    use ratatui::layout::Rect;

    fn demo_data() -> SyncDashboardData {
        let demo = DashboardData::demo();
        let mut issues = demo.issues;
        let team = issues
            .first()
            .map(|issue| issue.team.clone())
            .expect("demo issues should not be empty");
        issues.push(IssueSummary {
            id: "issue-13".to_string(),
            identifier: "MET-13".to_string(),
            title: "Manual Follow-up".to_string(),
            description: Some(
                "Track the local-only backlog entry before it is linked to Linear.".to_string(),
            ),
            url: "https://linear.app/metastack/MET-13".to_string(),
            priority: None,
            estimate: None,
            updated_at: "2026-03-14T16:10:00Z".to_string(),
            team,
            project: Some(ProjectRef {
                id: "project-demo".to_string(),
                name: "MetaStack CLI".to_string(),
            }),
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: Some(WorkflowState {
                id: "state-backlog".to_string(),
                name: "Backlog".to_string(),
                kind: Some("backlog".to_string()),
            }),
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        });
        SyncDashboardData {
            title: demo.title,
            issues: issues
                .into_iter()
                .enumerate()
                .map(|(index, issue)| SyncDashboardIssue {
                    entry_slug: issue.identifier.clone(),
                    issue,
                    linked_issue_identifier: match index {
                        0 | 1 => Some(format!("MET-1{}", index + 1)),
                        _ => None,
                    },
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
                    SyncDashboardAction::Enter,
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Enter,
                ],
                vim_mode: false,
            },
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };
        assert!(snapshot.contains("Ready to push MET-12"));
        assert!(snapshot.contains("Backlog Search"));
        assert!(snapshot.contains("Sync Action [focus]"));
        assert!(snapshot.contains("diverged"));
        assert!(snapshot.contains("Backlog Entries"));
    }

    #[test]
    fn back_returns_focus_to_issue_list() {
        let mut app = SyncDashboardApp::new(demo_data());

        assert_eq!(app.focus, Focus::Issues);
        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Preview);
        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Actions);
        app.apply(SyncDashboardAction::Back);
        assert_eq!(app.focus, Focus::Preview);
        app.apply(SyncDashboardAction::Back);
        assert_eq!(app.focus, Focus::Issues);
    }

    #[test]
    fn sync_dashboard_search_can_narrow_to_zero_results() {
        let mut app = SyncDashboardApp::new(demo_data());
        app.query = InputFieldState::new("zzz");

        assert!(app.visible_issue_results().is_empty());
        assert!(format!("{:?}", app.preview_text()).contains("No backlog entry is available"));
    }

    #[test]
    fn unlinked_rows_do_not_complete_push_actions() {
        let exit = run_sync_dashboard(
            demo_data(),
            SyncDashboardOptions {
                render_once: true,
                width: 120,
                height: 32,
                actions: vec![
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Enter,
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Enter,
                ],
                vim_mode: false,
            },
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };
        assert!(snapshot.contains("link required"));
        assert!(snapshot.contains("This backlog entry is unlinked."));
        assert!(
            snapshot.contains("<ISSUE> --entry MET-13` before pull or push becomes available.")
        );
        assert!(!snapshot.contains("Ready to push"));
    }

    #[test]
    fn sync_dashboard_preview_scrolls_to_bottom() {
        let mut data = demo_data();
        data.issues[0].issue.description = Some(
            (1..=24)
                .map(|index| format!("sync preview line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let viewport = preview_viewport(Rect::new(0, 0, 120, 20));
        let mut app = SyncDashboardApp::new(data);
        let _ = app.apply_in_viewport(SyncDashboardAction::Tab, viewport);
        let _ = app.apply_in_viewport(SyncDashboardAction::End, viewport);

        assert_eq!(app.focus, Focus::Preview);
        assert!(app.preview_scroll.offset() > 0);
    }

    #[test]
    fn render_once_mentions_mouse_wheel_in_preview_guidance() {
        let exit = run_sync_dashboard(
            demo_data(),
            SyncDashboardOptions {
                render_once: true,
                width: 120,
                height: 32,
                actions: vec![SyncDashboardAction::Enter],
                vim_mode: false,
            },
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };

        assert!(snapshot.contains("mouse wheel"));
    }
}

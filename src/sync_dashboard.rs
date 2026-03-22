use std::collections::BTreeSet;
use std::io::{self, IsTerminal};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Result, bail};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseEventKind,
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
    render_issue_row_with_prefix as render_sync_issue_row, search_issues,
};
use crate::tui::fields::InputFieldState;
use crate::tui::scroll::{ScrollState, plain_text, scrollable_content_paragraph, wrapped_rows};
use crate::tui::theme::{Tone, badge, empty_state, key_hints, list, panel_title, paragraph};

/// Load state for a single backlog issue in the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueLoadState {
    Loading,
    Loaded,
    Failed,
}

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
    pub load_state: IssueLoadState,
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
    ToggleSelect,
    SelectAll,
    FocusSearch,
    CycleStatusFilter,
    CycleLabelFilter,
    ClearFilters,
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

/// Message sent from background loading tasks to the dashboard event loop.
pub struct IssueUpdate {
    pub index: usize,
    pub issue: SyncDashboardIssue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncDashboardExit {
    Snapshot(String),
    Cancelled,
    Selected(Vec<SyncSelection>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Search,
    List,
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
    selected: BTreeSet<usize>,
    completed: Vec<SyncSelection>,
    preview_scroll: ScrollState,
    status_filter: Option<String>,
    label_filter: Option<String>,
}

const ACTIONS: [SyncSelectionAction; 2] = [SyncSelectionAction::Pull, SyncSelectionAction::Push];

/// Run the interactive sync dashboard, optionally receiving background issue updates.
///
/// When `issue_updates` is provided, the dashboard renders immediately and applies issue
/// data as it arrives through the channel without blocking the first paint.
pub fn run_sync_dashboard(
    data: SyncDashboardData,
    options: SyncDashboardOptions,
    issue_updates: Option<mpsc::Receiver<IssueUpdate>>,
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
        if let Some(ref rx) = issue_updates {
            app.drain_updates(rx);
        }

        terminal.draw(|frame| render_dashboard(frame, &app))?;

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // Ctrl+C always exits.
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        return Ok(SyncDashboardExit::Cancelled);
                    }

                    // When the search field has focus, route all character
                    // keys (including Space) to the query field.  Only
                    // navigation/focus keys escape the search pane.
                    if app.focus == Focus::Search {
                        let action = match key.code {
                            KeyCode::Esc => Some(SyncDashboardAction::Back),
                            KeyCode::Enter => Some(SyncDashboardAction::Enter),
                            KeyCode::Tab => Some(SyncDashboardAction::Tab),
                            KeyCode::Up => Some(SyncDashboardAction::Up),
                            KeyCode::Down => Some(SyncDashboardAction::Down),
                            _ => None,
                        };

                        if let Some(action) = action {
                            let result = app.apply_in_viewport(
                                action,
                                preview_viewport(terminal.size()?.into()),
                            );
                            if !result.is_empty() {
                                return Ok(SyncDashboardExit::Selected(result));
                            }
                        } else {
                            let _ = app.handle_query_key(key);
                        }
                    } else {
                        // Non-search focus: List, Preview, Actions.
                        let action = match key.code {
                            KeyCode::Char('q') => {
                                return Ok(SyncDashboardExit::Cancelled);
                            }
                            KeyCode::Char('/') if app.focus == Focus::List => {
                                Some(SyncDashboardAction::FocusSearch)
                            }
                            KeyCode::Char(' ') if app.focus == Focus::List => {
                                Some(SyncDashboardAction::ToggleSelect)
                            }
                            KeyCode::Char('a')
                                if key.modifiers.contains(KeyModifiers::CONTROL)
                                    && app.focus == Focus::List =>
                            {
                                Some(SyncDashboardAction::SelectAll)
                            }
                            KeyCode::Char('s')
                                if key.modifiers.contains(KeyModifiers::CONTROL)
                                    && app.focus == Focus::List =>
                            {
                                Some(SyncDashboardAction::CycleStatusFilter)
                            }
                            KeyCode::Char('l')
                                if key.modifiers.contains(KeyModifiers::CONTROL)
                                    && app.focus == Focus::List =>
                            {
                                Some(SyncDashboardAction::CycleLabelFilter)
                            }
                            KeyCode::Char('r')
                                if key.modifiers.contains(KeyModifiers::CONTROL)
                                    && app.focus == Focus::List =>
                            {
                                Some(SyncDashboardAction::ClearFilters)
                            }
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

                        if let Some(action) = action {
                            let result = app.apply_in_viewport(
                                action,
                                preview_viewport(terminal.size()?.into()),
                            );
                            if !result.is_empty() {
                                return Ok(SyncDashboardExit::Selected(result));
                            }
                        }
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
            Constraint::Length(if narrow { 6 } else { 5 }),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(4),
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
            Line::from(app.filter_hint_line()),
        ]),
        panel_title("meta sync", false),
    );
    frame.render_widget(header, outer[0]);

    let rendered_query = app.query.render(
        "Search by backlog slug, linked identifier, title, state, project, or description...",
        app.focus == Focus::Search,
    );
    let query_block = Block::default()
        .borders(Borders::ALL)
        .title(if app.focus == Focus::Search {
            "Backlog Search [focus]"
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

    render_footer(frame, outer[3], app);
}

fn render_issue_list(frame: &mut Frame<'_>, area: Rect, app: &SyncDashboardApp) {
    let results = app.visible_issue_results();
    let selected_count = app.selected.len();
    let filtered = app.status_filter.is_some() || app.label_filter.is_some();
    let title_text = {
        let mut parts = format!(
            "Backlog Entries ({}/{})",
            results.len(),
            app.data.issues.len()
        );
        if filtered {
            parts.push_str(" [filtered]");
        }
        if selected_count > 0 {
            parts.push_str(&format!(" [{selected_count} selected]"));
        }
        parts
    };
    let title = panel_title(title_text, app.focus == Focus::List);
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
                    let is_selected = app.selected.contains(&result.issue_index);
                    let prefix = checkbox_prefix(is_selected);
                    if issue.load_state == IssueLoadState::Loading {
                        ListItem::new(Text::from(vec![
                            Line::from(vec![
                                Span::raw(prefix),
                                Span::raw(issue.entry_slug.clone()),
                                Span::raw("  "),
                                Span::styled("Loading...", loading_style()),
                            ]),
                            Line::from(""),
                        ]))
                    } else {
                        render_sync_issue_row(
                            &issue.issue,
                            Some(result),
                            Some(issue.local_status.as_str()),
                            prefix,
                        )
                    }
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
    let preview = scrollable_content_paragraph(
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

fn render_footer(frame: &mut Frame<'_>, area: Rect, app: &SyncDashboardApp) {
    let focus_label = match app.focus {
        Focus::Search => "Search",
        Focus::List => "List",
        Focus::Preview => "Preview",
        Focus::Actions => "Actions",
    };
    let hints: Vec<(&str, &str)> = match app.focus {
        Focus::Search => vec![
            ("Esc", "clear/back"),
            ("Enter", "go to list"),
            ("Tab", "next pane"),
            ("Up/Down", "move"),
        ],
        Focus::List => vec![
            ("/", "search"),
            ("Space", "select"),
            ("Ctrl+A", "select visible"),
            ("Enter", "advance"),
            ("Tab", "next pane"),
            ("q", "exit"),
        ],
        Focus::Preview => vec![
            ("PgUp/PgDn", "scroll"),
            ("Enter", "advance"),
            ("Esc", "back"),
            ("Tab", "next pane"),
            ("q", "exit"),
        ],
        Focus::Actions => vec![
            ("Enter", "confirm"),
            ("Esc", "back"),
            ("Tab", "next pane"),
            ("q", "exit"),
        ],
    };
    let footer = paragraph(
        Text::from(vec![
            key_hints(&hints),
            Line::from(format!("Focus: {focus_label}  |  Ctrl+C quit")),
        ]),
        panel_title("Keys", false),
    );
    frame.render_widget(footer, area);
}

impl SyncDashboardApp {
    fn new(data: SyncDashboardData) -> Self {
        Self {
            data,
            focus: Focus::List,
            query: InputFieldState::default(),
            issue_index: 0,
            action_index: 0,
            selected: BTreeSet::new(),
            completed: Vec::new(),
            preview_scroll: ScrollState::default(),
            status_filter: None,
            label_filter: None,
        }
    }

    fn drain_updates(&mut self, rx: &mpsc::Receiver<IssueUpdate>) {
        while let Ok(update) = rx.try_recv() {
            if update.index < self.data.issues.len() {
                self.data.issues[update.index] = update.issue;
            }
        }
    }

    #[cfg(test)]
    fn apply(&mut self, action: SyncDashboardAction) -> Vec<SyncSelection> {
        self.apply_in_viewport(action, preview_viewport(Rect::new(0, 0, 120, 32)))
    }

    fn apply_in_viewport(
        &mut self,
        action: SyncDashboardAction,
        preview_viewport: Rect,
    ) -> Vec<SyncSelection> {
        self.completed.clear();

        match action {
            SyncDashboardAction::Up => match self.focus {
                Focus::Search | Focus::List => {
                    let len = self.visible_issue_results().len();
                    shift_index(&mut self.issue_index, len, -1);
                    self.preview_scroll.reset();
                }
                Focus::Preview => self.scroll_preview_key(KeyCode::Up, preview_viewport),
                Focus::Actions => shift_index(&mut self.action_index, ACTIONS.len(), -1),
            },
            SyncDashboardAction::Down => match self.focus {
                Focus::Search | Focus::List => {
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
                    Focus::Search => Focus::List,
                    Focus::List => Focus::Preview,
                    Focus::Preview => Focus::Actions,
                    Focus::Actions => Focus::Search,
                };
            }
            SyncDashboardAction::FocusSearch => {
                self.focus = Focus::Search;
            }
            SyncDashboardAction::Back => {
                if self.focus == Focus::Search {
                    if !self.query.value().is_empty() {
                        // Esc clears the search query first.
                        self.query.clear();
                        self.issue_index = 0;
                        self.preview_scroll.reset();
                    } else {
                        // With empty query, Esc returns to the results list.
                        self.focus = Focus::List;
                    }
                } else if self.focus == Focus::Actions {
                    self.focus = Focus::Preview;
                } else if self.focus == Focus::Preview {
                    self.focus = Focus::List;
                }
                self.action_index = 0;
            }
            SyncDashboardAction::ToggleSelect => {
                if self.focus == Focus::List {
                    let results = self.visible_issue_results();
                    if let Some(result) = results.get(self.issue_index) {
                        let issue_index = result.issue_index;
                        if !self.selected.remove(&issue_index) {
                            self.selected.insert(issue_index);
                        }
                    }
                }
            }
            SyncDashboardAction::SelectAll => {
                if self.focus == Focus::List {
                    let visible_indices: BTreeSet<usize> = self
                        .visible_issue_results()
                        .iter()
                        .map(|r| r.issue_index)
                        .collect();
                    let all_visible_selected =
                        !visible_indices.is_empty() && visible_indices.is_subset(&self.selected);
                    if all_visible_selected {
                        for idx in &visible_indices {
                            self.selected.remove(idx);
                        }
                    } else {
                        self.selected.extend(visible_indices);
                    }
                }
            }
            SyncDashboardAction::CycleStatusFilter => {
                if self.focus == Focus::List {
                    let statuses = self.available_statuses();
                    self.status_filter = cycle_filter(&self.status_filter, &statuses);
                    self.issue_index = 0;
                    self.preview_scroll.reset();
                }
            }
            SyncDashboardAction::CycleLabelFilter => {
                if self.focus == Focus::List {
                    let labels = self.available_labels();
                    self.label_filter = cycle_filter(&self.label_filter, &labels);
                    self.issue_index = 0;
                    self.preview_scroll.reset();
                }
            }
            SyncDashboardAction::ClearFilters => {
                if self.focus == Focus::List {
                    self.status_filter = None;
                    self.label_filter = None;
                    self.issue_index = 0;
                    self.preview_scroll.reset();
                }
            }
            SyncDashboardAction::Enter => match self.focus {
                Focus::Search => {
                    // Enter from search moves to the results list so the
                    // user can interact with the filtered results.
                    self.focus = Focus::List;
                }
                Focus::List => {
                    if !self.selected.is_empty() || self.selected_issue().is_some() {
                        self.focus = Focus::Preview;
                    }
                }
                Focus::Preview => {
                    if !self.selected.is_empty() || self.selected_issue().is_some() {
                        self.focus = Focus::Actions;
                    }
                }
                Focus::Actions => {
                    let selections = self.build_selections();
                    if !selections.is_empty() {
                        self.completed = selections.clone();
                        return selections;
                    }
                }
            },
        }

        Vec::new()
    }

    fn build_selections(&self) -> Vec<SyncSelection> {
        let action = ACTIONS[self.action_index];
        let target_indices = if self.selected.is_empty() {
            let results = self.visible_issue_results();
            results
                .get(self.issue_index)
                .map(|r| vec![r.issue_index])
                .unwrap_or_default()
        } else {
            self.selected.iter().copied().collect()
        };

        target_indices
            .into_iter()
            .filter_map(|idx| {
                let issue = self.data.issues.get(idx)?;
                let identifier = issue.linked_issue_identifier.clone()?;
                Some(SyncSelection {
                    issue_identifier: identifier,
                    action,
                })
            })
            .collect()
    }

    fn handle_query_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        if self.focus != Focus::Search {
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
        let base_results = if self.query.value().trim().is_empty() {
            (0..self.data.issues.len())
                .map(empty_search_result)
                .collect()
        } else {
            let issues = self
                .data
                .issues
                .iter()
                .map(SyncDashboardIssue::search_issue)
                .collect::<Vec<_>>();
            search_issues(&issues, self.query.value().trim())
        };

        if self.status_filter.is_none() && self.label_filter.is_none() {
            return base_results;
        }

        base_results
            .into_iter()
            .filter(|result| {
                let Some(issue) = self.data.issues.get(result.issue_index) else {
                    return false;
                };
                if let Some(ref status) = self.status_filter {
                    let issue_state = issue
                        .issue
                        .state
                        .as_ref()
                        .map(|s| s.name.as_str())
                        .unwrap_or("None");
                    if !issue_state.eq_ignore_ascii_case(status) {
                        return false;
                    }
                }
                if let Some(ref label) = self.label_filter {
                    let has_label = issue
                        .issue
                        .labels
                        .iter()
                        .any(|l| l.name.eq_ignore_ascii_case(label));
                    if !has_label {
                        return false;
                    }
                }
                true
            })
            .collect()
    }

    fn available_statuses(&self) -> Vec<String> {
        let mut statuses = BTreeSet::new();
        for issue in &self.data.issues {
            if let Some(state) = &issue.issue.state {
                statuses.insert(state.name.clone());
            }
        }
        statuses.into_iter().collect()
    }

    fn available_labels(&self) -> Vec<String> {
        let mut labels = BTreeSet::new();
        for issue in &self.data.issues {
            for label in &issue.issue.labels {
                labels.insert(label.name.clone());
            }
        }
        labels.into_iter().collect()
    }

    fn selected_issue(&self) -> Option<&SyncDashboardIssue> {
        self.visible_issue_results()
            .get(self.issue_index)
            .and_then(|result| self.data.issues.get(result.issue_index))
    }

    fn summary_line(&self) -> String {
        if !self.completed.is_empty() {
            let verb = self
                .completed
                .first()
                .map(|s| s.action.verb())
                .unwrap_or("sync");
            let identifiers = self
                .completed
                .iter()
                .map(|s| s.issue_identifier.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return format!("Ready to {verb} {identifiers}");
        }

        let loading = self
            .data
            .issues
            .iter()
            .filter(|i| i.load_state == IssueLoadState::Loading)
            .count();

        match self.focus {
            Focus::Search => {
                let visible = self.visible_issue_results().len();
                let total = self.data.issues.len();
                if self.query.value().is_empty() {
                    format!(
                        "Type to filter {total} backlog entries. Press Esc or Enter to return to the list."
                    )
                } else {
                    format!(
                        "{visible}/{total} entries match. Press Enter to interact with results, Esc to clear.",
                    )
                }
            }
            Focus::List => {
                if self.data.issues.is_empty() {
                    "No backlog entries were discovered under `.metastack/backlog/`.".to_string()
                } else if loading > 0 {
                    let visible = self.visible_issue_results().len();
                    format!(
                        "{visible} backlog entries ({loading} loading). Space selects, / searches.",
                    )
                } else {
                    let visible = self.visible_issue_results().len();
                    let total = self.data.issues.len();
                    if visible < total {
                        format!(
                            "{visible}/{total} entries shown (filtered). Space selects, / searches.",
                        )
                    } else {
                        format!("{visible} backlog entries loaded. Space selects, / searches.",)
                    }
                }
            }
            Focus::Preview => {
                let sel_count = self.selected.len();
                if sel_count > 1 {
                    format!(
                        "{sel_count} issues selected. Review selection, then choose a sync action."
                    )
                } else {
                    "Review the selected backlog preview. PgUp/PgDn/Home/End or the mouse wheel scroll when the panel overflows.".to_string()
                }
            }
            Focus::Actions => {
                let sel_count = self.selected.len();
                if sel_count > 1 {
                    format!("Choose pull or push for all {sel_count} selected issues.")
                } else {
                    match self.selected_issue() {
                        Some(issue) if issue.is_linked() => format!(
                            "Choose whether to pull or push {}.",
                            issue.linked_issue_identifier.as_deref().unwrap_or_default()
                        ),
                        Some(issue) => format!(
                            "{} is local-only. Link it before pull or push becomes available.",
                            issue.entry_slug
                        ),
                        None => "No backlog entry is available to sync.".to_string(),
                    }
                }
            }
        }
    }

    fn filter_hint_line(&self) -> Vec<Span<'static>> {
        let mut hints = Vec::new();
        let has_any_filter = self.status_filter.is_some() || self.label_filter.is_some();

        hints.push(Span::raw("Ctrl+S: status"));
        if let Some(ref status) = self.status_filter {
            hints.push(Span::styled(
                format!(" [{status}]"),
                ratatui::style::Style::default()
                    .fg(ratatui::style::Color::Yellow)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        hints.push(Span::raw("  Ctrl+L: label"));
        if let Some(ref label) = self.label_filter {
            hints.push(Span::styled(
                format!(" [{label}]"),
                ratatui::style::Style::default()
                    .fg(ratatui::style::Color::Yellow)
                    .add_modifier(ratatui::style::Modifier::BOLD),
            ));
        }
        if has_any_filter {
            hints.push(Span::raw("  Ctrl+R: clear filters"));
        }
        hints
    }

    fn preview_text(&self) -> Text<'static> {
        if self.selected.len() > 1 {
            let mut lines = vec![Line::from(format!(
                "{} issues selected:",
                self.selected.len()
            ))];
            lines.push(Line::from(""));
            for &idx in &self.selected {
                if let Some(issue) = self.data.issues.get(idx) {
                    let identifier = issue
                        .linked_issue_identifier
                        .as_deref()
                        .unwrap_or(&issue.entry_slug);
                    lines.push(Line::from(format!(
                        "  {} - {} [{}]",
                        identifier,
                        issue.issue.title,
                        issue.local_status.as_str(),
                    )));
                }
            }
            return Text::from(lines);
        }

        let results = self.visible_issue_results();
        let Some(result) = results.get(self.issue_index) else {
            return Text::from("No backlog entry is available for the current search.");
        };
        let issue = &self.data.issues[result.issue_index];
        if issue.load_state == IssueLoadState::Loading {
            return Text::from("Loading issue data from Linear...");
        }
        issue.preview_text(Some(result))
    }

    fn status_text(&self) -> String {
        if !self.completed.is_empty() {
            let verb = self
                .completed
                .first()
                .map(|s| s.action.verb())
                .unwrap_or("sync");
            let count = self.completed.len();
            return format!(
                "Ready to {verb} {count} issue{}.\nRender-once stops at the chosen state; interactive mode executes this selection immediately.",
                plural_suffix(count),
            );
        }

        match self.focus {
            Focus::Search => {
                "Type to filter entries. Press Enter to return to the list, or Esc to clear the query."
                    .to_string()
            }
            Focus::List => {
                if self.data.issues.is_empty() {
                    "Create or link backlog entries under `.metastack/backlog/`, then rerun `meta backlog sync`."
                        .to_string()
                } else {
                    "Step 1 of 3: choose backlog entries from `.metastack/backlog/`. Space selects, / searches, Ctrl+A selects all visible."
                        .to_string()
                }
            }
            Focus::Preview => {
                let sel_count = self.selected.len();
                if sel_count > 1 {
                    format!(
                        "Step 2 of 3: {sel_count} issues selected. Press Enter to choose an action or Esc to go back and adjust selection."
                    )
                } else {
                    "Step 2 of 3: review or scroll the selected backlog preview with PgUp/PgDn/Home/End or the mouse wheel before choosing a sync action.".to_string()
                }
            }
            Focus::Actions => {
                let sel_count = self.selected.len();
                if sel_count > 1 {
                    format!(
                        "Step 3 of 3: choose pull or push for all {sel_count} selected issues. Only linked issues will be synced."
                    )
                } else {
                    match self.selected_issue() {
                        Some(issue) if issue.is_linked() => "Step 3 of 3: choose pull to refresh local files or push to sync managed attachments. `index.md` only updates the Linear description when you run push with `--update-description`.".to_string(),
                        Some(issue) => format!(
                            "This backlog entry is unlinked. Run `meta backlog sync link <ISSUE> --entry {}` before pull or push becomes available.",
                            issue.entry_slug
                        ),
                        None => "No backlog entry is selected.".to_string(),
                    }
                }
            }
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
        if !self.selected.is_empty() {
            return self.selected.iter().any(|&idx| {
                self.data
                    .issues
                    .get(idx)
                    .is_some_and(SyncDashboardIssue::is_linked)
            });
        }
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

    pub(crate) fn verb(self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Push => "push",
        }
    }
}

fn checkbox_prefix(selected: bool) -> &'static str {
    if selected { "[x] " } else { "[ ] " }
}

fn loading_style() -> ratatui::style::Style {
    ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray)
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn cycle_filter(current: &Option<String>, options: &[String]) -> Option<String> {
    if options.is_empty() {
        return None;
    }
    match current {
        None => Some(options[0].clone()),
        Some(current_value) => {
            let current_pos = options.iter().position(|o| o == current_value);
            match current_pos {
                Some(pos) if pos + 1 < options.len() => Some(options[pos + 1].clone()),
                _ => None,
            }
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
            Constraint::Length(if narrow { 7 } else { 6 }),
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
        Focus, IssueLoadState, IssueUpdate, SyncDashboardAction, SyncDashboardApp,
        SyncDashboardData, SyncDashboardExit, SyncDashboardIssue, SyncDashboardOptions,
        SyncSelectionAction, preview_viewport, run_sync_dashboard,
    };
    use crate::backlog::BacklogSyncStatus;
    use crate::linear::{DashboardData, IssueSummary, LabelRef, ProjectRef, WorkflowState};
    use crate::tui::fields::InputFieldState;
    use ratatui::layout::Rect;
    use std::sync::mpsc;

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
                    load_state: IssueLoadState::Loaded,
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
                height: 36,
                actions: vec![
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Enter,
                    SyncDashboardAction::Enter,
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Enter,
                ],
                vim_mode: false,
            },
            None,
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };
        assert!(snapshot.contains("Ready to push MET-12"));
        assert!(snapshot.contains("Backlog Search"));
        assert!(snapshot.contains("Sync Action [focus]"));
        assert!(snapshot.contains("Backlog Entries"));
    }

    #[test]
    fn back_returns_focus_to_issue_list() {
        let mut app = SyncDashboardApp::new(demo_data());

        assert_eq!(app.focus, Focus::List);
        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Preview);
        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Actions);
        app.apply(SyncDashboardAction::Back);
        assert_eq!(app.focus, Focus::Preview);
        app.apply(SyncDashboardAction::Back);
        assert_eq!(app.focus, Focus::List);
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
                height: 36,
                actions: vec![
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Enter,
                    SyncDashboardAction::Down,
                    SyncDashboardAction::Enter,
                ],
                vim_mode: false,
            },
            None,
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
        let viewport = preview_viewport(Rect::new(0, 0, 120, 24));
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
                height: 36,
                actions: vec![SyncDashboardAction::Enter],
                vim_mode: false,
            },
            None,
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };

        assert!(snapshot.contains("mouse wheel"));
    }

    // --- Multi-select tests ---

    #[test]
    fn toggle_select_adds_and_removes_issue() {
        let mut app = SyncDashboardApp::new(demo_data());

        assert!(app.selected.is_empty());
        app.apply(SyncDashboardAction::ToggleSelect);
        assert_eq!(app.selected.len(), 1);
        assert!(app.selected.contains(&0));

        app.apply(SyncDashboardAction::ToggleSelect);
        assert!(app.selected.is_empty());
    }

    #[test]
    fn multi_select_returns_multiple_selections() {
        let mut app = SyncDashboardApp::new(demo_data());

        app.apply(SyncDashboardAction::ToggleSelect);
        app.apply(SyncDashboardAction::Down);
        app.apply(SyncDashboardAction::ToggleSelect);
        assert_eq!(app.selected.len(), 2);

        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Preview);
        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Actions);

        let selections = app.apply(SyncDashboardAction::Enter);
        assert_eq!(selections.len(), 2);
        assert_eq!(selections[0].issue_identifier, "MET-11");
        assert_eq!(selections[1].issue_identifier, "MET-12");
        assert_eq!(selections[0].action, SyncSelectionAction::Pull);
    }

    #[test]
    fn select_all_selects_all_visible_issues() {
        let mut app = SyncDashboardApp::new(demo_data());

        app.apply(SyncDashboardAction::SelectAll);
        let visible_count = app.visible_issue_results().len();
        assert_eq!(app.selected.len(), visible_count);

        app.apply(SyncDashboardAction::SelectAll);
        assert!(app.selected.is_empty());
    }

    #[test]
    fn select_all_respects_search_filter() {
        let mut app = SyncDashboardApp::new(demo_data());
        app.query = InputFieldState::new("MET-11");

        let visible = app.visible_issue_results();
        let visible_count = visible.len();
        assert!(visible_count > 0);
        assert!(visible_count < app.data.issues.len());

        app.apply(SyncDashboardAction::SelectAll);
        assert_eq!(app.selected.len(), visible_count);

        for &idx in &app.selected {
            assert!(visible.iter().any(|r| r.issue_index == idx));
        }
    }

    // --- Filter tests ---

    #[test]
    fn status_filter_cycles_through_available_statuses() {
        let mut app = SyncDashboardApp::new(demo_data());
        let statuses = app.available_statuses();
        assert!(!statuses.is_empty());

        assert!(app.status_filter.is_none());
        app.apply(SyncDashboardAction::CycleStatusFilter);
        assert_eq!(app.status_filter, Some(statuses[0].clone()));

        for _ in 0..statuses.len() {
            app.apply(SyncDashboardAction::CycleStatusFilter);
        }
        assert!(app.status_filter.is_none());
    }

    #[test]
    fn status_filter_narrows_visible_results() {
        let mut app = SyncDashboardApp::new(demo_data());
        let all_visible = app.visible_issue_results().len();

        app.status_filter = Some("In Progress".to_string());
        let filtered = app.visible_issue_results().len();
        assert!(filtered > 0);
        assert!(filtered <= all_visible);
    }

    #[test]
    fn label_filter_cycles_through_available_labels() {
        let mut data = demo_data();
        data.issues[0].issue.labels = vec![LabelRef {
            id: "label-1".to_string(),
            name: "tech".to_string(),
        }];
        let mut app = SyncDashboardApp::new(data);

        let labels = app.available_labels();
        assert!(!labels.is_empty());

        assert!(app.label_filter.is_none());
        app.apply(SyncDashboardAction::CycleLabelFilter);
        assert_eq!(app.label_filter, Some(labels[0].clone()));

        app.apply(SyncDashboardAction::CycleLabelFilter);
        assert!(app.label_filter.is_none());
    }

    // --- Async loading tests ---

    #[test]
    fn loading_state_renders_placeholder() {
        let mut data = demo_data();
        data.issues[0].load_state = IssueLoadState::Loading;
        let exit = run_sync_dashboard(
            data,
            SyncDashboardOptions {
                render_once: true,
                width: 120,
                height: 36,
                actions: vec![],
                vim_mode: false,
            },
            None,
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };
        assert!(snapshot.contains("Loading..."));
    }

    #[test]
    fn issue_updates_replace_loading_entries() {
        let mut data = demo_data();
        let original = data.issues[0].clone();
        data.issues[0].load_state = IssueLoadState::Loading;
        data.issues[0].issue.title = "placeholder".to_string();

        let mut app = SyncDashboardApp::new(data);
        assert_eq!(app.data.issues[0].load_state, IssueLoadState::Loading);

        let (tx, rx) = mpsc::channel();
        tx.send(IssueUpdate {
            index: 0,
            issue: original.clone(),
        })
        .unwrap();
        drop(tx);

        app.drain_updates(&rx);
        assert_eq!(app.data.issues[0].load_state, IssueLoadState::Loaded);
        assert_eq!(app.data.issues[0].issue.title, original.issue.title);
    }

    // --- Repo-scoped config tests (config-level, covered separately) ---

    #[test]
    fn single_selection_remains_valid() {
        let mut app = SyncDashboardApp::new(demo_data());

        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Preview);
        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Actions);
        let selections = app.apply(SyncDashboardAction::Enter);

        assert_eq!(selections.len(), 1);
        assert_eq!(selections[0].issue_identifier, "MET-11");
        assert_eq!(selections[0].action, SyncSelectionAction::Pull);
    }

    // --- Search input stability tests ---

    #[test]
    fn enter_from_search_moves_to_list() {
        let mut app = SyncDashboardApp::new(demo_data());
        app.focus = Focus::Search;
        app.query = InputFieldState::new("MET");

        app.apply(SyncDashboardAction::Enter);
        assert_eq!(
            app.focus,
            Focus::List,
            "Enter from search should move focus to the results list"
        );
        // Query text is preserved so the user can continue interacting
        // with the filtered results.
        assert_eq!(app.query.value(), "MET");
    }

    #[test]
    fn back_clears_query_when_focus_is_search() {
        let mut app = SyncDashboardApp::new(demo_data());
        app.focus = Focus::Search;
        app.query = InputFieldState::new("search term");

        assert!(!app.query.value().is_empty());

        app.apply(SyncDashboardAction::Back);

        // Esc should clear the query first, staying in Search.
        assert_eq!(app.focus, Focus::Search);
        assert!(app.query.value().is_empty());

        // A second Esc with empty query returns to List.
        app.apply(SyncDashboardAction::Back);
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn back_navigates_when_query_is_empty() {
        let mut app = SyncDashboardApp::new(demo_data());
        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::Preview);

        // With empty query, Back should navigate back.
        app.apply(SyncDashboardAction::Back);
        assert_eq!(app.focus, Focus::List);
    }

    // --- Clear filters tests ---

    #[test]
    fn clear_filters_resets_status_and_label_filters() {
        let mut data = demo_data();
        data.issues[0].issue.labels = vec![LabelRef {
            id: "label-1".to_string(),
            name: "tech".to_string(),
        }];
        let mut app = SyncDashboardApp::new(data);

        app.status_filter = Some("In Progress".to_string());
        app.label_filter = Some("tech".to_string());
        assert!(app.status_filter.is_some());
        assert!(app.label_filter.is_some());

        app.apply(SyncDashboardAction::ClearFilters);

        assert!(app.status_filter.is_none());
        assert!(app.label_filter.is_none());
    }

    #[test]
    fn render_once_shows_filtered_indicator_in_issue_list() {
        let exit = run_sync_dashboard(
            demo_data(),
            SyncDashboardOptions {
                render_once: true,
                width: 140,
                height: 36,
                actions: vec![SyncDashboardAction::CycleStatusFilter],
                vim_mode: false,
            },
            None,
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };
        assert!(
            snapshot.contains("[filtered]"),
            "active filter should show [filtered] marker in issue list title"
        );
    }

    #[test]
    fn render_once_shows_active_filter_value_in_header() {
        let exit = run_sync_dashboard(
            demo_data(),
            SyncDashboardOptions {
                render_once: true,
                width: 140,
                height: 36,
                actions: vec![SyncDashboardAction::CycleStatusFilter],
                vim_mode: false,
            },
            None,
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };
        // When a status filter is active, the header should show the filter
        // value in brackets.
        let statuses = {
            let app = SyncDashboardApp::new(demo_data());
            app.available_statuses()
        };
        assert!(
            !statuses.is_empty(),
            "demo data should have at least one status"
        );
        let expected_filter = format!("[{}]", statuses[0]);
        assert!(
            snapshot.contains(&expected_filter),
            "header should contain active filter value {expected_filter}"
        );
    }

    // --- Loading/error state tests ---

    #[test]
    fn failed_load_state_renders_distinct_from_loading() {
        let mut data = demo_data();
        data.issues[0].load_state = IssueLoadState::Failed;
        let app = SyncDashboardApp::new(data);

        let preview = format!("{:?}", app.preview_text());
        // Failed issues should still be browseable, not stuck on "Loading...".
        assert!(
            !preview.contains("Loading issue data from Linear"),
            "failed issues should not show the Loading placeholder"
        );
    }

    #[test]
    fn out_of_range_update_index_is_ignored() {
        let data = demo_data();
        let issue_count = data.issues.len();
        let mut app = SyncDashboardApp::new(data);

        let (tx, rx) = mpsc::channel();
        tx.send(IssueUpdate {
            index: issue_count + 10,
            issue: app.data.issues[0].clone(),
        })
        .unwrap();
        drop(tx);

        // Should not panic on out-of-range index.
        app.drain_updates(&rx);
        assert_eq!(app.data.issues.len(), issue_count);
    }

    // --- Focus semantics regression tests (MET-108) ---

    #[test]
    fn space_in_search_focus_does_not_toggle_selection() {
        let mut app = SyncDashboardApp::new(demo_data());
        app.focus = Focus::Search;

        // Space in search focus should NOT toggle any item.
        assert!(app.selected.is_empty());
        app.apply(SyncDashboardAction::ToggleSelect);
        // ToggleSelect is a no-op when focus is Search.
        assert!(
            app.selected.is_empty(),
            "ToggleSelect must not select items when search is focused"
        );
    }

    #[test]
    fn space_in_list_focus_toggles_selection() {
        let mut app = SyncDashboardApp::new(demo_data());
        assert_eq!(app.focus, Focus::List);

        // Space in list focus should toggle the highlighted item.
        assert!(app.selected.is_empty());
        app.apply(SyncDashboardAction::ToggleSelect);
        assert_eq!(
            app.selected.len(),
            1,
            "ToggleSelect must select item when list is focused"
        );
    }

    #[test]
    fn space_in_list_with_active_query_still_toggles() {
        let mut app = SyncDashboardApp::new(demo_data());
        app.focus = Focus::List;
        app.query = InputFieldState::new("MET");

        // Even with an active search query, Space in list focus toggles.
        app.apply(SyncDashboardAction::ToggleSelect);
        assert_eq!(
            app.selected.len(),
            1,
            "ToggleSelect in list focus should work even with a non-empty query"
        );
        // Query should not be mutated.
        assert_eq!(app.query.value(), "MET");
    }

    #[test]
    fn focus_search_action_switches_to_search() {
        let mut app = SyncDashboardApp::new(demo_data());
        assert_eq!(app.focus, Focus::List);

        app.apply(SyncDashboardAction::FocusSearch);
        assert_eq!(
            app.focus,
            Focus::Search,
            "FocusSearch action should switch to Search focus"
        );
    }

    #[test]
    fn tab_cycles_through_all_focus_states() {
        let mut app = SyncDashboardApp::new(demo_data());
        assert_eq!(app.focus, Focus::List);

        app.apply(SyncDashboardAction::Tab);
        assert_eq!(app.focus, Focus::Preview);
        app.apply(SyncDashboardAction::Tab);
        assert_eq!(app.focus, Focus::Actions);
        app.apply(SyncDashboardAction::Tab);
        assert_eq!(app.focus, Focus::Search);
        app.apply(SyncDashboardAction::Tab);
        assert_eq!(app.focus, Focus::List);
    }

    #[test]
    fn initial_focus_is_list() {
        let app = SyncDashboardApp::new(demo_data());
        assert_eq!(
            app.focus,
            Focus::List,
            "dashboard should start with list focus so Space toggles immediately"
        );
    }

    #[test]
    fn render_once_footer_shows_focus_and_keys() {
        let exit = run_sync_dashboard(
            demo_data(),
            SyncDashboardOptions {
                render_once: true,
                width: 120,
                height: 36,
                actions: vec![],
                vim_mode: false,
            },
            None,
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };
        // Footer should be present with focus label and key hints.
        assert!(
            snapshot.contains("Focus: List"),
            "footer should show the current focus state"
        );
        assert!(
            snapshot.contains("Space"),
            "footer should show Space key hint in list focus"
        );
    }

    #[test]
    fn render_once_footer_reflects_search_focus() {
        let exit = run_sync_dashboard(
            demo_data(),
            SyncDashboardOptions {
                render_once: true,
                width: 120,
                height: 36,
                actions: vec![SyncDashboardAction::FocusSearch],
                vim_mode: false,
            },
            None,
        )
        .expect("render once should succeed");

        let SyncDashboardExit::Snapshot(snapshot) = exit else {
            panic!("render_once should return a snapshot");
        };
        assert!(
            snapshot.contains("Focus: Search"),
            "footer should reflect Search focus after FocusSearch action"
        );
        assert!(
            snapshot.contains("Backlog Search [focus]"),
            "search bar should show [focus] indicator when search is focused"
        );
    }

    #[test]
    fn search_then_list_then_space_selects_item() {
        let mut app = SyncDashboardApp::new(demo_data());

        // Enter search, type a query, then return to list and toggle.
        app.apply(SyncDashboardAction::FocusSearch);
        assert_eq!(app.focus, Focus::Search);

        // Simulate typing by setting query (handle_query_key is the
        // interactive path; the action-based tests verify focus routing).
        app.query = InputFieldState::new("MET-11");

        // Enter returns to list focus.
        app.apply(SyncDashboardAction::Enter);
        assert_eq!(app.focus, Focus::List);

        // Space now toggles in the filtered list.
        let visible = app.visible_issue_results();
        assert!(!visible.is_empty(), "query should match at least one entry");

        app.apply(SyncDashboardAction::ToggleSelect);
        assert!(
            !app.selected.is_empty(),
            "Space in list focus should toggle even after searching"
        );
        // Query unchanged.
        assert_eq!(app.query.value(), "MET-11");
    }
}

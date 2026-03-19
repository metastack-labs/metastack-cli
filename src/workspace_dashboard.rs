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
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::tui::fields::InputFieldState;
use crate::tui::theme::{
    Tone, badge, empty_state, key_hints, label_style, list, muted_style, panel_title, paragraph,
    tone_style,
};

#[derive(Debug, Clone)]
pub struct WorkspaceDashboardData {
    pub workspace_root: String,
    pub entries: Vec<WorkspaceDashboardEntry>,
    pub github_note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceDashboardEntry {
    pub ticket: String,
    pub branch: String,
    pub size: String,
    pub modified: String,
    pub git_label: String,
    pub git_clean: bool,
    pub linear_state: String,
    pub pr_label: String,
    pub is_removal_candidate: bool,
    pub has_unpushed: bool,
    pub has_uncommitted: bool,
    pub is_detached: bool,
}

#[derive(Debug, Clone)]
pub struct WorkspaceDashboardOptions {
    pub render_once: bool,
    pub width: u16,
    pub height: u16,
    pub actions: Vec<WorkspaceDashboardAction>,
}

#[derive(Debug, Clone, Copy)]
pub enum WorkspaceDashboardAction {
    Up,
    Down,
    Enter,
    Back,
    ToggleSelect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceSelectionAction {
    Clean,
    CleanTargets,
    Prune,
    PruneDryRun,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceSelection {
    pub tickets: Vec<String>,
    pub action: WorkspaceSelectionAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceDashboardExit {
    Snapshot(String),
    Cancelled,
    Selected(WorkspaceSelection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Workspaces,
    Actions,
}

#[derive(Debug, Clone)]
struct WorkspaceDashboardApp {
    data: WorkspaceDashboardData,
    focus: Focus,
    query: InputFieldState,
    workspace_index: usize,
    action_index: usize,
    selected: Vec<bool>,
    completed: Option<WorkspaceSelection>,
}

const ACTIONS: [WorkspaceSelectionAction; 4] = [
    WorkspaceSelectionAction::Clean,
    WorkspaceSelectionAction::CleanTargets,
    WorkspaceSelectionAction::PruneDryRun,
    WorkspaceSelectionAction::Prune,
];

/// Enrichment update sent from a background task to the dashboard event loop.
#[derive(Debug, Clone)]
pub struct WorkspaceEnrichmentUpdate {
    pub entries: Vec<WorkspaceDashboardEntry>,
    pub github_note: Option<String>,
}

pub fn run_workspace_dashboard(
    data: WorkspaceDashboardData,
    options: WorkspaceDashboardOptions,
    enrichment_rx: Option<std::sync::mpsc::Receiver<WorkspaceEnrichmentUpdate>>,
) -> Result<WorkspaceDashboardExit> {
    if options.render_once {
        return render_once(data, options).map(WorkspaceDashboardExit::Snapshot);
    }

    if !io::stdout().is_terminal() {
        bail!(
            "the interactive workspace dashboard requires a TTY; use `meta workspace list --root .` for scripted runs"
        );
    }

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = WorkspaceDashboardApp::new(data);

    loop {
        terminal.draw(|frame| render_dashboard(frame, &app))?;

        // Check for enrichment updates from background task
        if let Some(ref rx) = enrichment_rx {
            if let Ok(update) = rx.try_recv() {
                app.apply_enrichment(update);
            }
        }

        if event::poll(Duration::from_millis(150))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            // Ctrl+C always exits regardless of focus
            if key.code == KeyCode::Char('c')
                && key
                    .modifiers
                    .contains(crossterm::event::KeyModifiers::CONTROL)
            {
                return Ok(WorkspaceDashboardExit::Cancelled);
            }

            // Esc at top level exits, in action menu goes back
            if key.code == KeyCode::Esc {
                if app.focus == Focus::Workspaces {
                    return Ok(WorkspaceDashboardExit::Cancelled);
                } else {
                    let _ = app.apply(WorkspaceDashboardAction::Back);
                    continue;
                }
            }

            // Navigation and actions
            let action = match key.code {
                KeyCode::Up => Some(WorkspaceDashboardAction::Up),
                KeyCode::Down => Some(WorkspaceDashboardAction::Down),
                KeyCode::Enter => Some(WorkspaceDashboardAction::Enter),
                KeyCode::Char(' ') if app.focus == Focus::Workspaces => {
                    Some(WorkspaceDashboardAction::ToggleSelect)
                }
                _ => None,
            };

            if let Some(action) = action
                && let Some(selection) = app.apply(action)
            {
                return Ok(WorkspaceDashboardExit::Selected(selection));
            } else if action.is_none() {
                // Pass to search input (absorbs typed characters)
                let _ = app.handle_query_key(key);
            }
        }
    }
}

fn render_once(data: WorkspaceDashboardData, options: WorkspaceDashboardOptions) -> Result<String> {
    let backend = TestBackend::new(options.width, options.height);
    let mut terminal = Terminal::new(backend)?;
    let mut app = WorkspaceDashboardApp::new(data);
    for action in options.actions {
        let _ = app.apply(action);
    }

    terminal.draw(|frame| render_dashboard(frame, &app))?;
    Ok(snapshot(terminal.backend()))
}

fn render_dashboard(frame: &mut Frame<'_>, app: &WorkspaceDashboardApp) {
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
        .constraints(vec![Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(outer[2]);
    let details = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(55),
            Constraint::Length(8),
            Constraint::Min(6),
        ])
        .split(body[1]);

    let selected_count = app.selected.iter().filter(|s| **s).count();
    let total_size = app.total_selected_size();
    let header = paragraph(
        Text::from(vec![
            Line::from(format!("Workspace: {}", app.data.workspace_root)),
            Line::from(app.summary_line()),
            key_hints(&[
                ("Type", "search"),
                ("Up/Down", "move"),
                ("Space", "select"),
                ("Enter", "advance"),
                ("Esc", "back"),
                ("q", "exit"),
            ]),
        ]),
        panel_title("meta workspace", false),
    );
    frame.render_widget(header, outer[0]);

    let rendered_query = app.query.render(
        "Search by ticket, branch, state, or status...",
        app.focus == Focus::Workspaces,
    );
    let query_block =
        Block::default()
            .borders(Borders::ALL)
            .title(if app.focus == Focus::Workspaces {
                format!(
                    "Search [active] ({} selected, {})",
                    selected_count, total_size
                )
            } else {
                format!("Search ({} selected, {})", selected_count, total_size)
            });
    let query_inner = query_block.inner(outer[1]);
    let query = Paragraph::new(rendered_query.text.clone())
        .block(query_block)
        .wrap(Wrap { trim: false });
    frame.render_widget(query, outer[1]);
    rendered_query.set_cursor(frame, query_inner);

    render_workspace_list(frame, body[0], app);
    render_workspace_preview(frame, details[0], app);
    render_action_list(frame, details[1], app);
    render_status(frame, details[2], app);
}

fn render_workspace_list(frame: &mut Frame<'_>, area: Rect, app: &WorkspaceDashboardApp) {
    let results = app.visible_results();
    let title = panel_title(
        format!("Workspaces ({}/{})", results.len(), app.data.entries.len()),
        app.focus == Focus::Workspaces,
    );

    let items = if app.data.entries.is_empty() {
        vec![ListItem::new(empty_state(
            "No workspace clones found.",
            "Run `meta agents listen` to create workspace clones for tickets.",
        ))]
    } else if results.is_empty() {
        vec![ListItem::new(empty_state(
            "No workspaces match the current search.",
            "Clear or broaden the query.",
        ))]
    } else {
        results
            .iter()
            .filter_map(|idx| {
                app.data.entries.get(*idx).map(|entry| {
                    let selected_marker = if app.selected.get(*idx).copied().unwrap_or(false) {
                        Span::styled("[x] ", tone_style(Tone::Success))
                    } else {
                        Span::styled("[ ] ", muted_style())
                    };

                    let state_tone = if entry.is_removal_candidate {
                        Tone::Success
                    } else {
                        Tone::Info
                    };

                    let git_tone = if entry.git_clean {
                        Tone::Muted
                    } else {
                        Tone::Danger
                    };

                    ListItem::new(Text::from(vec![
                        Line::from(vec![
                            selected_marker,
                            Span::styled(
                                entry.ticket.clone(),
                                tone_style(Tone::Accent)
                                    .add_modifier(ratatui::style::Modifier::BOLD),
                            ),
                            Span::raw("  "),
                            badge(entry.linear_state.clone(), state_tone),
                            Span::raw("  "),
                            badge(entry.git_label.clone(), git_tone),
                        ]),
                        Line::from(vec![
                            Span::styled("    ", muted_style()),
                            Span::styled(entry.size.clone(), label_style()),
                            Span::styled("  ", muted_style()),
                            Span::styled(format!("PR: {}", entry.pr_label), muted_style()),
                            if entry.is_removal_candidate {
                                Span::styled("  safe to remove", tone_style(Tone::Success))
                            } else {
                                Span::raw("")
                            },
                        ]),
                    ]))
                })
            })
            .collect::<Vec<_>>()
    };

    let mut state = ListState::default();
    if results.is_empty() {
        state.select(Some(0));
    } else {
        state.select(Some(app.workspace_index.min(results.len() - 1)));
    }

    let list = list(items, title);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_workspace_preview(frame: &mut Frame<'_>, area: Rect, app: &WorkspaceDashboardApp) {
    let results = app.visible_results();
    let text = if let Some(&idx) = results.get(app.workspace_index) {
        let entry = &app.data.entries[idx];
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Ticket: ", label_style()),
                Span::raw(entry.ticket.clone()),
            ]),
            Line::from(vec![
                Span::styled("Branch: ", label_style()),
                Span::raw(entry.branch.clone()),
            ]),
            Line::from(vec![
                Span::styled("Size: ", label_style()),
                Span::raw(entry.size.clone()),
            ]),
            Line::from(vec![
                Span::styled("Modified: ", label_style()),
                Span::raw(entry.modified.clone()),
            ]),
            Line::from(vec![
                Span::styled("Linear: ", label_style()),
                Span::raw(entry.linear_state.clone()),
            ]),
            Line::from(vec![
                Span::styled("PR: ", label_style()),
                Span::raw(entry.pr_label.clone()),
            ]),
            Line::from(vec![
                Span::styled("Git: ", label_style()),
                Span::raw(entry.git_label.clone()),
            ]),
            Line::from(""),
        ];

        if entry.is_removal_candidate {
            lines.push(Line::from(Span::styled(
                "Safe to remove — ticket is Done/Cancelled.",
                tone_style(Tone::Success),
            )));
        }
        if entry.has_unpushed {
            lines.push(Line::from(Span::styled(
                "Warning: unpushed commits detected.",
                tone_style(Tone::Danger),
            )));
        }
        if entry.has_uncommitted {
            lines.push(Line::from(Span::styled(
                "Warning: uncommitted changes detected.",
                tone_style(Tone::Danger),
            )));
        }
        if entry.is_detached {
            lines.push(Line::from(Span::styled(
                "Warning: HEAD is detached.",
                tone_style(Tone::Danger),
            )));
        }

        Text::from(lines)
    } else {
        Text::from("No workspace selected.")
    };

    let preview = paragraph(text, panel_title("Workspace Details", false));
    frame.render_widget(preview, area);
}

fn render_action_list(frame: &mut Frame<'_>, area: Rect, app: &WorkspaceDashboardApp) {
    let title = panel_title("Action", app.focus == Focus::Actions);
    let items = ACTIONS
        .iter()
        .map(|action| {
            ListItem::new(Text::from(vec![
                Line::from(vec![badge(action.label(), Tone::Accent)]),
                Line::from(action.description()),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state = ListState::default();
    state.select(Some(app.action_index.min(ACTIONS.len() - 1)));

    let list = list(items, title);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_status(frame: &mut Frame<'_>, area: Rect, app: &WorkspaceDashboardApp) {
    let text = if let Some(ref note) = app.data.github_note {
        format!("{}\n\n{}", app.status_text(), note)
    } else {
        app.status_text()
    };
    let status = paragraph(text, panel_title("Status", false));
    frame.render_widget(status, area);
}

impl WorkspaceDashboardApp {
    fn new(data: WorkspaceDashboardData) -> Self {
        let entry_count = data.entries.len();
        Self {
            data,
            focus: Focus::Workspaces,
            query: InputFieldState::default(),
            workspace_index: 0,
            action_index: 0,
            selected: vec![false; entry_count],
            completed: None,
        }
    }

    fn apply_enrichment(&mut self, update: WorkspaceEnrichmentUpdate) {
        // Match enriched entries to existing entries by ticket ID
        for enriched in &update.entries {
            if let Some(existing) = self
                .data
                .entries
                .iter_mut()
                .find(|e| e.ticket == enriched.ticket)
            {
                existing.linear_state = enriched.linear_state.clone();
                existing.pr_label = enriched.pr_label.clone();
                existing.is_removal_candidate = enriched.is_removal_candidate;
            }
        }
        self.data.github_note = update.github_note;
        // Resize selected vec if entries changed
        self.selected.resize(self.data.entries.len(), false);
    }

    fn apply(&mut self, action: WorkspaceDashboardAction) -> Option<WorkspaceSelection> {
        self.completed = None;

        match action {
            WorkspaceDashboardAction::Up => match self.focus {
                Focus::Workspaces => {
                    let len = self.visible_results().len();
                    shift_index(&mut self.workspace_index, len, -1)
                }
                Focus::Actions => shift_index(&mut self.action_index, ACTIONS.len(), -1),
            },
            WorkspaceDashboardAction::Down => match self.focus {
                Focus::Workspaces => {
                    let len = self.visible_results().len();
                    shift_index(&mut self.workspace_index, len, 1)
                }
                Focus::Actions => shift_index(&mut self.action_index, ACTIONS.len(), 1),
            },
            WorkspaceDashboardAction::ToggleSelect => {
                let results = self.visible_results();
                if let Some(&idx) = results.get(self.workspace_index) {
                    if let Some(sel) = self.selected.get_mut(idx) {
                        *sel = !*sel;
                    }
                }
            }
            WorkspaceDashboardAction::Back => {
                if self.focus == Focus::Actions {
                    self.focus = Focus::Workspaces;
                    self.action_index = 0;
                }
            }
            WorkspaceDashboardAction::Enter => match self.focus {
                Focus::Workspaces => {
                    // If nothing explicitly selected, select the focused item
                    if self.selected.iter().all(|s| !s) {
                        let results = self.visible_results();
                        if let Some(&idx) = results.get(self.workspace_index) {
                            if let Some(sel) = self.selected.get_mut(idx) {
                                *sel = true;
                            }
                        }
                    }
                    if self.selected.iter().any(|s| *s) {
                        self.focus = Focus::Actions;
                    }
                }
                Focus::Actions => {
                    let tickets: Vec<String> = self
                        .selected
                        .iter()
                        .enumerate()
                        .filter(|(_, sel)| **sel)
                        .filter_map(|(idx, _)| self.data.entries.get(idx).map(|e| e.ticket.clone()))
                        .collect();

                    if tickets.is_empty() {
                        return None;
                    }

                    let selection = WorkspaceSelection {
                        tickets,
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
        if self.focus != Focus::Workspaces {
            return false;
        }
        if self.query.handle_key(key) {
            self.workspace_index = 0;
            return true;
        }
        false
    }

    fn visible_results(&self) -> Vec<usize> {
        let query = self.query.value().trim().to_lowercase();
        if query.is_empty() {
            return (0..self.data.entries.len()).collect();
        }

        self.data
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                entry.ticket.to_lowercase().contains(&query)
                    || entry.branch.to_lowercase().contains(&query)
                    || entry.linear_state.to_lowercase().contains(&query)
                    || entry.git_label.to_lowercase().contains(&query)
                    || entry.pr_label.to_lowercase().contains(&query)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    fn summary_line(&self) -> String {
        let selected_count = self.selected.iter().filter(|s| **s).count();
        let total = self.data.entries.len();
        let candidates = self
            .data
            .entries
            .iter()
            .filter(|e| e.is_removal_candidate)
            .count();

        match self.focus {
            Focus::Workspaces => {
                format!(
                    "{total} workspace clones. {candidates} safe to remove. {selected_count} selected."
                )
            }
            Focus::Actions => {
                format!("{selected_count} workspace(s) selected. Choose an action.")
            }
        }
    }

    fn total_selected_size(&self) -> String {
        let selected_entries: Vec<&str> = self
            .selected
            .iter()
            .enumerate()
            .filter(|(_, sel)| **sel)
            .filter_map(|(idx, _)| self.data.entries.get(idx).map(|e| e.size.as_str()))
            .collect();

        if selected_entries.is_empty() {
            "0 B".to_string()
        } else {
            format!("{} entries", selected_entries.len())
        }
    }

    fn status_text(&self) -> String {
        if let Some(selection) = &self.completed {
            return format!(
                "Ready to {} {} workspace(s).",
                selection.action.label(),
                selection.tickets.len()
            );
        }

        match self.focus {
            Focus::Workspaces => {
                "Step 1: Search, select workspaces with Space, then Enter to choose action."
                    .to_string()
            }
            Focus::Actions => "Step 2: Choose an action for the selected workspace(s).".to_string(),
        }
    }
}

impl WorkspaceSelectionAction {
    fn label(&self) -> &'static str {
        match self {
            Self::Clean => "Clean (remove clones)",
            Self::CleanTargets => "Clean targets only",
            Self::PruneDryRun => "Prune (dry run)",
            Self::Prune => "Prune (remove completed)",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::Clean => "Delete selected workspace clone(s) entirely.",
            Self::CleanTargets => "Remove target/ build directories to reclaim disk space.",
            Self::PruneDryRun => "Preview which completed clones would be removed.",
            Self::Prune => "Remove completed clones with merged/closed PRs.",
        }
    }
}

fn shift_index(index: &mut usize, len: usize, delta: isize) {
    if len == 0 {
        return;
    }
    let next = (*index as isize + delta).rem_euclid(len as isize) as usize;
    *index = next;
}

fn snapshot(backend: &TestBackend) -> String {
    let buffer = backend.buffer();
    let mut lines = Vec::new();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            let cell = &buffer[(x, y)];
            line.push_str(cell.symbol());
        }
        lines.push(line.trim_end().to_string());
    }

    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

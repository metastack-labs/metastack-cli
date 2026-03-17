use std::io;
use std::time::Duration;

use anyhow::{Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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

use super::WorkflowState;
use crate::tui::fields::InputFieldState;

#[derive(Debug, Clone)]
pub struct IssueEditFormContext {
    pub issue_identifier: String,
    pub team_key: String,
    pub team_name: String,
    pub current_project: Option<String>,
    pub pending_project: Option<String>,
    pub states: Vec<WorkflowState>,
}

#[derive(Debug, Clone)]
pub struct IssueEditFormPrefill {
    pub title: String,
    pub description: Option<String>,
    pub state: Option<String>,
    pub priority: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct IssueEditFormOptions {
    pub render_once: bool,
    pub width: u16,
    pub height: u16,
    pub actions: Vec<IssueEditAction>,
}

#[derive(Debug, Clone, Copy)]
pub enum IssueEditAction {
    Up,
    Down,
    Left,
    Right,
    Tab,
    BackTab,
    Enter,
    Esc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueEditValues {
    pub title: String,
    pub description: Option<String>,
    pub state: Option<String>,
    pub priority: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueEditFormExit {
    Cancelled,
    Submitted(IssueEditValues),
    Snapshot(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditStep {
    Title,
    Description,
    StatusPriority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusPriorityFocus {
    State,
    Priority,
}

#[derive(Debug, Clone, Copy)]
struct PriorityOption {
    value: Option<u8>,
    label: &'static str,
}

const PRIORITY_OPTIONS: [PriorityOption; 5] = [
    PriorityOption {
        value: None,
        label: "None (0)",
    },
    PriorityOption {
        value: Some(1),
        label: "Urgent (1)",
    },
    PriorityOption {
        value: Some(2),
        label: "High (2)",
    },
    PriorityOption {
        value: Some(3),
        label: "Normal (3)",
    },
    PriorityOption {
        value: Some(4),
        label: "Low (4)",
    },
];

#[derive(Debug, Clone)]
struct IssueEditApp {
    context: IssueEditFormContext,
    step: EditStep,
    step_focus: StatusPriorityFocus,
    title: InputFieldState,
    description: InputFieldState,
    selected_state: usize,
    selected_priority: usize,
    error: Option<String>,
}

pub fn run_issue_edit_form(
    context: IssueEditFormContext,
    prefill: IssueEditFormPrefill,
    options: IssueEditFormOptions,
) -> Result<IssueEditFormExit> {
    let mut app = IssueEditApp::new(context, prefill)?;

    if options.render_once {
        return render_once(app, options);
    }

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|frame| render_issue_edit_form(frame, &app))?;

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(exit) = app.handle_key(key) {
                        return Ok(exit);
                    }
                }
                Event::Paste(text) => app.handle_paste(&text),
                _ => {}
            }
        }
    }
}

fn render_once(mut app: IssueEditApp, options: IssueEditFormOptions) -> Result<IssueEditFormExit> {
    let backend = TestBackend::new(options.width, options.height);
    let mut terminal = Terminal::new(backend)?;

    for action in options.actions {
        let _ = app.apply_action(action);
    }

    terminal.draw(|frame| render_issue_edit_form(frame, &app))?;
    Ok(IssueEditFormExit::Snapshot(snapshot(terminal.backend())))
}

fn render_issue_edit_form(frame: &mut Frame<'_>, app: &IssueEditApp) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(24),
            Constraint::Min(0),
            Constraint::Length(34),
        ])
        .split(layout[1]);

    let header = Paragraph::new(Text::from(vec![
        Line::from(format!(
            "Edit Linear Issue ({})",
            app.context.issue_identifier.as_str()
        )),
        Line::from(format!(
            "Team: {} ({}) | Project: {}",
            app.context.team_name.as_str(),
            app.context.team_key.as_str(),
            app.context.project_label()
        )),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Issue Edit"));
    frame.render_widget(header, layout[0]);

    render_step_list(frame, app, body[0]);
    render_step_panel(frame, app, body[1]);
    render_summary(frame, app, body[2]);
    render_footer(frame, app, layout[2]);
}

fn render_step_list(frame: &mut Frame<'_>, app: &IssueEditApp, area: ratatui::layout::Rect) {
    let mut state = ListState::default();
    state.select(Some(app.step.index()));

    let items = EditStep::all()
        .into_iter()
        .map(|step| ListItem::new(step.label()))
        .collect::<Vec<_>>();
    let steps = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Fields"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(steps, area, &mut state);
}

fn render_step_panel(frame: &mut Frame<'_>, app: &IssueEditApp, area: ratatui::layout::Rect) {
    match app.step {
        EditStep::Title => {
            let rendered = app.title.render("Type the issue title...", true);
            let block = Block::default()
                .borders(Borders::ALL)
                .title("Title [editing]")
                .border_style(Style::default().add_modifier(Modifier::BOLD));
            let inner = block.inner(area);
            let paragraph = Paragraph::new(rendered.text.clone())
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
            rendered.set_cursor(frame, inner);
        }
        EditStep::Description => {
            let rendered = app
                .description
                .render("Type the issue description...", true);
            let block = Block::default()
                .borders(Borders::ALL)
                .title("Description [editing]")
                .border_style(Style::default().add_modifier(Modifier::BOLD));
            let inner = block.inner(area);
            let paragraph = Paragraph::new(rendered.text.clone())
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
            rendered.set_cursor(frame, inner);
        }
        EditStep::StatusPriority => {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                .split(area);

            render_states(frame, app, columns[0]);
            render_priorities(frame, app, columns[1]);
        }
    }
}

fn render_states(frame: &mut Frame<'_>, app: &IssueEditApp, area: ratatui::layout::Rect) {
    if app.context.states.is_empty() {
        let paragraph = Paragraph::new("No workflow states loaded from Linear.")
            .block(Block::default().borders(Borders::ALL).title("Status"));
        frame.render_widget(paragraph, area);
        return;
    }

    let mut list_state = ListState::default();
    list_state.select(Some(app.selected_state));
    let items = app
        .context
        .states
        .iter()
        .map(|state| ListItem::new(state.name.clone()))
        .collect::<Vec<_>>();
    let title = if app.step_focus == StatusPriorityFocus::State {
        "Status [focus]"
    } else {
        "Status"
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_priorities(frame: &mut Frame<'_>, app: &IssueEditApp, area: ratatui::layout::Rect) {
    let mut list_state = ListState::default();
    list_state.select(Some(app.selected_priority));
    let items = PRIORITY_OPTIONS
        .iter()
        .map(|priority| ListItem::new(priority.label))
        .collect::<Vec<_>>();
    let title = if app.step_focus == StatusPriorityFocus::Priority {
        "Priority [focus]"
    } else {
        "Priority"
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_summary(frame: &mut Frame<'_>, app: &IssueEditApp, area: ratatui::layout::Rect) {
    let description = if app.description.value().trim().is_empty() {
        "No description".to_string()
    } else {
        app.description
            .value()
            .lines()
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" / ")
    };
    let summary = vec![
        Line::from(format!("Issue: {}", app.context.issue_identifier.as_str())),
        Line::from(format!(
            "Title: {}",
            if app.title.value().trim().is_empty() {
                "Untitled issue"
            } else {
                app.title.value().trim()
            }
        )),
        Line::from(format!("Description: {description}")),
        Line::from(format!(
            "Status: {}",
            app.selected_state_name().unwrap_or("Unassigned")
        )),
        Line::from(format!("Priority: {}", app.selected_priority_label())),
        Line::from(format!("Project: {}", app.context.project_label())),
    ];
    let paragraph = Paragraph::new(Text::from(summary))
        .block(Block::default().borders(Borders::ALL).title("Review"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame<'_>, app: &IssueEditApp, area: ratatui::layout::Rect) {
    let controls = match app.step {
        EditStep::Title => {
            "Type the title. Tab/Shift+Tab or Up/Down switches fields. Enter moves to Description."
        }
        EditStep::Description => {
            "Type the description. Enter advances. Shift+Enter inserts a newline. Tab/Shift+Tab or Up/Down switches fields."
        }
        EditStep::StatusPriority => {
            "Use Up/Down in the active list. Left/Right switches focus. Enter submits. Esc cancels."
        }
    };

    let footer = if let Some(error) = &app.error {
        Text::from(vec![
            Line::from(controls),
            Line::from(format!("Error: {error}")),
        ])
    } else {
        Text::from(vec![
            Line::from(controls),
            Line::from("Esc cancels without updating the issue."),
        ])
    };

    let paragraph =
        Paragraph::new(footer).block(Block::default().borders(Borders::ALL).title("Controls"));
    frame.render_widget(paragraph, area);
}

impl IssueEditApp {
    fn new(context: IssueEditFormContext, prefill: IssueEditFormPrefill) -> Result<Self> {
        let selected_state = select_state_index(prefill.state.as_deref(), &context.states)?;
        let selected_priority = select_priority_index(prefill.priority);

        Ok(Self {
            context,
            step: EditStep::Title,
            step_focus: StatusPriorityFocus::State,
            title: InputFieldState::new(prefill.title),
            description: InputFieldState::multiline(prefill.description.unwrap_or_default()),
            selected_state,
            selected_priority,
            error: None,
        })
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<IssueEditFormExit> {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(IssueEditFormExit::Cancelled)
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.apply_shift_enter();
                None
            }
            KeyCode::Char(_) | KeyCode::Backspace => {
                self.apply_text_key(key);
                None
            }
            KeyCode::Up => self.apply_action(IssueEditAction::Up),
            KeyCode::Down => self.apply_action(IssueEditAction::Down),
            KeyCode::Left => self.apply_action(IssueEditAction::Left),
            KeyCode::Right => self.apply_action(IssueEditAction::Right),
            KeyCode::Tab => self.apply_action(IssueEditAction::Tab),
            KeyCode::BackTab => self.apply_action(IssueEditAction::BackTab),
            KeyCode::Enter => self.apply_action(IssueEditAction::Enter),
            KeyCode::Esc => self.apply_action(IssueEditAction::Esc),
            _ => None,
        }
    }

    fn apply_action(&mut self, action: IssueEditAction) -> Option<IssueEditFormExit> {
        match action {
            IssueEditAction::Up => {
                self.error = None;
                if self.step == EditStep::StatusPriority {
                    self.move_selection(-1);
                } else {
                    self.step = self.step.previous();
                }
                None
            }
            IssueEditAction::Down => {
                self.error = None;
                if self.step == EditStep::StatusPriority {
                    self.move_selection(1);
                } else {
                    self.step = self.step.next();
                }
                None
            }
            IssueEditAction::Left => {
                if self.step == EditStep::StatusPriority {
                    self.step_focus = StatusPriorityFocus::State;
                }
                None
            }
            IssueEditAction::Right => {
                if self.step == EditStep::StatusPriority {
                    self.step_focus = StatusPriorityFocus::Priority;
                }
                None
            }
            IssueEditAction::Tab => {
                self.error = None;
                self.step = self.step.next();
                None
            }
            IssueEditAction::BackTab => {
                self.error = None;
                self.step = self.step.previous();
                None
            }
            IssueEditAction::Enter => self.handle_enter(),
            IssueEditAction::Esc => Some(IssueEditFormExit::Cancelled),
        }
    }

    fn handle_enter(&mut self) -> Option<IssueEditFormExit> {
        self.error = None;
        match self.step {
            EditStep::Title => {
                self.step = EditStep::Description;
                None
            }
            EditStep::Description => {
                self.step = EditStep::StatusPriority;
                None
            }
            EditStep::StatusPriority => match self.build_submission() {
                Ok(values) => Some(IssueEditFormExit::Submitted(values)),
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.step = EditStep::Title;
                    None
                }
            },
        }
    }

    fn apply_shift_enter(&mut self) {
        self.error = None;
        if self.step == EditStep::Description {
            let _ = self.description.insert_newline();
        }
    }

    fn apply_text_key(&mut self, key: KeyEvent) {
        self.error = None;
        match self.step {
            EditStep::Title => {
                let _ = self.title.handle_key(key);
            }
            EditStep::Description => {
                let _ = self.description.handle_key(key);
            }
            EditStep::StatusPriority => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        self.error = None;
        match self.step {
            EditStep::Title => {
                let _ = self.title.paste(text);
            }
            EditStep::Description => {
                let _ = self.description.paste(text);
            }
            EditStep::StatusPriority => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.step != EditStep::StatusPriority {
            return;
        }

        match self.step_focus {
            StatusPriorityFocus::State => {
                shift_index(&mut self.selected_state, self.context.states.len(), delta)
            }
            StatusPriorityFocus::Priority => {
                shift_index(&mut self.selected_priority, PRIORITY_OPTIONS.len(), delta)
            }
        }
    }

    fn build_submission(&self) -> Result<IssueEditValues> {
        let title = self.title.value().trim();
        if title.is_empty() {
            return Err(anyhow!("Title is required."));
        }

        let description = self.description.value().trim();
        Ok(IssueEditValues {
            title: title.to_string(),
            description: (!description.is_empty())
                .then_some(self.description.value().trim_end().to_string()),
            state: self.selected_state_name().map(str::to_string),
            priority: PRIORITY_OPTIONS[self.selected_priority].value,
        })
    }

    fn selected_state_name(&self) -> Option<&str> {
        self.context
            .states
            .get(self.selected_state)
            .map(|state| state.name.as_str())
    }

    fn selected_priority_label(&self) -> &'static str {
        PRIORITY_OPTIONS[self.selected_priority].label
    }
}

impl IssueEditFormContext {
    fn project_label(&self) -> String {
        match (&self.current_project, &self.pending_project) {
            (Some(current), Some(pending)) if !current.eq_ignore_ascii_case(pending) => {
                format!("{current} -> {pending}")
            }
            (_, Some(pending)) => pending.clone(),
            (Some(current), None) => current.clone(),
            (None, None) => "none".to_string(),
        }
    }
}

impl EditStep {
    fn all() -> [Self; 3] {
        [Self::Title, Self::Description, Self::StatusPriority]
    }

    fn index(self) -> usize {
        match self {
            Self::Title => 0,
            Self::Description => 1,
            Self::StatusPriority => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Title => "1. Title",
            Self::Description => "2. Description",
            Self::StatusPriority => "3. Status / Priority",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Title => Self::Description,
            Self::Description => Self::StatusPriority,
            Self::StatusPriority => Self::StatusPriority,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Title => Self::Title,
            Self::Description => Self::Title,
            Self::StatusPriority => Self::Description,
        }
    }
}

fn select_state_index(prefill: Option<&str>, states: &[WorkflowState]) -> Result<usize> {
    if states.is_empty() {
        return Ok(0);
    }

    if let Some(name) = prefill {
        return states
            .iter()
            .position(|state| state.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| anyhow!("state `{name}` was not found on the selected team"));
    }

    Ok(states
        .iter()
        .position(|state| state.kind.as_deref() == Some("unstarted"))
        .unwrap_or(0))
}

fn select_priority_index(priority: Option<u8>) -> usize {
    match priority {
        None | Some(0) => 0,
        Some(value) => PRIORITY_OPTIONS
            .iter()
            .position(|option| option.value == Some(value))
            .unwrap_or(0),
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
        EditStep, IssueEditAction, IssueEditApp, IssueEditFormContext, IssueEditFormExit,
        IssueEditFormPrefill, IssueEditValues,
    };
    use crate::linear::WorkflowState;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn context() -> IssueEditFormContext {
        IssueEditFormContext {
            issue_identifier: "MET-11".to_string(),
            team_key: "MET".to_string(),
            team_name: "Metastack".to_string(),
            current_project: Some("MetaStack CLI".to_string()),
            pending_project: None,
            states: vec![
                WorkflowState {
                    id: "state-1".to_string(),
                    name: "Todo".to_string(),
                    kind: Some("unstarted".to_string()),
                },
                WorkflowState {
                    id: "state-2".to_string(),
                    name: "In Progress".to_string(),
                    kind: Some("started".to_string()),
                },
            ],
        }
    }

    #[test]
    fn issue_edit_app_tracks_input_and_submit_state() {
        let mut app = IssueEditApp::new(
            context(),
            IssueEditFormPrefill {
                title: "Add docs".to_string(),
                description: Some("Ship it".to_string()),
                state: Some("Todo".to_string()),
                priority: Some(1),
            },
        )
        .expect("app should build");

        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let _ = app.apply_action(IssueEditAction::Right);
        let _ = app.apply_action(IssueEditAction::Down);

        let exit = app.apply_action(IssueEditAction::Enter);
        assert_eq!(
            exit,
            Some(IssueEditFormExit::Submitted(IssueEditValues {
                title: "Add docs!".to_string(),
                description: Some("Ship it!".to_string()),
                state: Some("Todo".to_string()),
                priority: Some(2),
            }))
        );
    }

    #[test]
    fn issue_edit_app_prefills_state_and_priority() {
        let app = IssueEditApp::new(
            context(),
            IssueEditFormPrefill {
                title: "Add docs".to_string(),
                description: None,
                state: Some("In Progress".to_string()),
                priority: Some(2),
            },
        )
        .expect("app should build");

        assert_eq!(app.selected_state_name(), Some("In Progress"));
        assert_eq!(app.selected_priority_label(), "High (2)");
    }

    #[test]
    fn issue_edit_app_requires_title_before_submit() {
        let mut app = IssueEditApp::new(
            context(),
            IssueEditFormPrefill {
                title: String::new(),
                description: None,
                state: Some("Todo".to_string()),
                priority: Some(1),
            },
        )
        .expect("app should build");
        app.step = EditStep::StatusPriority;

        let exit = app.apply_action(IssueEditAction::Enter);

        assert!(exit.is_none());
        assert_eq!(app.step, EditStep::Title);
        assert_eq!(app.error.as_deref(), Some("Title is required."));
    }

    #[test]
    fn issue_edit_app_shift_enter_adds_newline_in_description() {
        let mut app = IssueEditApp::new(
            context(),
            IssueEditFormPrefill {
                title: "Add docs".to_string(),
                description: Some("Ship it".to_string()),
                state: Some("Todo".to_string()),
                priority: Some(1),
            },
        )
        .expect("app should build");
        app.step = EditStep::Description;

        let exit = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        assert_eq!(exit, None);
        assert_eq!(app.description.value(), "Ship it\n");
        assert_eq!(app.step, EditStep::Description);
    }

    #[test]
    fn issue_edit_app_enter_advances_from_description_to_status_priority() {
        let mut app = IssueEditApp::new(
            context(),
            IssueEditFormPrefill {
                title: "Add docs".to_string(),
                description: Some("Ship it".to_string()),
                state: Some("Todo".to_string()),
                priority: Some(1),
            },
        )
        .expect("app should build");
        app.step = EditStep::Description;

        let exit = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(exit, None);
        assert_eq!(app.description.value(), "Ship it");
        assert_eq!(app.step, EditStep::StatusPriority);
    }

    #[test]
    fn issue_edit_app_paste_preserves_multiline_description() {
        let mut app = IssueEditApp::new(
            context(),
            IssueEditFormPrefill {
                title: "Add docs".to_string(),
                description: None,
                state: Some("Todo".to_string()),
                priority: Some(1),
            },
        )
        .expect("app should build");
        app.step = EditStep::Description;

        app.handle_paste("Line one\nLine two\n");

        assert_eq!(app.description.value(), "Line one\nLine two\n");
        assert_eq!(app.error, None);
    }

    #[test]
    fn issue_edit_app_paste_normalizes_multiline_title_to_single_line() {
        let mut app = IssueEditApp::new(
            context(),
            IssueEditFormPrefill {
                title: String::new(),
                description: None,
                state: Some("Todo".to_string()),
                priority: Some(1),
            },
        )
        .expect("app should build");
        app.step = EditStep::Title;

        app.handle_paste("Line one\nLine two\n");

        assert_eq!(app.title.value(), "Line one Line two");
        assert_eq!(app.error, None);
    }
}

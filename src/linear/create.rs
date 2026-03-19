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
use crate::tui::fields::{InputFieldState, SelectFieldState};

#[derive(Debug, Clone)]
pub struct IssueCreateFormContext {
    pub team_key: String,
    pub team_name: String,
    pub project: Option<String>,
    pub states: Vec<WorkflowState>,
}

#[derive(Debug, Clone, Default)]
pub struct IssueCreateFormPrefill {
    pub title: Option<String>,
    pub description: Option<String>,
    pub state: Option<String>,
    pub priority: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct IssueCreateFormOptions {
    pub render_once: bool,
    pub width: u16,
    pub height: u16,
    pub actions: Vec<IssueCreateAction>,
}

#[derive(Debug, Clone, Copy)]
pub enum IssueCreateAction {
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
pub struct IssueCreateValues {
    pub title: String,
    pub description: Option<String>,
    pub state: Option<String>,
    pub priority: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssueCreateFormExit {
    Cancelled,
    Submitted(IssueCreateValues),
    Snapshot(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreateStep {
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
struct IssueCreateApp {
    context: IssueCreateFormContext,
    step: CreateStep,
    step_focus: StatusPriorityFocus,
    title: InputFieldState,
    description: InputFieldState,
    state_field: SelectFieldState,
    priority_field: SelectFieldState,
    error: Option<String>,
}

pub fn run_issue_create_form(
    context: IssueCreateFormContext,
    prefill: IssueCreateFormPrefill,
    options: IssueCreateFormOptions,
) -> Result<IssueCreateFormExit> {
    let mut app = IssueCreateApp::new(context, prefill)?;

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
        terminal.draw(|frame| render_issue_create_form(frame, &app))?;

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

fn render_once(
    mut app: IssueCreateApp,
    options: IssueCreateFormOptions,
) -> Result<IssueCreateFormExit> {
    let backend = TestBackend::new(options.width, options.height);
    let mut terminal = Terminal::new(backend)?;

    for action in options.actions {
        let _ = app.apply_action(action);
    }

    terminal.draw(|frame| render_issue_create_form(frame, &app))?;
    Ok(IssueCreateFormExit::Snapshot(snapshot(terminal.backend())))
}

fn render_issue_create_form(frame: &mut Frame<'_>, app: &IssueCreateApp) {
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
            "Create Linear Issue ({})",
            app.context.team_key.as_str()
        )),
        Line::from(format!(
            "Team: {} | Project: {}",
            app.context.team_name.as_str(),
            app.context.project.as_deref().unwrap_or("none")
        )),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Issue Create"));
    frame.render_widget(header, layout[0]);

    render_step_list(frame, app, body[0]);
    render_step_panel(frame, app, body[1]);
    render_summary(frame, app, body[2]);
    render_footer(frame, app, layout[2]);
}

fn render_step_list(frame: &mut Frame<'_>, app: &IssueCreateApp, area: ratatui::layout::Rect) {
    let mut state = ListState::default();
    state.select(Some(app.step.index()));

    let items = CreateStep::all()
        .into_iter()
        .map(|step| ListItem::new(step.label()))
        .collect::<Vec<_>>();
    let steps = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Steps"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(steps, area, &mut state);
}

fn render_step_panel(frame: &mut Frame<'_>, app: &IssueCreateApp, area: ratatui::layout::Rect) {
    match app.step {
        CreateStep::Title => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title("Step 1 of 3: Title [editing]")
                .border_style(Style::default().add_modifier(Modifier::BOLD));
            let inner = block.inner(area);
            let rendered =
                app.title
                    .render_with_width("Type the issue title...", true, inner.width);
            let paragraph = Paragraph::new(rendered.text.clone())
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
            rendered.set_cursor(frame, inner);
        }
        CreateStep::Description => {
            let block = Block::default()
                .borders(Borders::ALL)
                .title("Step 2 of 3: Description [editing]")
                .border_style(Style::default().add_modifier(Modifier::BOLD));
            let inner = block.inner(area);
            let rendered = app.description.render_with_width(
                "Type the issue description...",
                true,
                inner.width,
            );
            let paragraph = Paragraph::new(rendered.text.clone())
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
            rendered.set_cursor(frame, inner);
        }
        CreateStep::StatusPriority => {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                .split(area);

            render_states(frame, app, columns[0]);
            render_priorities(frame, app, columns[1]);
        }
    }
}

fn render_states(frame: &mut Frame<'_>, app: &IssueCreateApp, area: ratatui::layout::Rect) {
    if app.context.states.is_empty() {
        let paragraph = Paragraph::new("No workflow states loaded from Linear.").block(
            Block::default()
                .borders(Borders::ALL)
                .title("Step 3 of 3: State"),
        );
        frame.render_widget(paragraph, area);
        return;
    }

    let mut list_state = ListState::default();
    list_state.select(Some(app.state_field.selected()));
    let items = app
        .state_field
        .options()
        .iter()
        .map(|state| ListItem::new(state.clone()))
        .collect::<Vec<_>>();
    let title = if app.step_focus == StatusPriorityFocus::State {
        "Step 3 of 3: State [focus]"
    } else {
        "Step 3 of 3: State"
    };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_priorities(frame: &mut Frame<'_>, app: &IssueCreateApp, area: ratatui::layout::Rect) {
    let mut list_state = ListState::default();
    list_state.select(Some(app.priority_field.selected()));
    let items = app
        .priority_field
        .options()
        .iter()
        .map(|priority| ListItem::new(priority.clone()))
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

fn render_summary(frame: &mut Frame<'_>, app: &IssueCreateApp, area: ratatui::layout::Rect) {
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
            "State: {}",
            app.selected_state_name().unwrap_or("Unassigned")
        )),
        Line::from(format!("Priority: {}", app.selected_priority_label())),
    ];
    let paragraph = Paragraph::new(Text::from(summary))
        .block(Block::default().borders(Borders::ALL).title("Summary"))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut Frame<'_>, app: &IssueCreateApp, area: ratatui::layout::Rect) {
    let controls = match app.step {
        CreateStep::Title => "Type the title. Enter or Tab advances. Esc cancels the create flow.",
        CreateStep::Description => {
            "Type the description. Enter advances. Shift+Enter inserts a newline. Tab advances. Shift+Tab goes back."
        }
        CreateStep::StatusPriority => {
            "Use Up/Down in the active list. Left/Right switches focus. Enter submits. Shift+Tab goes back."
        }
    };

    let footer = if let Some(error) = &app.error {
        Text::from(vec![
            Line::from(controls),
            Line::from(format!("Error: {error}")),
        ])
    } else {
        Text::from(vec![Line::from(controls), Line::from("Ready.")])
    };

    let paragraph =
        Paragraph::new(footer).block(Block::default().borders(Borders::ALL).title("Controls"));
    frame.render_widget(paragraph, area);
}

impl IssueCreateApp {
    fn new(context: IssueCreateFormContext, prefill: IssueCreateFormPrefill) -> Result<Self> {
        let selected_state = select_state_index(prefill.state.as_deref(), &context.states)?;
        let selected_priority = select_priority_index(prefill.priority);
        let state_options = context
            .states
            .iter()
            .map(|state| state.name.clone())
            .collect();
        let priority_options = PRIORITY_OPTIONS
            .iter()
            .map(|priority| priority.label.to_string())
            .collect();

        Ok(Self {
            context,
            step: CreateStep::Title,
            step_focus: StatusPriorityFocus::State,
            title: InputFieldState::new(prefill.title.unwrap_or_default()),
            description: InputFieldState::multiline(prefill.description.unwrap_or_default()),
            state_field: SelectFieldState::new(state_options, selected_state),
            priority_field: SelectFieldState::new(priority_options, selected_priority),
            error: None,
        })
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<IssueCreateFormExit> {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(IssueCreateFormExit::Cancelled)
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.apply_shift_enter();
                None
            }
            KeyCode::Char(_) | KeyCode::Backspace => {
                self.apply_text_key(key);
                None
            }
            KeyCode::Up => self.apply_action(IssueCreateAction::Up),
            KeyCode::Down => self.apply_action(IssueCreateAction::Down),
            KeyCode::Left => self.apply_action(IssueCreateAction::Left),
            KeyCode::Right => self.apply_action(IssueCreateAction::Right),
            KeyCode::Tab => self.apply_action(IssueCreateAction::Tab),
            KeyCode::BackTab => self.apply_action(IssueCreateAction::BackTab),
            KeyCode::Enter => self.apply_action(IssueCreateAction::Enter),
            KeyCode::Esc => self.apply_action(IssueCreateAction::Esc),
            _ => None,
        }
    }

    fn apply_action(&mut self, action: IssueCreateAction) -> Option<IssueCreateFormExit> {
        match action {
            IssueCreateAction::Up => {
                self.move_selection(-1);
                None
            }
            IssueCreateAction::Down => {
                self.move_selection(1);
                None
            }
            IssueCreateAction::Left => {
                if self.step == CreateStep::StatusPriority {
                    self.step_focus = StatusPriorityFocus::State;
                }
                None
            }
            IssueCreateAction::Right => {
                if self.step == CreateStep::StatusPriority {
                    self.step_focus = StatusPriorityFocus::Priority;
                }
                None
            }
            IssueCreateAction::Tab => {
                self.error = None;
                self.step = self.step.next();
                None
            }
            IssueCreateAction::BackTab => {
                self.error = None;
                self.step = self.step.previous();
                None
            }
            IssueCreateAction::Enter => self.handle_enter(),
            IssueCreateAction::Esc => Some(IssueCreateFormExit::Cancelled),
        }
    }

    fn handle_enter(&mut self) -> Option<IssueCreateFormExit> {
        self.error = None;
        match self.step {
            CreateStep::Title => {
                self.step = CreateStep::Description;
                None
            }
            CreateStep::Description => {
                self.step = CreateStep::StatusPriority;
                None
            }
            CreateStep::StatusPriority => match self.build_submission() {
                Ok(values) => Some(IssueCreateFormExit::Submitted(values)),
                Err(error) => {
                    self.error = Some(error.to_string());
                    self.step = CreateStep::Title;
                    None
                }
            },
        }
    }

    fn apply_shift_enter(&mut self) {
        self.error = None;
        if self.step == CreateStep::Description {
            let _ = self.description.insert_newline();
        }
    }

    fn apply_text_key(&mut self, key: KeyEvent) {
        self.error = None;
        match self.step {
            CreateStep::Title => {
                let _ = self.title.handle_key(key);
            }
            CreateStep::Description => {
                let _ = self.description.handle_key(key);
            }
            CreateStep::StatusPriority => {}
        }
    }

    fn handle_paste(&mut self, text: &str) {
        self.error = None;
        match self.step {
            CreateStep::Title => {
                let _ = self.title.paste(text);
            }
            CreateStep::Description => {
                let _ = self.description.paste(text);
            }
            CreateStep::StatusPriority => {}
        }
    }

    fn move_selection(&mut self, delta: isize) {
        self.error = None;
        if self.step != CreateStep::StatusPriority {
            return;
        }

        match self.step_focus {
            StatusPriorityFocus::State => self.state_field.move_by(delta),
            StatusPriorityFocus::Priority => self.priority_field.move_by(delta),
        }
    }

    fn build_submission(&self) -> Result<IssueCreateValues> {
        let title = self.title.value().trim();
        if title.is_empty() {
            return Err(anyhow!("Title is required."));
        }

        let description = self.description.value().trim();
        Ok(IssueCreateValues {
            title: title.to_string(),
            description: (!description.is_empty())
                .then_some(self.description.value().trim_end().to_string()),
            state: self.selected_state_name().map(str::to_string),
            priority: PRIORITY_OPTIONS[self.priority_field.selected()].value,
        })
    }

    fn selected_state_name(&self) -> Option<&str> {
        self.state_field.selected_label()
    }

    fn selected_priority_label(&self) -> &'static str {
        PRIORITY_OPTIONS[self.priority_field.selected()].label
    }
}

impl CreateStep {
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
        CreateStep, IssueCreateAction, IssueCreateApp, IssueCreateFormContext, IssueCreateFormExit,
        IssueCreateFormPrefill,
    };
    use crate::linear::WorkflowState;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn context() -> IssueCreateFormContext {
        IssueCreateFormContext {
            team_key: "MET".to_string(),
            team_name: "Metastack".to_string(),
            project: Some("MetaStack CLI".to_string()),
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
    fn issue_create_app_tracks_input_and_submit_state() {
        let mut app = IssueCreateApp::new(context(), IssueCreateFormPrefill::default())
            .expect("app should build");

        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('N'), KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        let _ = app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let _ = app.apply_action(IssueCreateAction::Right);
        let _ = app.apply_action(IssueCreateAction::Down);

        let exit = app.apply_action(IssueCreateAction::Enter);
        assert_eq!(
            exit,
            Some(IssueCreateFormExit::Submitted(super::IssueCreateValues {
                title: "Add".to_string(),
                description: Some("No\nt".to_string()),
                state: Some("Todo".to_string()),
                priority: Some(1),
            }))
        );
    }

    #[test]
    fn issue_create_app_prefills_state_and_priority() {
        let app = IssueCreateApp::new(
            context(),
            IssueCreateFormPrefill {
                title: Some("Add docs".to_string()),
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
    fn issue_create_app_requires_title_before_submit() {
        let mut app = IssueCreateApp::new(context(), IssueCreateFormPrefill::default())
            .expect("app should build");
        app.step = CreateStep::StatusPriority;

        let exit = app.apply_action(IssueCreateAction::Enter);

        assert!(exit.is_none());
        assert_eq!(app.step, CreateStep::Title);
        assert_eq!(app.error.as_deref(), Some("Title is required."));
    }

    #[test]
    fn issue_create_app_paste_preserves_multiline_description() {
        let mut app = IssueCreateApp::new(context(), IssueCreateFormPrefill::default())
            .expect("app should build");
        app.step = CreateStep::Description;

        app.handle_paste("Line one\nLine two\n");

        assert_eq!(app.description.value(), "Line one\nLine two\n");
        assert_eq!(app.error, None);
    }

    #[test]
    fn issue_create_app_shift_enter_adds_newline_in_description() {
        let mut app = IssueCreateApp::new(context(), IssueCreateFormPrefill::default())
            .expect("app should build");
        app.step = CreateStep::Description;

        let exit = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        assert_eq!(exit, None);
        assert_eq!(app.description.value(), "\n");
        assert_eq!(app.step, CreateStep::Description);
    }

    #[test]
    fn issue_create_app_enter_advances_from_description_to_status_priority() {
        let mut app = IssueCreateApp::new(context(), IssueCreateFormPrefill::default())
            .expect("app should build");
        app.step = CreateStep::Description;

        let exit = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(exit, None);
        assert_eq!(app.description.value(), "");
        assert_eq!(app.step, CreateStep::StatusPriority);
    }
}

use std::io;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use cron::Schedule;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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

use crate::tui::fields::{InputFieldState, SelectFieldState};

const NONE_AGENT_LABEL: &str = "None";
const ENABLED_OPTIONS: [&str; 2] = ["Enabled", "Disabled"];

#[derive(Debug, Clone)]
pub(crate) struct CronInitFormContext {
    pub(crate) agent_options: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CronInitFormPrefill {
    pub(crate) name: Option<String>,
    pub(crate) schedule: Option<String>,
    pub(crate) command: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) prompt: Option<String>,
    pub(crate) shell: Option<String>,
    pub(crate) working_directory: Option<String>,
    pub(crate) timeout_seconds: Option<u64>,
    pub(crate) disabled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CronInitFormOptions {
    pub(crate) render_once: bool,
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) actions: Vec<CronInitAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CronInitFormValues {
    pub(crate) name: String,
    pub(crate) schedule: String,
    pub(crate) command: String,
    pub(crate) agent: Option<String>,
    pub(crate) prompt: Option<String>,
    pub(crate) shell: String,
    pub(crate) working_directory: String,
    pub(crate) timeout_seconds: u64,
    pub(crate) enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CronInitFormExit {
    Cancelled,
    Submitted(CronInitFormValues),
    Snapshot(String),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CronInitAction {
    Up,
    Down,
    Left,
    Right,
    Tab,
    BackTab,
    Save,
    Esc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchedulePreset {
    EveryMinutes,
    EveryHours,
    DailyAtTime,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CronField {
    Name,
    SchedulePreset,
    MinutesInterval,
    HourInterval,
    HourlyMinute,
    DailyHour,
    DailyMinute,
    CustomSchedule,
    Command,
    Agent,
    Prompt,
    WorkingDirectory,
    Shell,
    TimeoutSeconds,
    Enabled,
    Save,
}

#[derive(Debug, Clone)]
struct CronInitApp {
    focus: CronField,
    name: InputFieldState,
    schedule_preset: SelectFieldState,
    minutes_interval: InputFieldState,
    hour_interval: InputFieldState,
    hourly_minute: InputFieldState,
    daily_hour: InputFieldState,
    daily_minute: InputFieldState,
    custom_schedule: InputFieldState,
    command: InputFieldState,
    agent: SelectFieldState,
    prompt: InputFieldState,
    working_directory: InputFieldState,
    shell: InputFieldState,
    timeout_seconds: InputFieldState,
    enabled: SelectFieldState,
    error: Option<String>,
}

pub(crate) fn run_cron_init_form(
    context: CronInitFormContext,
    prefill: CronInitFormPrefill,
    options: CronInitFormOptions,
) -> Result<CronInitFormExit> {
    let mut app = CronInitApp::new(context, prefill);

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
        terminal.draw(|frame| render_cron_init_form(frame, &app))?;

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

fn render_once(mut app: CronInitApp, options: CronInitFormOptions) -> Result<CronInitFormExit> {
    let backend = TestBackend::new(options.width, options.height);
    let mut terminal = Terminal::new(backend)?;

    for action in options.actions {
        app.apply_action(action);
    }

    terminal.draw(|frame| render_cron_init_form(frame, &app))?;
    Ok(CronInitFormExit::Snapshot(snapshot(terminal.backend())))
}

fn render_cron_init_form(frame: &mut Frame<'_>, app: &CronInitApp) {
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
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(layout[1]);

    let header = Paragraph::new(Text::from(vec![
        Line::from("Cron Init Dashboard"),
        Line::from(
            "Configure a schedule preset, an optional shell command, and an optional recurring agent prompt.",
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title("meta cron init"));
    frame.render_widget(header, layout[0]);

    render_form_fields(frame, app, body[0]);
    render_preview(frame, app, body[1]);
    render_footer(frame, app, layout[2]);
}

fn render_form_fields(frame: &mut Frame<'_>, app: &CronInitApp, area: Rect) {
    let mut state = ListState::default();
    state.select(Some(app.focus.index()));

    let items = CronField::all()
        .iter()
        .map(|field| {
            ListItem::new(Line::from(format!(
                "{}: {}",
                field.label(),
                app.field_value(*field)
            )))
        })
        .collect::<Vec<_>>();
    let fields = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Fields"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(fields, area, &mut state);
}

fn render_preview(frame: &mut Frame<'_>, app: &CronInitApp, area: Rect) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(11), Constraint::Min(0)])
        .split(area);

    let schedule_summary = app
        .resolved_schedule()
        .unwrap_or_else(|error| format!("Invalid schedule: {error}"));
    let agent_summary = match (
        app.selected_agent(),
        normalized_optional(app.prompt.value()),
    ) {
        (Some(agent), Some(_)) => format!("Enabled via `{agent}`"),
        (_, Some(_)) => "Prompt set, but no agent selected".to_string(),
        _ => "Disabled until a prompt is provided".to_string(),
    };
    let preview = Paragraph::new(Text::from(vec![
        Line::from(format!(
            "Job file: .metastack/cron/{}.md",
            if app.name.value().trim().is_empty() {
                "<name>"
            } else {
                app.name.value().trim()
            }
        )),
        Line::from(format!("Generated schedule: {schedule_summary}")),
        Line::from(format!(
            "Command: {}",
            empty_placeholder(app.command.value(), "<optional>")
        )),
        Line::from(format!("Agent phase: {agent_summary}")),
        Line::from(format!(
            "Working directory: {}",
            empty_placeholder(app.working_directory.value(), ".")
        )),
        Line::from(format!(
            "Timeout / shell: {}s via {}",
            empty_placeholder(app.timeout_seconds.value(), "900"),
            empty_placeholder(app.shell.value(), "/bin/sh")
        )),
        Line::from(format!(
            "Enabled: {}",
            app.enabled.selected_label().unwrap_or("Enabled")
        )),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Preview"))
    .wrap(Wrap { trim: false });
    frame.render_widget(preview, sections[0]);

    let active_help = Paragraph::new(Text::from(vec![
        Line::from(app.focus.help_text()),
        Line::from(""),
        Line::from("Execution contract:"),
        Line::from(
            "- When configured, the shell command runs first inside the configured working directory.",
        ),
        Line::from("- When both agent and prompt are configured, the agent runs after the shell phase."),
        Line::from(
            "- The agent receives cron execution context through METASTACK_CRON_* env vars and an augmented prompt.",
        ),
        Line::from(""),
        Line::from("Prompt preview:"),
        Line::from(empty_placeholder(app.prompt.value(), "<blank>")),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Details"))
    .wrap(Wrap { trim: false });
    frame.render_widget(active_help, sections[1]);
}

fn render_footer(frame: &mut Frame<'_>, app: &CronInitApp, area: Rect) {
    let footer = Paragraph::new(Text::from(vec![
        Line::from("Tab/Shift+Tab or Up/Down moves between fields. Left/Right changes selections."),
        Line::from(
            "Type to edit text fields. Enter creates the job from any row. In Prompt, Shift+Enter inserts a newline. Ctrl+S also creates the job. Esc cancels.",
        ),
        Line::from(
            app.error
                .as_deref()
                .unwrap_or("Ready to create the cron job."),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Controls"))
    .wrap(Wrap { trim: false });
    frame.render_widget(footer, area);
}

impl CronInitApp {
    fn new(context: CronInitFormContext, prefill: CronInitFormPrefill) -> Self {
        let agent_options = normalized_agent_options(context.agent_options);
        let schedule_prefill = parse_schedule_prefill(prefill.schedule.as_deref());
        let preferred_agent = normalized_optional(prefill.agent.as_deref().unwrap_or_default());
        let default_agent_index = preferred_agent
            .as_deref()
            .and_then(|agent| {
                agent_options
                    .iter()
                    .position(|candidate| candidate.eq_ignore_ascii_case(agent))
            })
            .unwrap_or_else(|| {
                agent_options
                    .iter()
                    .position(|candidate| candidate != NONE_AGENT_LABEL)
                    .unwrap_or(0)
            });

        Self {
            focus: CronField::Name,
            name: InputFieldState::new(prefill.name.unwrap_or_default()),
            schedule_preset: SelectFieldState::new(
                SchedulePreset::labels(),
                schedule_prefill.preset.index(),
            ),
            minutes_interval: InputFieldState::new(schedule_prefill.minutes_interval),
            hour_interval: InputFieldState::new(schedule_prefill.hour_interval),
            hourly_minute: InputFieldState::new(schedule_prefill.hourly_minute),
            daily_hour: InputFieldState::new(schedule_prefill.daily_hour),
            daily_minute: InputFieldState::new(schedule_prefill.daily_minute),
            custom_schedule: InputFieldState::new(schedule_prefill.custom_schedule),
            command: InputFieldState::new(prefill.command.unwrap_or_default()),
            agent: SelectFieldState::new(agent_options, default_agent_index),
            prompt: InputFieldState::multiline(prefill.prompt.unwrap_or_default()),
            working_directory: InputFieldState::new(
                prefill.working_directory.unwrap_or_else(|| ".".to_string()),
            ),
            shell: InputFieldState::new(prefill.shell.unwrap_or_else(|| "/bin/sh".to_string())),
            timeout_seconds: InputFieldState::new(
                prefill.timeout_seconds.unwrap_or(900).to_string(),
            ),
            enabled: SelectFieldState::new(
                ENABLED_OPTIONS
                    .iter()
                    .map(|value| value.to_string())
                    .collect(),
                usize::from(prefill.disabled),
            ),
            error: None,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<CronInitFormExit> {
        self.error = None;

        match key.code {
            KeyCode::Esc => return Some(CronInitFormExit::Cancelled),
            KeyCode::Tab => {
                self.next_field();
                return None;
            }
            KeyCode::Enter
                if self.focus == CronField::Prompt
                    && key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                let _ = self.prompt.insert_newline();
                return None;
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::BackTab => {
                self.previous_field();
                return None;
            }
            KeyCode::Up => {
                self.previous_field();
                return None;
            }
            KeyCode::Down => {
                self.next_field();
                return None;
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.submit();
            }
            _ => {}
        }

        self.handle_field_key(key);
        None
    }

    fn handle_paste(&mut self, text: &str) {
        self.error = None;
        match self.focus {
            CronField::Name => {
                let _ = self.name.paste(text);
            }
            CronField::SchedulePreset => {}
            CronField::MinutesInterval => {
                let _ = self.minutes_interval.paste(text);
            }
            CronField::HourInterval => {
                let _ = self.hour_interval.paste(text);
            }
            CronField::HourlyMinute => {
                let _ = self.hourly_minute.paste(text);
            }
            CronField::DailyHour => {
                let _ = self.daily_hour.paste(text);
            }
            CronField::DailyMinute => {
                let _ = self.daily_minute.paste(text);
            }
            CronField::CustomSchedule => {
                let _ = self.custom_schedule.paste(text);
            }
            CronField::Command => {
                let _ = self.command.paste(text);
            }
            CronField::Agent => {}
            CronField::Prompt => {
                let _ = self.prompt.paste(text);
            }
            CronField::WorkingDirectory => {
                let _ = self.working_directory.paste(text);
            }
            CronField::Shell => {
                let _ = self.shell.paste(text);
            }
            CronField::TimeoutSeconds => {
                let _ = self.timeout_seconds.paste(text);
            }
            CronField::Enabled | CronField::Save => {}
        }
    }

    fn apply_action(&mut self, action: CronInitAction) {
        let key = match action {
            CronInitAction::Up => KeyEvent::from(KeyCode::Up),
            CronInitAction::Down => KeyEvent::from(KeyCode::Down),
            CronInitAction::Left => KeyEvent::from(KeyCode::Left),
            CronInitAction::Right => KeyEvent::from(KeyCode::Right),
            CronInitAction::Tab => KeyEvent::from(KeyCode::Tab),
            CronInitAction::BackTab => KeyEvent::from(KeyCode::BackTab),
            CronInitAction::Save => KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
            CronInitAction::Esc => KeyEvent::from(KeyCode::Esc),
        };
        let _ = self.handle_key(key);
    }

    fn field_value(&self, field: CronField) -> String {
        match field {
            CronField::Name => summarize(self.name.value(), "<required>"),
            CronField::SchedulePreset => self
                .schedule_preset
                .selected_label()
                .unwrap_or("Every 15 minutes")
                .to_string(),
            CronField::MinutesInterval => summarize(self.minutes_interval.value(), "15"),
            CronField::HourInterval => summarize(self.hour_interval.value(), "6"),
            CronField::HourlyMinute => summarize(self.hourly_minute.value(), "0"),
            CronField::DailyHour => summarize(self.daily_hour.value(), "9"),
            CronField::DailyMinute => summarize(self.daily_minute.value(), "0"),
            CronField::CustomSchedule => summarize(self.custom_schedule.value(), "<custom cron>"),
            CronField::Command => summarize(self.command.value(), "<optional>"),
            CronField::Agent => self
                .agent
                .selected_label()
                .unwrap_or(NONE_AGENT_LABEL)
                .to_string(),
            CronField::Prompt => summarize(self.prompt.value(), "<blank>"),
            CronField::WorkingDirectory => summarize(self.working_directory.value(), "."),
            CronField::Shell => summarize(self.shell.value(), "/bin/sh"),
            CronField::TimeoutSeconds => summarize(self.timeout_seconds.value(), "900"),
            CronField::Enabled => self
                .enabled
                .selected_label()
                .unwrap_or("Enabled")
                .to_string(),
            CronField::Save => "Create cron job".to_string(),
        }
    }

    fn handle_field_key(&mut self, key: KeyEvent) {
        match self.focus {
            CronField::Name => {
                self.name.handle_key(key);
            }
            CronField::SchedulePreset => {
                Self::apply_select_key(&mut self.schedule_preset, key);
            }
            CronField::MinutesInterval => {
                self.minutes_interval.handle_key(key);
            }
            CronField::HourInterval => {
                self.hour_interval.handle_key(key);
            }
            CronField::HourlyMinute => {
                self.hourly_minute.handle_key(key);
            }
            CronField::DailyHour => {
                self.daily_hour.handle_key(key);
            }
            CronField::DailyMinute => {
                self.daily_minute.handle_key(key);
            }
            CronField::CustomSchedule => {
                self.custom_schedule.handle_key(key);
            }
            CronField::Command => {
                self.command.handle_key(key);
            }
            CronField::Agent => {
                Self::apply_select_key(&mut self.agent, key);
            }
            CronField::Prompt => {
                self.prompt.handle_key(key);
            }
            CronField::WorkingDirectory => {
                self.working_directory.handle_key(key);
            }
            CronField::Shell => {
                self.shell.handle_key(key);
            }
            CronField::TimeoutSeconds => {
                self.timeout_seconds.handle_key(key);
            }
            CronField::Enabled => {
                Self::apply_select_key(&mut self.enabled, key);
            }
            CronField::Save => {}
        }
    }

    fn apply_select_key(field: &mut SelectFieldState, key: KeyEvent) {
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => field.move_by(-1),
            KeyCode::Right | KeyCode::Char('l') => field.move_by(1),
            _ => {
                let _ = field.handle_key(key);
            }
        }
    }

    fn next_field(&mut self) {
        self.focus = self.focus.next();
    }

    fn previous_field(&mut self) {
        self.focus = self.focus.previous();
    }

    fn selected_agent(&self) -> Option<String> {
        self.agent
            .selected_label()
            .filter(|agent| !agent.eq_ignore_ascii_case(NONE_AGENT_LABEL))
            .map(str::to_string)
    }

    fn selected_schedule_preset(&self) -> SchedulePreset {
        SchedulePreset::from_index(self.schedule_preset.selected())
    }

    fn resolved_schedule(&self) -> Result<String> {
        let schedule = match self.selected_schedule_preset() {
            SchedulePreset::EveryMinutes => {
                let interval =
                    parse_u8_in_range(self.minutes_interval.value(), 1, 59, "minutes interval")?;
                if interval == 1 {
                    "* * * * *".to_string()
                } else {
                    format!("*/{interval} * * * *")
                }
            }
            SchedulePreset::EveryHours => {
                let interval =
                    parse_u8_in_range(self.hour_interval.value(), 1, 23, "hour interval")?;
                let minute = parse_u8_in_range(self.hourly_minute.value(), 0, 59, "hourly minute")?;
                format!("{minute} */{interval} * * *")
            }
            SchedulePreset::DailyAtTime => {
                let hour = parse_u8_in_range(self.daily_hour.value(), 0, 23, "daily hour")?;
                let minute = parse_u8_in_range(self.daily_minute.value(), 0, 59, "daily minute")?;
                format!("{minute} {hour} * * *")
            }
            SchedulePreset::Custom => self.custom_schedule.value().trim().to_string(),
        };

        validate_schedule(&schedule)?;
        Ok(schedule)
    }

    fn submit(&mut self) -> Option<CronInitFormExit> {
        match self.build_values() {
            Ok(values) => Some(CronInitFormExit::Submitted(values)),
            Err(error) => {
                self.error = Some(error.to_string());
                None
            }
        }
    }

    fn build_values(&self) -> Result<CronInitFormValues> {
        let name = self.name.value().trim();
        if name.is_empty() {
            bail!("job name is required");
        }
        validate_name(name)?;

        let schedule = self.resolved_schedule()?;
        let command = self.command.value().trim();

        let working_directory = self.working_directory.value().trim();
        if working_directory.is_empty() {
            bail!("working directory is required");
        }

        let shell = self.shell.value().trim();
        if shell.is_empty() {
            bail!("shell is required");
        }

        let timeout_seconds = self
            .timeout_seconds
            .value()
            .trim()
            .parse::<u64>()
            .map_err(|_| anyhow!("timeout must be a positive integer number of seconds"))?;
        if timeout_seconds == 0 {
            bail!("timeout must be at least 1 second");
        }

        let prompt = normalized_optional(self.prompt.value());
        if command.is_empty() && prompt.is_none() {
            bail!("provide a shell command or an agent prompt");
        }
        if prompt.is_some() && self.selected_agent().is_none() {
            bail!("select an agent or clear the prompt");
        }

        Ok(CronInitFormValues {
            name: name.to_string(),
            schedule,
            command: command.to_string(),
            agent: if prompt.is_some() {
                self.selected_agent()
            } else {
                None
            },
            prompt,
            shell: shell.to_string(),
            working_directory: working_directory.to_string(),
            timeout_seconds,
            enabled: self.enabled.selected() == 0,
        })
    }
}

impl SchedulePreset {
    fn labels() -> Vec<String> {
        vec![
            "Every N minutes".to_string(),
            "Every N hours".to_string(),
            "Daily at HH:MM".to_string(),
            "Custom cron".to_string(),
        ]
    }

    fn index(self) -> usize {
        match self {
            Self::EveryMinutes => 0,
            Self::EveryHours => 1,
            Self::DailyAtTime => 2,
            Self::Custom => 3,
        }
    }

    fn from_index(index: usize) -> Self {
        match index {
            1 => Self::EveryHours,
            2 => Self::DailyAtTime,
            3 => Self::Custom,
            _ => Self::EveryMinutes,
        }
    }
}

impl CronField {
    fn all() -> &'static [Self] {
        &[
            Self::Name,
            Self::SchedulePreset,
            Self::MinutesInterval,
            Self::HourInterval,
            Self::HourlyMinute,
            Self::DailyHour,
            Self::DailyMinute,
            Self::CustomSchedule,
            Self::Command,
            Self::Agent,
            Self::Prompt,
            Self::WorkingDirectory,
            Self::Shell,
            Self::TimeoutSeconds,
            Self::Enabled,
            Self::Save,
        ]
    }

    fn index(self) -> usize {
        Self::all()
            .iter()
            .position(|field| *field == self)
            .unwrap_or(0)
    }

    fn next(self) -> Self {
        let index = (self.index() + 1) % Self::all().len();
        Self::all()[index]
    }

    fn previous(self) -> Self {
        let index = if self.index() == 0 {
            Self::all().len() - 1
        } else {
            self.index() - 1
        };
        Self::all()[index]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::SchedulePreset => "Schedule preset",
            Self::MinutesInterval => "Minutes interval",
            Self::HourInterval => "Hour interval",
            Self::HourlyMinute => "Hourly minute",
            Self::DailyHour => "Daily hour",
            Self::DailyMinute => "Daily minute",
            Self::CustomSchedule => "Custom cron",
            Self::Command => "Shell command",
            Self::Agent => "Agent",
            Self::Prompt => "Agent prompt",
            Self::WorkingDirectory => "Working directory",
            Self::Shell => "Shell",
            Self::TimeoutSeconds => "Timeout seconds",
            Self::Enabled => "Enabled",
            Self::Save => "Save",
        }
    }

    fn help_text(self) -> &'static str {
        match self {
            Self::Name => "Repository-local cron jobs are written to .metastack/cron/<NAME>.md.",
            Self::SchedulePreset => {
                "Choose between a minute-based preset, an hour-based preset, a daily time, or a raw cron expression."
            }
            Self::MinutesInterval => {
                "Used when the schedule preset is Every N minutes. Valid values: 1-59."
            }
            Self::HourInterval => {
                "Used when the schedule preset is Every N hours. Valid values: 1-23."
            }
            Self::HourlyMinute => {
                "Used with the hourly preset to pin the minute within each selected hour."
            }
            Self::DailyHour => "Used when the preset is Daily at HH:MM. Valid values: 0-23.",
            Self::DailyMinute => "Used when the preset is Daily at HH:MM. Valid values: 0-59.",
            Self::CustomSchedule => {
                "Stores a raw 5-field cron expression when the preset is Custom cron."
            }
            Self::Command => {
                "Optional shell command. Leave blank to run only the agent prompt on schedule."
            }
            Self::Agent => {
                "When combined with a prompt, the selected agent runs after the optional shell phase."
            }
            Self::Prompt => {
                "This recurring prompt is stored as the Markdown body and sent to the agent on every cron run."
            }
            Self::WorkingDirectory => {
                "Both the optional shell command and the optional agent run from this repository-relative path."
            }
            Self::Shell => "Shell binary used to execute the cron command.",
            Self::TimeoutSeconds => {
                "Maximum number of seconds allowed for the optional shell command phase."
            }
            Self::Enabled => "Disabled jobs stay on disk but are skipped by the scheduler.",
            Self::Save => {
                "Create or update the cron job Markdown file using the current form values."
            }
        }
    }
}

#[derive(Debug, Clone)]
struct SchedulePrefill {
    preset: SchedulePreset,
    minutes_interval: String,
    hour_interval: String,
    hourly_minute: String,
    daily_hour: String,
    daily_minute: String,
    custom_schedule: String,
}

fn parse_schedule_prefill(schedule: Option<&str>) -> SchedulePrefill {
    let default = SchedulePrefill {
        preset: SchedulePreset::EveryMinutes,
        minutes_interval: "15".to_string(),
        hour_interval: "6".to_string(),
        hourly_minute: "0".to_string(),
        daily_hour: "9".to_string(),
        daily_minute: "0".to_string(),
        custom_schedule: String::new(),
    };

    let Some(schedule) = schedule.map(str::trim).filter(|value| !value.is_empty()) else {
        return default;
    };

    let fields = schedule.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return SchedulePrefill {
            preset: SchedulePreset::Custom,
            custom_schedule: schedule.to_string(),
            ..default
        };
    }

    if fields[1] == "*" && fields[2] == "*" && fields[3] == "*" && fields[4] == "*" {
        if fields[0] == "*" {
            return SchedulePrefill {
                preset: SchedulePreset::EveryMinutes,
                minutes_interval: "1".to_string(),
                ..default
            };
        }
        if let Some(value) = fields[0].strip_prefix("*/") {
            return SchedulePrefill {
                preset: SchedulePreset::EveryMinutes,
                minutes_interval: value.to_string(),
                ..default
            };
        }
    }

    if fields[2] == "*" && fields[3] == "*" && fields[4] == "*" {
        if let Some(value) = fields[1].strip_prefix("*/") {
            return SchedulePrefill {
                preset: SchedulePreset::EveryHours,
                hour_interval: value.to_string(),
                hourly_minute: fields[0].to_string(),
                ..default
            };
        }

        if fields[0].parse::<u8>().is_ok() && fields[1].parse::<u8>().is_ok() {
            return SchedulePrefill {
                preset: SchedulePreset::DailyAtTime,
                daily_hour: fields[1].to_string(),
                daily_minute: fields[0].to_string(),
                ..default
            };
        }
    }

    SchedulePrefill {
        preset: SchedulePreset::Custom,
        custom_schedule: schedule.to_string(),
        ..default
    }
}

fn normalized_agent_options(agent_options: Vec<String>) -> Vec<String> {
    let mut options = vec![NONE_AGENT_LABEL.to_string()];

    for option in agent_options {
        let trimmed = option.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(NONE_AGENT_LABEL) {
            continue;
        }
        if !options
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(trimmed))
        {
            options.push(trimmed.to_string());
        }
    }

    options
}

fn summarize(value: &str, placeholder: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return placeholder.to_string();
    }

    let summary = trimmed.replace('\n', " ");
    if summary.chars().count() <= 40 {
        summary
    } else {
        let shortened = summary.chars().take(37).collect::<String>();
        format!("{shortened}...")
    }
}

fn empty_placeholder<'a>(value: &'a str, placeholder: &'a str) -> &'a str {
    if value.trim().is_empty() {
        placeholder
    } else {
        value.trim()
    }
}

fn normalized_optional(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_u8_in_range(value: &str, min: u8, max: u8, label: &str) -> Result<u8> {
    let parsed = value
        .trim()
        .parse::<u8>()
        .map_err(|_| anyhow!("{label} must be between {min} and {max}"))?;
    if parsed < min || parsed > max {
        bail!("{label} must be between {min} and {max}");
    }
    Ok(parsed)
}

fn validate_name(name: &str) -> Result<()> {
    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        bail!("job names may only contain ASCII letters, digits, `-`, and `_`");
    }
    Ok(())
}

fn validate_schedule(schedule: &str) -> Result<()> {
    let normalized = normalize_schedule(schedule)?;
    Schedule::from_str(&normalized)
        .map(|_| ())
        .map_err(|error| {
            anyhow!(
                "failed to parse cron schedule `{}`: {error}",
                schedule.trim()
            )
        })
}

fn normalize_schedule(schedule: &str) -> Result<String> {
    let trimmed = schedule.trim();
    let fields = trimmed.split_whitespace().count();
    match fields {
        5 => Ok(format!("0 {trimmed}")),
        6 | 7 => Ok(trimmed.to_string()),
        _ => bail!(
            "cron schedules must use 5 fields (minute hour day month weekday) or a full 6/7-field expression"
        ),
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
        CronField, CronInitAction, CronInitApp, CronInitFormContext, CronInitFormExit,
        CronInitFormPrefill, SchedulePreset, parse_schedule_prefill,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn schedule_prefill_detects_hourly_expression() {
        let prefill = parse_schedule_prefill(Some("15 */4 * * *"));
        assert_eq!(prefill.preset, SchedulePreset::EveryHours);
        assert_eq!(prefill.hour_interval, "4");
        assert_eq!(prefill.hourly_minute, "15");
    }

    #[test]
    fn schedule_prefill_falls_back_to_custom_for_unknown_shape() {
        let prefill = parse_schedule_prefill(Some("0 0 * * 1"));
        assert_eq!(prefill.preset, SchedulePreset::Custom);
        assert_eq!(prefill.custom_schedule, "0 0 * * 1");
    }

    #[test]
    fn prompt_without_text_disables_agent_on_submit() {
        let app = CronInitApp::new(
            CronInitFormContext {
                agent_options: vec!["codex".to_string()],
            },
            CronInitFormPrefill {
                name: Some("nightly".to_string()),
                command: Some("echo hello".to_string()),
                ..CronInitFormPrefill::default()
            },
        );

        let values = app.build_values().expect("values should build");
        assert_eq!(values.agent, None);
        assert_eq!(values.prompt, None);
    }

    #[test]
    fn render_actions_move_focus_between_fields() {
        let mut app = CronInitApp::new(
            CronInitFormContext {
                agent_options: vec!["codex".to_string()],
            },
            CronInitFormPrefill::default(),
        );

        app.apply_action(CronInitAction::Tab);
        app.apply_action(CronInitAction::Tab);
        assert_eq!(app.focus.index(), 2);
    }

    #[test]
    fn enter_on_save_submits_the_form() {
        let mut app = CronInitApp::new(
            CronInitFormContext {
                agent_options: vec!["codex".to_string()],
            },
            CronInitFormPrefill {
                name: Some("nightly".to_string()),
                command: Some("echo hello".to_string()),
                ..CronInitFormPrefill::default()
            },
        );
        app.focus = CronField::Save;

        let exit = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match exit {
            Some(CronInitFormExit::Submitted(values)) => {
                assert_eq!(values.name, "nightly");
                assert_eq!(values.command, "echo hello");
            }
            other => panic!("expected submitted exit, got {other:?}"),
        }
    }

    #[test]
    fn shift_enter_from_prompt_adds_a_newline_instead_of_submitting() {
        let mut app = CronInitApp::new(
            CronInitFormContext {
                agent_options: vec!["codex".to_string()],
            },
            CronInitFormPrefill {
                name: Some("nightly".to_string()),
                command: Some("echo hello".to_string()),
                ..CronInitFormPrefill::default()
            },
        );
        app.focus = CronField::Prompt;

        let exit = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));

        assert!(exit.is_none());
        assert_eq!(app.prompt.value(), "\n");
        assert_eq!(app.error, None);
    }

    #[test]
    fn enter_from_prompt_submits_the_form() {
        let mut app = CronInitApp::new(
            CronInitFormContext {
                agent_options: vec!["codex".to_string()],
            },
            CronInitFormPrefill {
                name: Some("nightly".to_string()),
                command: Some("echo hello".to_string()),
                prompt: Some("Summarize failures".to_string()),
                ..CronInitFormPrefill::default()
            },
        );
        app.focus = CronField::Prompt;

        let exit = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        match exit {
            Some(CronInitFormExit::Submitted(values)) => {
                assert_eq!(values.name, "nightly");
                assert_eq!(values.command, "echo hello");
                assert_eq!(values.prompt.as_deref(), Some("Summarize failures"));
            }
            other => panic!("expected submitted exit, got {other:?}"),
        }
    }

    #[test]
    fn prompt_paste_preserves_multiline_text() {
        let mut app = CronInitApp::new(
            CronInitFormContext {
                agent_options: vec!["codex".to_string()],
            },
            CronInitFormPrefill::default(),
        );
        app.focus = CronField::Prompt;

        app.handle_paste("First line\nSecond line\n");

        assert_eq!(app.prompt.value(), "First line\nSecond line\n");
        assert_eq!(app.error, None);
    }
}

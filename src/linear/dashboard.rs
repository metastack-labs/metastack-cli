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

use super::browser::{
    IssueSearchResult, empty_search_result, issue_state_label, render_issue_preview,
    render_issue_row, search_issues,
};
use super::{DashboardData, IssueSummary};
use crate::tui::fields::InputFieldState;
use crate::tui::scroll::{ScrollState, plain_text, scrollable_paragraph, wrapped_rows};
use crate::tui::theme::{Tone, badge, empty_state, key_hints, list, panel_title, paragraph};

#[derive(Debug, Clone)]
pub struct DashboardOptions {
    pub render_once: bool,
    pub width: u16,
    pub height: u16,
    pub actions: Vec<DashboardAction>,
    pub initial_state_filter: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum DashboardAction {
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    Tab,
    Enter,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Status,
    Estimate,
    Issues,
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EstimateFilter {
    All,
    Unestimated,
    Exact(String),
}

#[derive(Debug, Clone)]
struct FilterOption<T> {
    label: String,
    value: T,
    count: usize,
}

#[derive(Debug, Clone)]
struct DashboardApp {
    data: DashboardData,
    focus: Focus,
    query: InputFieldState,
    status_options: Vec<FilterOption<Option<String>>>,
    estimate_options: Vec<FilterOption<EstimateFilter>>,
    status_index: usize,
    estimate_index: usize,
    issue_index: usize,
    active_status: Option<String>,
    active_estimate: EstimateFilter,
    preview_scroll: ScrollState,
}

pub fn run_dashboard(data: DashboardData, options: DashboardOptions) -> Result<Option<String>> {
    if options.render_once {
        return render_once(data, options).map(Some);
    }

    if !io::stdout().is_terminal() {
        bail!(
            "the interactive issue dashboard requires a TTY; pass `--json` for machine-readable output"
        );
    }

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = DashboardApp::new(data, options.initial_state_filter);

    loop {
        terminal.draw(|frame| render_dashboard(frame, &app))?;

        if event::poll(Duration::from_millis(250))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Up => app.apply_in_viewport(
                        DashboardAction::Up,
                        preview_viewport(terminal.size()?.into()),
                    ),
                    KeyCode::Down => app.apply_in_viewport(
                        DashboardAction::Down,
                        preview_viewport(terminal.size()?.into()),
                    ),
                    KeyCode::PageUp => app.apply_in_viewport(
                        DashboardAction::PageUp,
                        preview_viewport(terminal.size()?.into()),
                    ),
                    KeyCode::PageDown => app.apply_in_viewport(
                        DashboardAction::PageDown,
                        preview_viewport(terminal.size()?.into()),
                    ),
                    KeyCode::Home => app.apply_in_viewport(
                        DashboardAction::Home,
                        preview_viewport(terminal.size()?.into()),
                    ),
                    KeyCode::End => app.apply_in_viewport(
                        DashboardAction::End,
                        preview_viewport(terminal.size()?.into()),
                    ),
                    KeyCode::Tab => app.apply_in_viewport(
                        DashboardAction::Tab,
                        preview_viewport(terminal.size()?.into()),
                    ),
                    KeyCode::Enter => app.apply_in_viewport(
                        DashboardAction::Enter,
                        preview_viewport(terminal.size()?.into()),
                    ),
                    _ => {
                        let _ = app.handle_query_key(key);
                    }
                },
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

    Ok(None)
}

fn render_once(data: DashboardData, options: DashboardOptions) -> Result<String> {
    let backend = TestBackend::new(options.width, options.height);
    let mut terminal = Terminal::new(backend)?;
    let mut app = DashboardApp::new(data, options.initial_state_filter);
    for action in options.actions {
        app.apply_in_viewport(
            action,
            preview_viewport(Rect::new(0, 0, options.width, options.height)),
        );
    }

    terminal.draw(|frame| render_dashboard(frame, &app))?;
    Ok(snapshot(terminal.backend()))
}

fn render_dashboard(frame: &mut Frame<'_>, app: &DashboardApp) {
    let narrow = frame.area().width < 115;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
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
            vec![
                Constraint::Length(12),
                Constraint::Percentage(44),
                Constraint::Min(10),
            ]
        } else {
            vec![
                Constraint::Percentage(26),
                Constraint::Percentage(34),
                Constraint::Percentage(40),
            ]
        })
        .split(outer[2]);
    let sidebar = Layout::default()
        .direction(if narrow {
            Direction::Horizontal
        } else {
            Direction::Vertical
        })
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(body[0]);

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
                ("Enter", "apply"),
                ("q", "exit"),
            ]),
        ]),
        panel_title("Linear Issues", false),
    );
    frame.render_widget(header, outer[0]);

    let rendered_query = app.query.render(
        "Search by identifier, title, state, project, or description...",
        app.focus == Focus::Issues,
    );
    let query_block = Block::default()
        .borders(Borders::ALL)
        .title(if app.focus == Focus::Issues {
            "Issue Search [active]"
        } else {
            "Issue Search"
        });
    let query_inner = query_block.inner(outer[1]);
    let query = rendered_query.paragraph(query_block);
    frame.render_widget(query, outer[1]);
    rendered_query.set_cursor(frame, query_inner);

    render_filter_list(
        frame,
        sidebar[0],
        "Status",
        app.focus == Focus::Status,
        &app.status_options,
        app.status_index,
        |value| app.status_option_is_active(value),
    );
    render_filter_list(
        frame,
        sidebar[1],
        "Estimate",
        app.focus == Focus::Estimate,
        &app.estimate_options,
        app.estimate_index,
        |value| app.estimate_option_is_active(value),
    );

    let filtered_issue_results = app.visible_issue_results();
    let issue_title = panel_title(
        format!(
            "Issues ({}/{})",
            filtered_issue_results.len(),
            app.data.issues.len()
        ),
        app.focus == Focus::Issues,
    );
    let issue_items = if filtered_issue_results.is_empty() {
        vec![ListItem::new(empty_state(
            "No issues match the current search and filters.",
            "Adjust the search query or sidebar filters to widen the result set.",
        ))]
    } else {
        filtered_issue_results
            .iter()
            .filter_map(|result| {
                app.data
                    .issues
                    .get(result.issue_index)
                    .map(|issue| render_issue_row(issue, Some(result), None))
            })
            .collect::<Vec<_>>()
    };
    let mut issue_state = ListState::default();
    if filtered_issue_results.is_empty() {
        issue_state.select(Some(0));
    } else {
        issue_state.select(Some(app.issue_index.min(filtered_issue_results.len() - 1)));
    }
    let issue_list = list(issue_items, issue_title);
    frame.render_stateful_widget(issue_list, body[1], &mut issue_state);

    let preview = scrollable_paragraph(
        app.preview_text(),
        panel_title("Description Preview", app.focus == Focus::Preview),
        &app.preview_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(preview, body[2]);
}

fn render_filter_list<T, F>(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    is_focused: bool,
    options: &[FilterOption<T>],
    selected_index: usize,
    is_active: F,
) where
    F: Fn(&T) -> bool,
{
    let mut state = ListState::default();
    state.select(Some(selected_index.min(options.len().saturating_sub(1))));
    let items = options
        .iter()
        .map(|option| {
            let badge_label = if is_active(&option.value) {
                "on"
            } else {
                "off"
            };
            let badge_tone = if is_active(&option.value) {
                Tone::Success
            } else {
                Tone::Muted
            };
            ListItem::new(Line::from(vec![
                badge(badge_label, badge_tone),
                Span::raw(format!(" {} ({})", option.label, option.count)),
            ]))
        })
        .collect::<Vec<_>>();
    let list = list(items, panel_title(title, is_focused));
    frame.render_stateful_widget(list, area, &mut state);
}

impl DashboardApp {
    fn new(data: DashboardData, initial_state_filter: Option<String>) -> Self {
        let status_options = build_status_options(&data.issues);
        let estimate_options = build_estimate_options(&data.issues);
        let active_status = initial_state_filter
            .as_deref()
            .and_then(|state| match_status_option(&status_options, state));

        let mut app = Self {
            data,
            focus: Focus::Issues,
            query: InputFieldState::default(),
            status_options,
            estimate_options,
            status_index: 0,
            estimate_index: 0,
            issue_index: 0,
            active_status,
            active_estimate: EstimateFilter::All,
            preview_scroll: ScrollState::default(),
        };

        app.status_index = app.selected_status_index();
        app.estimate_index = app.selected_estimate_index();
        app.clamp_issue_index();
        app
    }

    #[cfg(test)]
    fn apply(&mut self, action: DashboardAction) {
        self.apply_in_viewport(action, preview_viewport(Rect::new(0, 0, 120, 32)));
    }

    fn apply_in_viewport(&mut self, action: DashboardAction, preview_viewport: Rect) {
        match action {
            DashboardAction::Up => {
                if self.focus == Focus::Preview {
                    self.scroll_preview_line(-1, preview_viewport);
                } else {
                    self.move_selection(-1);
                }
            }
            DashboardAction::Down => {
                if self.focus == Focus::Preview {
                    self.scroll_preview_line(1, preview_viewport);
                } else {
                    self.move_selection(1);
                }
            }
            DashboardAction::PageUp => self.scroll_preview_page(-1, preview_viewport),
            DashboardAction::PageDown => self.scroll_preview_page(1, preview_viewport),
            DashboardAction::Home => {
                if self.focus == Focus::Preview {
                    self.preview_scroll.reset();
                }
            }
            DashboardAction::End => {
                if self.focus == Focus::Preview {
                    let _ = self.preview_scroll.apply_key_code_in_viewport(
                        KeyCode::End,
                        preview_viewport,
                        self.preview_content_rows(preview_viewport.width),
                    );
                }
            }
            DashboardAction::Tab => {
                self.focus = match self.focus {
                    Focus::Status => Focus::Estimate,
                    Focus::Estimate => Focus::Issues,
                    Focus::Issues => Focus::Preview,
                    Focus::Preview => Focus::Status,
                };
            }
            DashboardAction::Enter => self.apply_focus_selection(),
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.focus {
            Focus::Status => shift_index(&mut self.status_index, self.status_options.len(), delta),
            Focus::Estimate => {
                shift_index(&mut self.estimate_index, self.estimate_options.len(), delta)
            }
            Focus::Issues => {
                let len = self.visible_issue_results().len();
                shift_index(&mut self.issue_index, len, delta);
                self.preview_scroll.reset();
            }
            Focus::Preview => {}
        }
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

    fn apply_focus_selection(&mut self) {
        match self.focus {
            Focus::Status => {
                if let Some(option) = self.status_options.get(self.status_index) {
                    self.active_status = option.value.clone();
                    self.clamp_issue_index();
                    self.preview_scroll.reset();
                }
            }
            Focus::Estimate => {
                if let Some(option) = self.estimate_options.get(self.estimate_index) {
                    self.active_estimate = option.value.clone();
                    self.clamp_issue_index();
                    self.preview_scroll.reset();
                }
            }
            Focus::Issues | Focus::Preview => {}
        }
    }

    fn filtered_issue_indices(&self) -> Vec<usize> {
        self.data
            .issues
            .iter()
            .enumerate()
            .filter(|(_, issue)| self.matches_status(issue) && self.matches_estimate(issue))
            .map(|(index, _)| index)
            .collect()
    }

    fn visible_issue_results(&self) -> Vec<IssueSearchResult> {
        let filtered = self.filtered_issue_indices();
        if self.query.value().trim().is_empty() {
            return filtered.into_iter().map(empty_search_result).collect();
        }

        search_issues(&self.data.issues, self.query.value().trim())
            .into_iter()
            .filter(|result| filtered.contains(&result.issue_index))
            .collect()
    }

    fn matches_status(&self, issue: &IssueSummary) -> bool {
        self.active_status
            .as_ref()
            .is_none_or(|status| issue_state_label(issue).eq_ignore_ascii_case(status))
    }

    fn matches_estimate(&self, issue: &IssueSummary) -> bool {
        match &self.active_estimate {
            EstimateFilter::All => true,
            EstimateFilter::Unestimated => issue.estimate.is_none(),
            EstimateFilter::Exact(expected) => issue_estimate_key(issue)
                .as_deref()
                .map(|value| value == expected)
                .unwrap_or(false),
        }
    }

    fn preview_text(&self) -> Text<'static> {
        let results = self.visible_issue_results();
        let Some(selected_result) = results.get(self.issue_index) else {
            return Text::from(vec![
                Line::from("No issues match the current search and filters."),
                Line::from("Adjust the search query or sidebar filters to widen the result set."),
            ]);
        };
        let issue = &self.data.issues[selected_result.issue_index];
        render_issue_preview(
            issue,
            Some(selected_result),
            None,
            "No description provided.",
        )
    }

    fn summary_line(&self) -> String {
        format!(
            "Visible issues: {}/{} | Search: {} | Status: {} | Estimate: {}",
            self.visible_issue_results().len(),
            self.data.issues.len(),
            if self.query.value().trim().is_empty() {
                "all".to_string()
            } else {
                format!("\"{}\"", self.query.value().trim())
            },
            self.active_status.as_deref().unwrap_or("All statuses"),
            match &self.active_estimate {
                EstimateFilter::All => "All estimates".to_string(),
                EstimateFilter::Unestimated => "Unestimated".to_string(),
                EstimateFilter::Exact(value) => format!("{value} pts"),
            }
        )
    }

    fn selected_status_index(&self) -> usize {
        self.status_options
            .iter()
            .position(|option| option.value == self.active_status)
            .unwrap_or(0)
    }

    fn selected_estimate_index(&self) -> usize {
        self.estimate_options
            .iter()
            .position(|option| option.value == self.active_estimate)
            .unwrap_or(0)
    }

    fn preview_content_rows(&self, width: u16) -> usize {
        wrapped_rows(&plain_text(&self.preview_text()), width.max(1))
    }

    fn scroll_preview_line(&mut self, delta: isize, viewport: Rect) {
        let key = if delta.is_negative() {
            KeyCode::Up
        } else {
            KeyCode::Down
        };
        let _ = self.preview_scroll.apply_key_code_in_viewport(
            key,
            viewport,
            self.preview_content_rows(viewport.width.max(1)),
        );
    }

    fn scroll_preview_page(&mut self, delta: isize, viewport: Rect) {
        if self.focus != Focus::Preview {
            return;
        }
        let key = if delta.is_negative() {
            crossterm::event::KeyEvent::from(KeyCode::PageUp)
        } else {
            crossterm::event::KeyEvent::from(KeyCode::PageDown)
        };
        let _ = self.preview_scroll.apply_key_in_viewport(
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

    fn status_option_is_active(&self, value: &Option<String>) -> bool {
        self.active_status == *value
    }

    fn estimate_option_is_active(&self, value: &EstimateFilter) -> bool {
        self.active_estimate == *value
    }

    fn clamp_issue_index(&mut self) {
        let len = self.visible_issue_results().len();
        if len == 0 {
            self.issue_index = 0;
        } else {
            self.issue_index = self.issue_index.min(len - 1);
        }
    }
}

fn build_status_options(issues: &[IssueSummary]) -> Vec<FilterOption<Option<String>>> {
    let mut labels = issues.iter().map(issue_state_label).collect::<Vec<_>>();
    labels.sort();
    labels.dedup();

    let mut options = vec![FilterOption {
        label: "All statuses".to_string(),
        value: None,
        count: issues.len(),
    }];
    options.extend(labels.into_iter().map(|label| {
        FilterOption {
            count: issues
                .iter()
                .filter(|issue| issue_state_label(issue) == label)
                .count(),
            value: Some(label.clone()),
            label,
        }
    }));
    options
}

fn build_estimate_options(issues: &[IssueSummary]) -> Vec<FilterOption<EstimateFilter>> {
    let mut values = issues
        .iter()
        .filter_map(issue_estimate_key)
        .collect::<Vec<_>>();
    values.sort_by(|left, right| compare_estimate_keys(left, right));
    values.dedup();

    let mut options = vec![FilterOption {
        label: "All estimates".to_string(),
        value: EstimateFilter::All,
        count: issues.len(),
    }];

    let unestimated_count = issues
        .iter()
        .filter(|issue| issue.estimate.is_none())
        .count();
    if unestimated_count > 0 {
        options.push(FilterOption {
            label: "Unestimated".to_string(),
            value: EstimateFilter::Unestimated,
            count: unestimated_count,
        });
    }

    options.extend(values.into_iter().map(|value| {
        let count = issues
            .iter()
            .filter(|issue| issue_estimate_key(issue).as_deref() == Some(value.as_str()))
            .count();
        FilterOption {
            label: format!("{value} pts"),
            value: EstimateFilter::Exact(value),
            count,
        }
    }));
    options
}

fn match_status_option(options: &[FilterOption<Option<String>>], state: &str) -> Option<String> {
    options.iter().find_map(|option| {
        option
            .value
            .as_ref()
            .filter(|value| value.eq_ignore_ascii_case(state))
            .cloned()
    })
}

fn issue_estimate_key(issue: &IssueSummary) -> Option<String> {
    issue.estimate.map(format_estimate)
}

fn format_estimate(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        let rendered = format!("{value:.2}");
        rendered
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

fn compare_estimate_keys(left: &str, right: &str) -> std::cmp::Ordering {
    match (left.parse::<f64>(), right.parse::<f64>()) {
        (Ok(left), Ok(right)) => left
            .partial_cmp(&right)
            .unwrap_or(std::cmp::Ordering::Equal),
        _ => left.cmp(right),
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
    let narrow = area.width < 115;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
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
        .constraints(if narrow {
            vec![
                Constraint::Length(12),
                Constraint::Percentage(44),
                Constraint::Min(10),
            ]
        } else {
            vec![
                Constraint::Percentage(26),
                Constraint::Percentage(34),
                Constraint::Percentage(40),
            ]
        })
        .split(outer[2]);

    Rect::new(
        body[2].x.saturating_add(1),
        body[2].y.saturating_add(1),
        body[2].width.saturating_sub(2).max(1),
        body[2].height.saturating_sub(2).max(1),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        DashboardAction, DashboardApp, DashboardOptions, EstimateFilter, Focus, preview_viewport,
        run_dashboard,
    };
    use crate::linear::DashboardData;
    use crate::tui::fields::InputFieldState;
    use ratatui::layout::Rect;

    #[test]
    fn dashboard_state_applies_status_and_estimate_filters() {
        let mut app = DashboardApp::new(DashboardData::demo(), None);

        assert_eq!(visible_issue_ids(&app), vec!["MET-11", "MET-12"]);

        app.apply(DashboardAction::Tab);
        assert_eq!(app.focus, Focus::Preview);
        app.apply(DashboardAction::Tab);
        assert_eq!(app.focus, Focus::Status);
        app.apply(DashboardAction::Down);
        app.apply(DashboardAction::Down);
        app.apply(DashboardAction::Enter);
        assert_eq!(app.active_status.as_deref(), Some("Todo"));
        assert_eq!(visible_issue_ids(&app), vec!["MET-12"]);

        app.apply(DashboardAction::Tab);
        assert_eq!(app.focus, Focus::Estimate);
        app.apply(DashboardAction::Down);
        app.apply(DashboardAction::Down);
        app.apply(DashboardAction::Enter);
        assert_eq!(app.active_estimate, EstimateFilter::Exact("5".to_string()));
        assert_eq!(visible_issue_ids(&app), vec!["MET-12"]);
    }

    #[test]
    fn dashboard_honors_initial_state_filter() {
        let app = DashboardApp::new(DashboardData::demo(), Some("In Progress".to_string()));

        assert_eq!(app.active_status.as_deref(), Some("In Progress"));
        assert_eq!(visible_issue_ids(&app), vec!["MET-11"]);
    }

    #[test]
    fn dashboard_search_filters_visible_issue_results() {
        let mut app = DashboardApp::new(DashboardData::demo(), None);
        app.query = InputFieldState::new("tests");

        assert_eq!(visible_issue_ids(&app), vec!["MET-12"]);
    }

    #[test]
    fn dashboard_search_zero_results_updates_preview_copy() {
        let mut app = DashboardApp::new(DashboardData::demo(), None);
        app.query = InputFieldState::new("zzz");

        assert!(visible_issue_ids(&app).is_empty());
        assert!(
            format!("{:?}", app.preview_text())
                .contains("No issues match the current search and filters.")
        );
    }

    #[test]
    fn dashboard_render_once_surfaces_empty_state_in_narrow_layout() {
        let snapshot = run_dashboard(
            DashboardData {
                title: "Linear dashboard".to_string(),
                issues: Vec::new(),
            },
            DashboardOptions {
                render_once: true,
                width: 96,
                height: 30,
                actions: Vec::new(),
                initial_state_filter: None,
            },
        )
        .expect("render once should succeed")
        .expect("snapshot should be returned");

        assert!(snapshot.contains("No issues match the current search and filters."));
        assert!(
            snapshot
                .contains("Adjust the search query or sidebar filters to widen the result set.")
        );
    }

    #[test]
    fn dashboard_preview_scrolls_to_overflowed_description() {
        let mut data = DashboardData::demo();
        data.issues[0].description = Some(
            (1..=24)
                .map(|index| format!("preview line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let viewport = preview_viewport(Rect::new(0, 0, 120, 20));
        let mut app = DashboardApp::new(data, None);
        app.apply_in_viewport(DashboardAction::Tab, viewport);
        app.apply_in_viewport(DashboardAction::End, viewport);

        assert_eq!(app.focus, Focus::Preview);
        assert!(app.preview_scroll.offset() > 0);
    }

    fn visible_issue_ids(app: &DashboardApp) -> Vec<&str> {
        app.visible_issue_results()
            .into_iter()
            .filter_map(|result| app.data.issues.get(result.issue_index))
            .map(|issue| issue.identifier.as_str())
            .collect()
    }
}

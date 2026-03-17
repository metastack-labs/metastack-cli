use std::collections::BTreeSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use unicode_width::UnicodeWidthChar;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputFieldState {
    value: String,
    cursor: usize,
    mode: InputFieldMode,
}

#[derive(Debug, Clone)]
pub(crate) struct InputFieldRender {
    pub(crate) text: Text<'static>,
    cursor_prefix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputFieldMode {
    SingleLine,
    MultiLine,
}

impl Default for InputFieldState {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl InputFieldState {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            cursor: value.len(),
            value,
            mode: InputFieldMode::SingleLine,
        }
    }

    pub(crate) fn multiline(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            cursor: value.len(),
            value,
            mode: InputFieldMode::MultiLine,
        }
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn render(&self, placeholder: &str, active: bool) -> InputFieldRender {
        if self.value.is_empty() {
            let text = Text::from(Line::styled(
                placeholder.to_string(),
                Style::default().add_modifier(Modifier::DIM),
            ));
            return InputFieldRender {
                text,
                cursor_prefix: active.then(String::new),
            };
        }

        if !active {
            return InputFieldRender {
                text: Text::from(self.value.clone()),
                cursor_prefix: None,
            };
        }

        InputFieldRender {
            text: Text::from(self.value.clone()),
            cursor_prefix: Some(self.value[..self.cursor].to_string()),
        }
    }

    pub(crate) fn insert_newline(&mut self) -> bool {
        if self.mode != InputFieldMode::MultiLine {
            return false;
        }

        self.insert('\n');
        true
    }

    pub(crate) fn paste(&mut self, text: &str) -> bool {
        let normalized = match self.mode {
            InputFieldMode::SingleLine => normalize_single_line_paste(text),
            InputFieldMode::MultiLine => normalize_multi_line_paste(text),
        };

        if normalized.is_empty() {
            return false;
        }

        for ch in normalized.chars() {
            self.insert(ch);
        }

        true
    }
}

impl InputFieldRender {
    pub(crate) fn set_cursor(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let Some(prefix) = &self.cursor_prefix else {
            return;
        };

        let (column, row) = wrapped_cursor_position(prefix, area.width);
        if row < area.height {
            frame.set_cursor_position((area.x + column, area.y + row));
        }
    }

    #[cfg(test)]
    pub(crate) fn cursor_position(&self, width: u16) -> Option<(u16, u16)> {
        self.cursor_prefix
            .as_ref()
            .map(|prefix| wrapped_cursor_position(prefix, width.max(1)))
    }
}

impl InputFieldState {
    fn insert(&mut self, ch: char) {
        self.value.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let start = previous_boundary(&self.value, self.cursor);
        self.value.drain(start..self.cursor);
        self.cursor = start;
    }

    fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    fn move_left(&mut self) {
        self.cursor = previous_boundary(&self.value, self.cursor);
    }

    fn move_right(&mut self) {
        self.cursor = next_boundary(&self.value, self.cursor);
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.value.len();
    }
}

fn normalize_single_line_paste(text: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;

    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = !normalized.is_empty() || !text.is_empty();
            continue;
        }

        if ch.is_control() {
            continue;
        }

        if pending_space {
            normalized.push(' ');
            pending_space = false;
        }

        normalized.push(ch);
    }

    normalized
}

fn normalize_multi_line_paste(text: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_carriage_return = false;

    for ch in text.chars() {
        match ch {
            '\r' => {
                normalized.push('\n');
                previous_was_carriage_return = true;
            }
            '\n' => {
                if !previous_was_carriage_return {
                    normalized.push('\n');
                }
                previous_was_carriage_return = false;
            }
            '\t' => {
                normalized.push(' ');
                previous_was_carriage_return = false;
            }
            _ if ch.is_control() => {
                previous_was_carriage_return = false;
            }
            _ => {
                normalized.push(ch);
                previous_was_carriage_return = false;
            }
        }
    }

    normalized
}

fn wrapped_cursor_position(prefix: &str, width: u16) -> (u16, u16) {
    let width = usize::from(width.max(1));
    let mut row = 0usize;
    let mut column = 0usize;

    for ch in prefix.chars() {
        if ch == '\n' {
            row += 1;
            column = 0;
            continue;
        }

        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if char_width == 0 {
            continue;
        }

        if column + char_width > width {
            row += 1;
            column = 0;
        }

        column += char_width;
        if column >= width {
            row += column / width;
            column %= width;
        }
    }

    (column as u16, row as u16)
}

impl InputFieldState {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Enter
                if self.mode == InputFieldMode::MultiLine
                    && key.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                self.insert('\n');
                true
            }
            KeyCode::Backspace => {
                self.backspace();
                true
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.clear();
                true
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert(ch);
                true
            }
            KeyCode::Left => {
                self.move_left();
                true
            }
            KeyCode::Right => {
                self.move_right();
                true
            }
            KeyCode::Home => {
                self.move_home();
                true
            }
            KeyCode::End => {
                self.move_end();
                true
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SelectFieldState {
    options: Vec<String>,
    selected: usize,
}

impl SelectFieldState {
    pub(crate) fn new(options: Vec<String>, selected: usize) -> Self {
        let selected = selected.min(options.len().saturating_sub(1));
        Self { options, selected }
    }

    pub(crate) fn options(&self) -> &[String] {
        &self.options
    }

    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn selected_label(&self) -> Option<&str> {
        self.options.get(self.selected).map(String::as_str)
    }

    pub(crate) fn move_by(&mut self, delta: isize) {
        wrap_index(&mut self.selected, self.options.len(), delta);
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_by(-1);
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_by(1);
                true
            }
            KeyCode::Home => {
                self.selected = 0;
                true
            }
            KeyCode::End => {
                if !self.options.is_empty() {
                    self.selected = self.options.len() - 1;
                }
                true
            }
            _ => false,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MultiSelectFieldState {
    options: Vec<String>,
    cursor: usize,
    selected: BTreeSet<usize>,
}

#[allow(dead_code)]
impl MultiSelectFieldState {
    pub(crate) fn new(options: Vec<String>, selected: impl IntoIterator<Item = usize>) -> Self {
        let mut state = Self {
            options,
            cursor: 0,
            selected: BTreeSet::new(),
        };
        for index in selected {
            if index < state.options.len() {
                state.selected.insert(index);
            }
        }
        state
    }

    pub(crate) fn options(&self) -> &[String] {
        &self.options
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn selected_indices(&self) -> Vec<usize> {
        self.selected.iter().copied().collect()
    }

    pub(crate) fn selected_labels(&self) -> Vec<&str> {
        self.selected
            .iter()
            .filter_map(|index| self.options.get(*index).map(String::as_str))
            .collect()
    }

    pub(crate) fn toggle_current(&mut self) {
        if self.options.is_empty() {
            return;
        }

        if !self.selected.insert(self.cursor) {
            self.selected.remove(&self.cursor);
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                wrap_index(&mut self.cursor, self.options.len(), -1);
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                wrap_index(&mut self.cursor, self.options.len(), 1);
                true
            }
            KeyCode::Char(' ') => {
                self.toggle_current();
                true
            }
            _ => false,
        }
    }
}

pub(crate) fn wrap_index(index: &mut usize, len: usize, delta: isize) {
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

fn previous_boundary(value: &str, cursor: usize) -> usize {
    if cursor == 0 {
        0
    } else {
        value[..cursor]
            .char_indices()
            .last()
            .map(|(index, _)| index)
            .unwrap_or(0)
    }
}

fn next_boundary(value: &str, cursor: usize) -> usize {
    if cursor >= value.len() {
        value.len()
    } else {
        value[cursor..]
            .chars()
            .next()
            .map(|ch| cursor + ch.len_utf8())
            .unwrap_or(value.len())
    }
}

#[cfg(test)]
mod tests {
    use super::{InputFieldState, MultiSelectFieldState, SelectFieldState};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn input_field_tracks_text_and_clear() {
        let mut field = InputFieldState::new("cod");
        assert!(field.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)));
        assert!(field.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));
        assert!(field.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)));

        assert_eq!(field.value(), "");
        assert_eq!(field.cursor(), 0);
    }

    #[test]
    fn input_field_moves_cursor_and_inserts_in_place() {
        let mut field = InputFieldState::new("code");
        assert!(field.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        assert!(field.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        assert!(field.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE)));

        assert_eq!(field.value(), "coXde");
        assert_eq!(field.cursor(), 3);
    }

    #[test]
    fn input_field_paste_normalizes_multiline_text_for_single_line_fields() {
        let mut field = InputFieldState::new("Plan:");
        assert!(field.paste(" add dashboard flow\nanswer follow-up\n"));

        assert_eq!(field.value(), "Plan: add dashboard flow answer follow-up");
        assert_eq!(field.cursor(), field.value().len());
    }

    #[test]
    fn input_field_paste_preserves_newlines_for_multiline_fields() {
        let mut field = InputFieldState::multiline("Plan:");
        assert!(field.paste(" add dashboard flow\nanswer follow-up\r\n"));

        assert_eq!(
            field.value(),
            "Plan: add dashboard flow\nanswer follow-up\n"
        );
        assert_eq!(field.cursor(), field.value().len());
    }

    #[test]
    fn input_field_render_uses_terminal_cursor_instead_of_inline_caret() {
        let field = InputFieldState::new("repo");
        let render = field.render("placeholder", true);

        assert_eq!(render.text.lines[0].to_string(), "repo");
        assert_eq!(render.cursor_position(12), Some((4, 0)));
    }

    #[test]
    fn input_field_cursor_position_tracks_wrapped_multiline_content() {
        let field = InputFieldState::multiline("wide\n界z");
        let render = field.render("placeholder", true);

        assert_eq!(render.cursor_position(4), Some((3, 2)));
    }

    #[test]
    fn input_field_insert_newline_only_changes_multiline_fields() {
        let mut single_line = InputFieldState::new("repo");
        let mut multi_line = InputFieldState::multiline("repo");

        assert!(!single_line.insert_newline());
        assert_eq!(single_line.value(), "repo");

        assert!(multi_line.insert_newline());
        assert_eq!(multi_line.value(), "repo\n");
        assert_eq!(multi_line.cursor(), multi_line.value().len());
    }

    #[test]
    fn input_field_shift_enter_only_changes_multiline_fields() {
        let mut single_line = InputFieldState::new("repo");
        let mut multi_line = InputFieldState::multiline("repo");

        assert!(!single_line.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)));
        assert_eq!(single_line.value(), "repo");

        assert!(multi_line.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT)));
        assert_eq!(multi_line.value(), "repo\n");
        assert_eq!(multi_line.cursor(), multi_line.value().len());
    }

    #[test]
    fn input_field_plain_enter_does_not_change_multiline_fields() {
        let mut multi_line = InputFieldState::multiline("repo");

        assert!(!multi_line.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(multi_line.value(), "repo");
        assert_eq!(multi_line.cursor(), multi_line.value().len());
    }

    #[test]
    fn select_field_wraps_navigation() {
        let mut field = SelectFieldState::new(
            vec![
                "codex".to_string(),
                "claude".to_string(),
                "cursor".to_string(),
            ],
            0,
        );

        assert!(field.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)));
        assert_eq!(field.selected_label(), Some("cursor"));
        assert!(field.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(field.selected_label(), Some("codex"));
        assert!(field.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)));
        assert_eq!(field.selected(), 2);
    }

    #[test]
    fn multi_select_field_tracks_toggles() {
        let mut field = MultiSelectFieldState::new(
            vec!["one".to_string(), "two".to_string(), "three".to_string()],
            [1],
        );

        assert!(field.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert!(field.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)));

        assert_eq!(field.cursor(), 1);
        assert_eq!(field.selected_indices(), Vec::<usize>::new());
        assert!(field.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert!(field.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)));
        assert_eq!(field.selected_labels(), vec!["three"]);
    }
}

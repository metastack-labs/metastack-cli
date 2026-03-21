use std::collections::BTreeSet;

use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph, Wrap};
use unicode_width::UnicodeWidthChar;

use crate::tui::prompt_images::{
    ClipboardPromptPaste, MAX_PROMPT_IMAGES, PromptImageAttachment,
    resolve_attachment_from_pasted_text, resolve_clipboard_prompt_paste,
};
use crate::tui::scroll::{ScrollState, clamp_offset, wrapped_rows};

const ATTACHMENT_MARKER: char = '\u{fffc}';

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputFieldState {
    value: String,
    cursor: usize,
    mode: InputFieldMode,
    attachment_mode: AttachmentMode,
    attachments: Vec<PromptImageAttachment>,
    preferred_column: Option<usize>,
    scroll: ScrollState,
}

#[derive(Debug, Clone)]
pub(crate) struct InputFieldRender {
    pub(crate) text: Text<'static>,
    cursor_prefix: Option<String>,
    pub(crate) scroll_offset: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputFieldMode {
    SingleLine,
    MultiLine,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AttachmentMode {
    Disabled,
    Enabled,
    Rejected { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AttachmentPasteOutcome {
    NoChange,
    TextPasted,
    AttachmentPasted,
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
            attachment_mode: AttachmentMode::Disabled,
            attachments: Vec::new(),
            preferred_column: None,
            scroll: ScrollState::default(),
        }
    }

    pub(crate) fn multiline(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            cursor: value.len(),
            value,
            mode: InputFieldMode::MultiLine,
            attachment_mode: AttachmentMode::Disabled,
            attachments: Vec::new(),
            preferred_column: None,
            scroll: ScrollState::default(),
        }
    }

    pub(crate) fn multiline_with_prompt_attachments(value: impl Into<String>) -> Self {
        let value = value.into();
        Self {
            cursor: value.len(),
            value,
            mode: InputFieldMode::MultiLine,
            attachment_mode: AttachmentMode::Enabled,
            attachments: Vec::new(),
            preferred_column: None,
            scroll: ScrollState::default(),
        }
    }

    pub(crate) fn multiline_rejecting_prompt_attachments(
        value: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let value = value.into();
        Self {
            cursor: value.len(),
            value,
            mode: InputFieldMode::MultiLine,
            attachment_mode: AttachmentMode::Rejected {
                message: message.into(),
            },
            attachments: Vec::new(),
            preferred_column: None,
            scroll: ScrollState::default(),
        }
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn display_value(&self) -> String {
        render_value_with_attachments(&self.value, &self.attachments)
    }

    pub(crate) fn prompt_attachments(&self) -> &[PromptImageAttachment] {
        &self.attachments
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn render(&self, placeholder: &str, active: bool) -> InputFieldRender {
        self.render_with_viewport(placeholder, active, 1, 1)
    }

    pub(crate) fn render_with_width(
        &self,
        placeholder: &str,
        active: bool,
        width: u16,
    ) -> InputFieldRender {
        self.render_with_viewport(placeholder, active, width, 1)
    }

    pub(crate) fn render_with_viewport(
        &self,
        placeholder: &str,
        active: bool,
        width: u16,
        height: u16,
    ) -> InputFieldRender {
        let display_value = self.display_value();
        if display_value.is_empty() {
            let text = Text::from(Line::styled(
                placeholder.to_string(),
                Style::default().add_modifier(Modifier::DIM),
            ));
            return InputFieldRender {
                text,
                cursor_prefix: active.then(String::new),
                scroll_offset: 0,
            };
        }

        let content_rows = wrapped_rows(&display_value, width);
        let scroll_offset = clamp_offset(self.scroll.offset(), height, content_rows);

        if !active {
            return InputFieldRender {
                text: Text::from(display_value),
                cursor_prefix: None,
                scroll_offset,
            };
        }

        InputFieldRender {
            text: Text::from(display_value),
            cursor_prefix: Some(render_prefix_with_attachments(&self.value[..self.cursor])),
            scroll_offset,
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
        self.paste_normalized_text(text)
    }

    pub(crate) fn paste_with_prompt_attachments(
        &mut self,
        text: &str,
    ) -> Result<AttachmentPasteOutcome> {
        match &self.attachment_mode {
            AttachmentMode::Disabled => Ok(if self.paste_normalized_text(text) {
                AttachmentPasteOutcome::TextPasted
            } else {
                AttachmentPasteOutcome::NoChange
            }),
            AttachmentMode::Rejected { message } => {
                let message = message.clone();
                self.paste_or_reject_prompt_attachment_text(text, &message)
            }
            AttachmentMode::Enabled => self.paste_prompt_attachment_text(text),
        }
    }

    pub(crate) fn paste_clipboard_with_prompt_attachments(
        &mut self,
    ) -> Result<AttachmentPasteOutcome> {
        match &self.attachment_mode {
            AttachmentMode::Disabled => Ok(AttachmentPasteOutcome::NoChange),
            AttachmentMode::Rejected { message } => {
                let message = message.clone();
                match resolve_clipboard_prompt_paste()? {
                    ClipboardPromptPaste::Attachment(_) => bail!("{message}"),
                    ClipboardPromptPaste::Text(text) => {
                        self.paste_or_reject_prompt_attachment_text(&text, &message)
                    }
                    ClipboardPromptPaste::Empty => Ok(AttachmentPasteOutcome::NoChange),
                }
            }
            AttachmentMode::Enabled => match resolve_clipboard_prompt_paste()? {
                ClipboardPromptPaste::Attachment(attachment) => {
                    self.insert_attachment(attachment)?;
                    Ok(AttachmentPasteOutcome::AttachmentPasted)
                }
                ClipboardPromptPaste::Text(text) => self.paste_prompt_attachment_text(&text),
                ClipboardPromptPaste::Empty => Ok(AttachmentPasteOutcome::NoChange),
            },
        }
    }

    fn paste_prompt_attachment_text(&mut self, text: &str) -> Result<AttachmentPasteOutcome> {
        if let Some(attachment) = resolve_attachment_from_pasted_text(text)? {
            self.insert_attachment(attachment)?;
            return Ok(AttachmentPasteOutcome::AttachmentPasted);
        }

        Ok(if self.paste_normalized_text(text) {
            AttachmentPasteOutcome::TextPasted
        } else {
            AttachmentPasteOutcome::NoChange
        })
    }

    fn paste_or_reject_prompt_attachment_text(
        &mut self,
        text: &str,
        message: &str,
    ) -> Result<AttachmentPasteOutcome> {
        if resolve_attachment_from_pasted_text(text)?.is_some() {
            bail!("{message}");
        }

        Ok(if self.paste_normalized_text(text) {
            AttachmentPasteOutcome::TextPasted
        } else {
            AttachmentPasteOutcome::NoChange
        })
    }

    fn paste_normalized_text(&mut self, text: &str) -> bool {
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
    pub(crate) fn paragraph(&self, block: Block<'static>) -> Paragraph<'static> {
        Paragraph::new(self.text.clone())
            .block(block)
            .scroll((self.scroll_offset, 0))
            .wrap(Wrap { trim: false })
    }

    pub(crate) fn set_cursor(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let Some(prefix) = &self.cursor_prefix else {
            return;
        };

        let (column, row) = wrapped_cursor_position(prefix, area.width);
        if row >= self.scroll_offset && row - self.scroll_offset < area.height {
            frame.set_cursor_position((area.x + column, area.y + row - self.scroll_offset));
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
        self.preferred_column = None;
    }

    fn insert_attachment(&mut self, attachment: PromptImageAttachment) -> Result<()> {
        if self.attachments.len() >= MAX_PROMPT_IMAGES {
            bail!("prompt editors support at most {MAX_PROMPT_IMAGES} image attachments");
        }

        let attachment_index = attachment_index_for_cursor(&self.value, self.cursor);
        self.value.insert(self.cursor, ATTACHMENT_MARKER);
        self.cursor += ATTACHMENT_MARKER.len_utf8();
        self.attachments.insert(attachment_index, attachment);
        self.preferred_column = None;
        Ok(())
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let start = previous_boundary(&self.value, self.cursor);
        if self.value[start..self.cursor].starts_with(ATTACHMENT_MARKER) {
            self.remove_attachment_at_raw_index(start);
            return;
        }
        self.value.drain(start..self.cursor);
        self.cursor = start;
        self.preferred_column = None;
    }

    fn delete_forward(&mut self) {
        if self.cursor >= self.value.len() {
            return;
        }

        let end = next_boundary(&self.value, self.cursor);
        if self.value[self.cursor..end].starts_with(ATTACHMENT_MARKER) {
            self.remove_attachment_at_raw_index(self.cursor);
            return;
        }
        self.value.drain(self.cursor..end);
        self.preferred_column = None;
    }

    fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.attachments.clear();
        self.preferred_column = None;
        self.scroll.reset();
    }

    fn move_left(&mut self) {
        self.cursor = previous_boundary(&self.value, self.cursor);
        self.preferred_column = None;
    }

    fn move_right(&mut self) {
        self.cursor = next_boundary(&self.value, self.cursor);
        self.preferred_column = None;
    }

    fn move_home(&mut self) {
        self.cursor = 0;
        self.preferred_column = None;
    }

    fn move_end(&mut self) {
        self.cursor = self.value.len();
        self.preferred_column = None;
    }

    fn remove_attachment_at_raw_index(&mut self, raw_index: usize) {
        let attachment_index = attachment_index_before_raw_index(&self.value, raw_index);
        let end = next_boundary(&self.value, raw_index);
        self.value.drain(raw_index..end);
        self.cursor = raw_index;
        if attachment_index < self.attachments.len() {
            self.attachments.remove(attachment_index);
        }
        self.preferred_column = None;
    }

    fn move_up(&mut self, width: u16) {
        self.move_vertical(width, -1);
    }

    fn move_down(&mut self, width: u16) {
        self.move_vertical(width, 1);
    }

    fn move_page_up(&mut self, width: u16, height: u16) {
        self.move_vertical(width, -(height.saturating_sub(1).max(1) as isize));
    }

    fn move_page_down(&mut self, width: u16, height: u16) {
        self.move_vertical(width, height.saturating_sub(1).max(1) as isize);
    }

    fn move_vertical(&mut self, width: u16, delta: isize) {
        if self.mode != InputFieldMode::MultiLine {
            return;
        }

        let points = cursor_points(&self.value, width);
        let Some(current_index) = points.iter().position(|point| point.byte == self.cursor) else {
            return;
        };
        let current = &points[current_index];
        let preferred_column = self.preferred_column.unwrap_or(current.column);
        let target_row = current.row as isize + delta;
        if target_row < 0 {
            self.cursor = points
                .iter()
                .find(|point| point.row == 0)
                .map(|point| point.byte)
                .unwrap_or(0);
            self.preferred_column = Some(preferred_column);
            return;
        }

        let target_row = target_row as usize;
        let mut best_match = None;
        for point in points.iter().filter(|point| point.row == target_row) {
            match best_match {
                None => best_match = Some(point),
                Some(best) if point.column <= preferred_column && point.column >= best.column => {
                    best_match = Some(point);
                }
                Some(best) if best.column > preferred_column && point.column < best.column => {
                    best_match = Some(point);
                }
                _ => {}
            }
        }

        if let Some(target) = best_match {
            self.cursor = target.byte;
            self.preferred_column = Some(preferred_column);
        }
    }

    fn sync_cursor_scroll(&mut self, width: u16, height: u16) {
        let prefix = render_prefix_with_attachments(&self.value[..self.cursor]);
        let (_, row) = wrapped_cursor_position(&prefix, width.max(1));
        let content_rows = wrapped_rows(&self.display_value(), width.max(1));
        let _ = self
            .scroll
            .ensure_row_visible(row, height.max(1), content_rows.max(1));
    }

    pub(crate) fn handle_mouse_scroll(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        area: Rect,
        width: u16,
        _height: u16,
    ) -> bool {
        if self.mode != InputFieldMode::MultiLine {
            return false;
        }

        let content_rows = wrapped_rows(&self.display_value(), width.max(1));
        self.scroll
            .apply_mouse_in_viewport(mouse, area, content_rows)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorPoint {
    byte: usize,
    row: usize,
    column: usize,
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

fn render_value_with_attachments(value: &str, _attachments: &[PromptImageAttachment]) -> String {
    let mut rendered = String::new();
    let mut attachment_index = 0usize;

    for ch in value.chars() {
        if ch == ATTACHMENT_MARKER {
            rendered.push_str(&format!("[Image #{}]", attachment_index + 1));
            attachment_index += 1;
        } else {
            rendered.push(ch);
        }
    }

    rendered
}

fn render_prefix_with_attachments(value: &str) -> String {
    render_value_with_attachments(value, &[])
}

fn attachment_index_for_cursor(value: &str, cursor: usize) -> usize {
    value[..cursor]
        .chars()
        .filter(|ch| *ch == ATTACHMENT_MARKER)
        .count()
}

fn attachment_index_before_raw_index(value: &str, raw_index: usize) -> usize {
    value
        .char_indices()
        .take_while(|(index, _)| *index < raw_index)
        .filter(|(_, ch)| *ch == ATTACHMENT_MARKER)
        .count()
}

fn wrapped_cursor_position(prefix: &str, width: u16) -> (u16, u16) {
    let boundaries = wrapped_cursor_boundaries(prefix, width);
    boundaries
        .last()
        .map(|boundary| (boundary.column, boundary.row))
        .unwrap_or((0, 0))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorBoundary {
    byte: usize,
    column: u16,
    row: u16,
}

fn wrapped_cursor_boundaries(value: &str, width: u16) -> Vec<CursorBoundary> {
    let width = usize::from(width.max(1));
    let mut boundaries = Vec::with_capacity(value.chars().count() + 1);
    let mut row = 0usize;
    let mut column = 0usize;
    boundaries.push(CursorBoundary {
        byte: 0,
        column: 0,
        row: 0,
    });

    for (byte_index, ch) in value.char_indices() {
        if ch == '\n' {
            row += 1;
            column = 0;
        } else {
            let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if char_width > 0 && column + char_width > width {
                row += 1;
                column = 0;
            }

            column += char_width;
            if column >= width {
                row += column / width;
                column %= width;
            }
        }

        boundaries.push(CursorBoundary {
            byte: byte_index + ch.len_utf8(),
            column: column as u16,
            row: row as u16,
        });
    }

    boundaries
}

impl InputFieldState {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        self.handle_key_with_viewport(key, 1, 1)
    }

    pub(crate) fn handle_key_with_width(&mut self, key: KeyEvent, width: u16) -> bool {
        self.handle_key_with_viewport(key, width, 1)
    }

    pub(crate) fn handle_key_with_viewport(
        &mut self,
        key: KeyEvent,
        width: u16,
        height: u16,
    ) -> bool {
        let handled = match key.code {
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
            KeyCode::Delete => {
                self.delete_forward();
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
            KeyCode::Up if self.mode == InputFieldMode::MultiLine => {
                self.move_up(width);
                true
            }
            KeyCode::Down if self.mode == InputFieldMode::MultiLine => {
                self.move_down(width);
                true
            }
            KeyCode::PageUp if self.mode == InputFieldMode::MultiLine => {
                self.move_page_up(width, height);
                true
            }
            KeyCode::PageDown if self.mode == InputFieldMode::MultiLine => {
                self.move_page_down(width, height);
                true
            }
            _ => false,
        };

        if handled && self.mode == InputFieldMode::MultiLine {
            self.sync_cursor_scroll(width, height);
        }

        handled
    }
}

fn cursor_points(value: &str, width: u16) -> Vec<CursorPoint> {
    let width = usize::from(width.max(1));
    let mut points = Vec::with_capacity(value.chars().count() + 1);
    let mut row = 0usize;
    let mut column = 0usize;

    points.push(CursorPoint {
        byte: 0,
        row,
        column,
    });

    for (index, ch) in value.char_indices() {
        if ch == '\n' {
            row += 1;
            column = 0;
            points.push(CursorPoint {
                byte: index + ch.len_utf8(),
                row,
                column,
            });
            continue;
        }

        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if char_width > 0 {
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

        points.push(CursorPoint {
            byte: index + ch.len_utf8(),
            row,
            column,
        });
    }

    points
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
            KeyCode::Up => {
                self.move_by(-1);
                true
            }
            KeyCode::Down => {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FilterableSelectFieldState {
    all_options: Vec<String>,
    filter: String,
    filter_cursor: usize,
    visible_indices: Vec<usize>,
    cursor: usize,
}

impl FilterableSelectFieldState {
    pub(crate) fn new(options: Vec<String>) -> Self {
        let visible_indices: Vec<usize> = (0..options.len()).collect();
        Self {
            all_options: options,
            filter: String::new(),
            filter_cursor: 0,
            visible_indices,
            cursor: 0,
        }
    }

    pub(crate) fn filter_value(&self) -> &str {
        &self.filter
    }

    pub(crate) fn visible_options(&self) -> Vec<&str> {
        self.visible_indices
            .iter()
            .map(|&i| self.all_options[i].as_str())
            .collect()
    }

    pub(crate) fn cursor_index(&self) -> usize {
        self.cursor
    }

    /// Returns the index into `all_options` for the currently highlighted item.
    pub(crate) fn selected_original_index(&self) -> Option<usize> {
        self.visible_indices.get(self.cursor).copied()
    }

    pub(crate) fn selected_label(&self) -> Option<&str> {
        self.selected_original_index()
            .map(|i| self.all_options[i].as_str())
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Up => {
                if !self.visible_indices.is_empty() {
                    wrap_index(&mut self.cursor, self.visible_indices.len(), -1);
                }
                true
            }
            KeyCode::Down => {
                if !self.visible_indices.is_empty() {
                    wrap_index(&mut self.cursor, self.visible_indices.len(), 1);
                }
                true
            }
            KeyCode::Backspace => {
                if self.filter_cursor > 0 {
                    let start = previous_boundary(&self.filter, self.filter_cursor);
                    self.filter.drain(start..self.filter_cursor);
                    self.filter_cursor = start;
                    self.refilter();
                }
                true
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.clear();
                self.filter_cursor = 0;
                self.refilter();
                true
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.insert(self.filter_cursor, ch);
                self.filter_cursor += ch.len_utf8();
                self.refilter();
                true
            }
            _ => false,
        }
    }

    fn refilter(&mut self) {
        let query = self.filter.to_lowercase();
        self.visible_indices = if query.is_empty() {
            (0..self.all_options.len()).collect()
        } else {
            self.all_options
                .iter()
                .enumerate()
                .filter(|(_, opt)| opt.to_lowercase().contains(&query))
                .map(|(i, _)| i)
                .collect()
        };
        self.cursor = 0;
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
            KeyCode::Up => {
                wrap_index(&mut self.cursor, self.options.len(), -1);
                true
            }
            KeyCode::Down => {
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
    use image::{ImageBuffer, Rgba};
    use tempfile::tempdir;

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
    fn prompt_attachment_paste_inserts_placeholder_and_tracks_order() {
        let temp = tempdir().expect("temp dir");
        let first_path = temp.path().join("first.png");
        let second_path = temp.path().join("second.png");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([1, 2, 3, 255]))
            .save(&first_path)
            .expect("save first");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([4, 5, 6, 255]))
            .save(&second_path)
            .expect("save second");

        let mut field = InputFieldState::multiline_with_prompt_attachments("Plan ");
        field
            .paste_with_prompt_attachments(first_path.to_str().expect("utf8"))
            .expect("first attachment");
        field
            .paste_with_prompt_attachments(second_path.to_str().expect("utf8"))
            .expect("second attachment");

        assert_eq!(field.display_value(), "Plan [Image #1][Image #2]");
        assert_eq!(field.prompt_attachments().len(), 2);
    }

    #[test]
    fn backspace_removes_attachment_and_renumbers_placeholders() {
        let temp = tempdir().expect("temp dir");
        let first_path = temp.path().join("first.png");
        let second_path = temp.path().join("second.png");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([1, 2, 3, 255]))
            .save(&first_path)
            .expect("save first");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([4, 5, 6, 255]))
            .save(&second_path)
            .expect("save second");

        let mut field = InputFieldState::multiline_with_prompt_attachments(String::new());
        field
            .paste_with_prompt_attachments(first_path.to_str().expect("utf8"))
            .expect("first attachment");
        field
            .paste_with_prompt_attachments(second_path.to_str().expect("utf8"))
            .expect("second attachment");
        assert!(field.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        assert!(field.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)));

        assert_eq!(field.display_value(), "[Image #1]");
        assert_eq!(field.prompt_attachments().len(), 1);
    }

    #[test]
    fn delete_forward_removes_attachment_and_renumbers_placeholders() {
        let temp = tempdir().expect("temp dir");
        let first_path = temp.path().join("first.png");
        let second_path = temp.path().join("second.png");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([1, 2, 3, 255]))
            .save(&first_path)
            .expect("save first");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([4, 5, 6, 255]))
            .save(&second_path)
            .expect("save second");

        let mut field = InputFieldState::multiline_with_prompt_attachments(String::new());
        field
            .paste_with_prompt_attachments(first_path.to_str().expect("utf8"))
            .expect("first attachment");
        field
            .paste_with_prompt_attachments(second_path.to_str().expect("utf8"))
            .expect("second attachment");
        assert!(field.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE)));
        assert!(field.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)));

        assert_eq!(field.display_value(), "[Image #1]");
        assert_eq!(field.prompt_attachments().len(), 1);
    }

    #[test]
    fn prompt_attachment_paste_falls_back_to_text_for_non_image_paths() {
        let temp = tempdir().expect("temp dir");
        let note_path = temp.path().join("notes.txt");
        std::fs::write(&note_path, "plain text").expect("save note");

        let mut field = InputFieldState::multiline_with_prompt_attachments("Plan: ");
        let outcome = field
            .paste_with_prompt_attachments(note_path.to_str().expect("utf8"))
            .expect("text fallback");

        assert_eq!(outcome, super::AttachmentPasteOutcome::TextPasted);
        assert_eq!(
            field.value(),
            format!("Plan: {}", note_path.to_str().expect("utf8"))
        );
        assert!(field.prompt_attachments().is_empty());
    }

    #[test]
    fn clipboard_text_image_path_is_normalized_into_an_attachment() {
        let temp = tempdir().expect("temp dir");
        let image_path = temp.path().join("clipboard-image.png");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([1, 2, 3, 255]))
            .save(&image_path)
            .expect("save image");

        let mut field = InputFieldState::multiline_with_prompt_attachments("Plan: ");
        let outcome = field
            .paste_prompt_attachment_text(image_path.to_str().expect("utf8"))
            .expect("attachment from clipboard text");

        assert_eq!(outcome, super::AttachmentPasteOutcome::AttachmentPasted);
        assert_eq!(field.display_value(), "Plan: [Image #1]");
        assert_eq!(field.prompt_attachments().len(), 1);
    }

    #[test]
    fn rejecting_prompt_attachment_fields_reject_clipboard_image_paths() {
        let temp = tempdir().expect("temp dir");
        let image_path = temp.path().join("clipboard-image.png");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([1, 2, 3, 255]))
            .save(&image_path)
            .expect("save image");

        let mut field = InputFieldState::multiline_rejecting_prompt_attachments(
            String::new(),
            "attachments disabled",
        );
        let error = field
            .paste_or_reject_prompt_attachment_text(
                image_path.to_str().expect("utf8"),
                "attachments disabled",
            )
            .expect_err("image path should be rejected");

        assert!(error.to_string().contains("attachments disabled"));
        assert_eq!(field.value(), "");
        assert!(field.prompt_attachments().is_empty());
    }

    #[test]
    fn prompt_attachment_paste_enforces_the_documented_cap() {
        let temp = tempdir().expect("temp dir");
        let mut paths = Vec::new();
        for index in 0..=5 {
            let path = temp.path().join(format!("{index}.png"));
            ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([1, 2, 3, 255]))
                .save(&path)
                .expect("save image");
            paths.push(path);
        }

        let mut field = InputFieldState::multiline_with_prompt_attachments(String::new());
        for path in paths.iter().take(5) {
            field
                .paste_with_prompt_attachments(path.to_str().expect("utf8"))
                .expect("attachment within cap");
        }

        let error = field
            .paste_with_prompt_attachments(paths[5].to_str().expect("utf8"))
            .expect_err("sixth attachment should fail");
        assert!(
            error
                .to_string()
                .contains("prompt editors support at most 5 image attachments")
        );
        assert_eq!(field.prompt_attachments().len(), 5);
    }

    #[test]
    fn input_field_up_down_moves_between_wrapped_lines() {
        let mut field = InputFieldState::multiline("12345\n12");

        assert!(field.handle_key_with_width(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 4));
        assert_eq!(field.cursor(), 5);

        assert!(field.handle_key_with_width(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 4));
        assert_eq!(field.cursor(), field.value().len());
    }

    #[test]
    fn input_field_vertical_navigation_preserves_preferred_column() {
        let mut field = InputFieldState::multiline("abcdef\nab\nabcdef");

        assert!(field.handle_key_with_width(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 8));
        assert_eq!(field.cursor(), "abcdef\nab".len());

        assert!(field.handle_key_with_width(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), 8));
        assert_eq!(field.cursor(), 6);

        assert!(field.handle_key_with_width(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), 8));
        assert_eq!(field.cursor(), "abcdef\nab".len());
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

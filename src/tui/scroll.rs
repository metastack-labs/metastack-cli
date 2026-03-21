use crossterm::event::{KeyCode, KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph};
use unicode_width::UnicodeWidthChar;

use crate::tui::theme::panel;

/// Shared vertical scroll state for wrapped TUI panes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScrollState {
    offset: u16,
}

impl ScrollState {
    pub(crate) fn offset(&self) -> u16 {
        self.offset
    }

    pub(crate) fn reset(&mut self) {
        self.offset = 0;
    }

    pub(crate) fn ensure_row_visible(
        &mut self,
        row: u16,
        viewport_height: u16,
        content_rows: usize,
    ) -> bool {
        let clamped = clamp_offset(self.offset, viewport_height, content_rows);
        let end = clamped.saturating_add(viewport_height.max(1));
        let next = if row < clamped {
            row
        } else if row >= end {
            row.saturating_add(1).saturating_sub(viewport_height.max(1))
        } else {
            clamped
        };
        self.set_offset(next, viewport_height, content_rows)
    }

    pub(crate) fn apply_key(
        &mut self,
        key: KeyEvent,
        viewport_height: u16,
        content_rows: usize,
    ) -> bool {
        match key.code {
            KeyCode::Up => self.scroll_lines(-1, viewport_height, content_rows),
            KeyCode::Down => self.scroll_lines(1, viewport_height, content_rows),
            KeyCode::PageUp => self.scroll_lines(
                -(page_step(viewport_height) as isize),
                viewport_height,
                content_rows,
            ),
            KeyCode::PageDown => self.scroll_lines(
                page_step(viewport_height) as isize,
                viewport_height,
                content_rows,
            ),
            KeyCode::Home => self.set_offset(0, viewport_height, content_rows),
            KeyCode::End => self.set_offset(
                max_offset(viewport_height, content_rows),
                viewport_height,
                content_rows,
            ),
            _ => false,
        }
    }

    pub(crate) fn apply_key_in_viewport(
        &mut self,
        key: KeyEvent,
        viewport: Rect,
        content_rows: usize,
    ) -> bool {
        self.apply_key(key, viewport.height.max(1), content_rows.max(1))
    }

    pub(crate) fn apply_key_code_in_viewport(
        &mut self,
        code: KeyCode,
        viewport: Rect,
        content_rows: usize,
    ) -> bool {
        self.apply_key_in_viewport(KeyEvent::from(code), viewport, content_rows)
    }

    pub(crate) fn apply_mouse(
        &mut self,
        mouse: MouseEvent,
        area: Rect,
        viewport_height: u16,
        content_rows: usize,
    ) -> bool {
        if !contains(area, mouse.column, mouse.row) {
            return false;
        }

        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll_lines(-3, viewport_height, content_rows),
            MouseEventKind::ScrollDown => self.scroll_lines(3, viewport_height, content_rows),
            _ => false,
        }
    }

    pub(crate) fn apply_mouse_in_viewport(
        &mut self,
        mouse: MouseEvent,
        viewport: Rect,
        content_rows: usize,
    ) -> bool {
        self.apply_mouse(mouse, viewport, viewport.height.max(1), content_rows.max(1))
    }

    fn scroll_lines(&mut self, delta: isize, viewport_height: u16, content_rows: usize) -> bool {
        let current = clamp_offset(self.offset, viewport_height, content_rows);
        let next = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs() as u16)
        } else {
            current.saturating_add(delta as u16)
        };
        self.set_offset(next, viewport_height, content_rows)
    }

    fn set_offset(&mut self, offset: u16, viewport_height: u16, content_rows: usize) -> bool {
        let next = clamp_offset(offset, viewport_height, content_rows);
        if next == self.offset {
            return false;
        }
        self.offset = next;
        true
    }
}

/// Returns the wrapped row count for `value` rendered in `width`.
pub(crate) fn wrapped_rows(value: &str, width: u16) -> usize {
    let width = usize::from(width.max(1));
    value
        .split('\n')
        .map(|line| {
            let columns = line
                .chars()
                .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
                .sum::<usize>();
            columns.max(1).div_ceil(width)
        })
        .sum()
}

/// Returns a plain string representation of `text`, preserving line breaks.
pub(crate) fn plain_text(text: &Text<'_>) -> String {
    text.lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Shared scrollable paragraph builder for wrapped text panels.
pub(crate) fn scrollable_paragraph(
    text: impl Into<Text<'static>>,
    title: impl Into<Line<'static>>,
    scroll: &ScrollState,
) -> Paragraph<'static> {
    scrollable_paragraph_with_block(text, panel(title), scroll)
}

/// Shared scrollable paragraph builder that preserves a caller-provided block.
pub(crate) fn scrollable_paragraph_with_block(
    text: impl Into<Text<'static>>,
    block: Block<'static>,
    scroll: &ScrollState,
) -> Paragraph<'static> {
    Paragraph::new(text.into())
        .block(block)
        .scroll((scroll.offset(), 0))
}

pub(crate) fn clamp_offset(offset: u16, viewport_height: u16, content_rows: usize) -> u16 {
    offset.min(max_offset(viewport_height, content_rows))
}

fn max_offset(viewport_height: u16, content_rows: usize) -> u16 {
    content_rows.saturating_sub(usize::from(viewport_height.max(1))) as u16
}

fn page_step(viewport_height: u16) -> u16 {
    viewport_height.saturating_sub(1).max(1)
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;

    use super::{ScrollState, clamp_offset, plain_text, wrapped_rows};

    #[test]
    fn wrapped_rows_counts_wrapped_and_explicit_lines() {
        assert_eq!(wrapped_rows("abcdef", 3), 2);
        assert_eq!(wrapped_rows("ab\ncd", 3), 2);
    }

    #[test]
    fn clamp_offset_respects_viewport_height() {
        assert_eq!(clamp_offset(0, 4, 2), 0);
        assert_eq!(clamp_offset(9, 4, 10), 6);
    }

    #[test]
    fn scroll_state_handles_keyboard_navigation() {
        let mut state = ScrollState::default();
        let handled = state.apply_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), 4, 12);
        assert!(handled);
        assert_eq!(state.offset(), 3);
        let handled = state.apply_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), 4, 12);
        assert!(handled);
        assert_eq!(state.offset(), 8);
    }

    #[test]
    fn scroll_state_handles_mouse_wheel() {
        let mut state = ScrollState::default();
        let handled = state.apply_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 2,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 10, 10),
            4,
            12,
        );
        assert!(handled);
        assert_eq!(state.offset(), 3);
        let handled = state.apply_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 2,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
            Rect::new(0, 0, 10, 10),
            4,
            12,
        );
        assert!(handled);
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn scroll_state_handles_viewport_helpers() {
        let mut state = ScrollState::default();
        let viewport = Rect::new(0, 0, 10, 4);

        let handled = state.apply_key_code_in_viewport(KeyCode::End, viewport, 12);
        assert!(handled);
        assert_eq!(state.offset(), 8);

        let handled = state.apply_mouse_in_viewport(
            MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 2,
                row: 2,
                modifiers: KeyModifiers::NONE,
            },
            viewport,
            12,
        );
        assert!(handled);
        assert_eq!(state.offset(), 5);
    }

    #[test]
    fn plain_text_preserves_rendered_lines() {
        let text = ratatui::text::Text::from(vec![
            ratatui::text::Line::from("alpha"),
            ratatui::text::Line::from("beta"),
        ]);
        assert_eq!(plain_text(&text), "alpha\nbeta");
    }
}

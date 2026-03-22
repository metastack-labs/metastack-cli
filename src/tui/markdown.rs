use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use crate::tui::theme::{Tone, tone_style};

/// A source-byte range that should receive highlight treatment in rendered markdown output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MarkdownHighlight {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MarkdownLineKind {
    Blank,
    Paragraph,
    Heading { level: usize },
    Bullet,
    Blockquote,
    FenceDelimiter,
    Code,
}

/// Render a narrow markdown subset into ratatui text while preserving source line breaks.
///
/// Unsupported markdown is rendered as plain paragraph text with the provided `base_style`.
pub(crate) fn render_markdown(
    markdown: &str,
    base_style: Style,
    highlights: &[MarkdownHighlight],
) -> Text<'static> {
    let mut lines = Vec::new();
    let mut line_start = 0usize;
    let mut in_fence = false;

    for raw_line in markdown.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let kind = classify_line(line, in_fence);
        if matches!(kind, MarkdownLineKind::FenceDelimiter) {
            in_fence = !in_fence;
        }

        let rendered = match kind {
            MarkdownLineKind::Blank => Line::from(""),
            MarkdownLineKind::Paragraph => {
                Line::from(highlighted_spans(line, line_start, highlights, base_style))
            }
            MarkdownLineKind::Heading { level } => Line::from(highlighted_spans(
                line,
                line_start,
                highlights,
                heading_style(level),
            )),
            MarkdownLineKind::Bullet => Line::from(highlighted_spans(
                line,
                line_start,
                highlights,
                bullet_style(base_style),
            )),
            MarkdownLineKind::Blockquote => Line::from(highlighted_spans(
                line,
                line_start,
                highlights,
                blockquote_style(),
            )),
            MarkdownLineKind::FenceDelimiter => Line::from(highlighted_spans(
                line,
                line_start,
                highlights,
                fence_style(),
            )),
            MarkdownLineKind::Code => Line::from(highlighted_spans(
                line,
                line_start,
                highlights,
                code_style(),
            )),
        };
        lines.push(rendered);
        line_start += raw_line.len() + 1;
    }

    Text::from(lines)
}

fn classify_line(line: &str, in_fence: bool) -> MarkdownLineKind {
    if line.trim().is_empty() {
        return MarkdownLineKind::Blank;
    }

    if is_fence_delimiter(line) {
        return MarkdownLineKind::FenceDelimiter;
    }

    if in_fence {
        return MarkdownLineKind::Code;
    }

    if let Some(level) = heading_level(line) {
        return MarkdownLineKind::Heading { level };
    }

    if is_blockquote(line) {
        return MarkdownLineKind::Blockquote;
    }

    if is_bullet(line) {
        return MarkdownLineKind::Bullet;
    }

    MarkdownLineKind::Paragraph
}

fn heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    let count = trimmed
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if (1..=6).contains(&count) && trimmed.as_bytes().get(count) == Some(&b' ') {
        Some(count)
    } else {
        None
    }
}

fn is_blockquote(line: &str) -> bool {
    line.trim_start().starts_with("> ")
}

fn is_bullet(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("- ") || trimmed.starts_with("* ") || trimmed.starts_with("+ ")
}

fn is_fence_delimiter(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

fn highlighted_spans(
    line: &str,
    line_start: usize,
    highlights: &[MarkdownHighlight],
    base_style: Style,
) -> Vec<Span<'static>> {
    let line_end = line_start + line.len();
    let relevant = highlights
        .iter()
        .filter_map(|highlight| {
            if highlight.end <= line_start || highlight.start >= line_end {
                return None;
            }

            Some(MarkdownHighlight {
                start: highlight.start.saturating_sub(line_start),
                end: highlight.end.min(line_end).saturating_sub(line_start),
            })
        })
        .collect::<Vec<_>>();

    if relevant.is_empty() {
        return vec![Span::styled(line.to_string(), base_style)];
    }

    let mut spans = Vec::new();
    let mut cursor = 0usize;
    for highlight in relevant {
        if cursor < highlight.start {
            spans.push(Span::styled(
                line[cursor..highlight.start].to_string(),
                base_style,
            ));
        }
        spans.push(Span::styled(
            line[highlight.start..highlight.end].to_string(),
            base_style.patch(highlight_style()),
        ));
        cursor = highlight.end;
    }
    if cursor < line.len() {
        spans.push(Span::styled(line[cursor..].to_string(), base_style));
    }
    spans
}

fn heading_style(level: usize) -> Style {
    let emphasis = match level {
        1 => Modifier::BOLD | Modifier::UNDERLINED,
        2 => Modifier::BOLD,
        _ => Modifier::BOLD | Modifier::ITALIC,
    };

    tone_style(Tone::Accent).add_modifier(emphasis)
}

fn bullet_style(base_style: Style) -> Style {
    base_style.add_modifier(Modifier::BOLD)
}

fn blockquote_style() -> Style {
    tone_style(Tone::Info).add_modifier(Modifier::ITALIC)
}

fn fence_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

fn code_style() -> Style {
    Style::default().fg(Color::Green)
}

fn highlight_style() -> Style {
    Style::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier, Style};

    use super::{MarkdownHighlight, render_markdown};
    use crate::tui::scroll::plain_text;

    #[test]
    fn markdown_renderer_preserves_supported_block_text_and_blank_lines() {
        let markdown = "# Heading\n\n- bullet\n> quote\n\n```rust\nlet value = 1;\n```";
        let rendered = render_markdown(markdown, Style::default().fg(Color::Gray), &[]);

        assert_eq!(plain_text(&rendered), markdown);
        assert_eq!(
            rendered.lines[0].spans[0].style.add_modifier,
            Modifier::BOLD | Modifier::UNDERLINED
        );
        assert_eq!(
            rendered.lines[2].spans[0].style.add_modifier,
            Modifier::BOLD
        );
        assert_eq!(
            rendered.lines[3].spans[0].style.add_modifier,
            Modifier::ITALIC
        );
        assert_eq!(rendered.lines[6].spans[0].style.fg, Some(Color::Green));
    }

    #[test]
    fn markdown_renderer_keeps_highlights_aligned_with_multiline_source_offsets() {
        let markdown = "# Heading\nplain line";
        let rendered = render_markdown(
            markdown,
            Style::default().fg(Color::Gray),
            &[MarkdownHighlight { start: 10, end: 15 }],
        );

        assert_eq!(plain_text(&rendered), markdown);
        assert_eq!(rendered.lines[1].spans.len(), 2);
        assert_eq!(rendered.lines[1].spans[1].content.as_ref(), " line");
        assert_eq!(rendered.lines[1].spans[0].content.as_ref(), "plain");
        assert_eq!(rendered.lines[1].spans[0].style.bg, Some(Color::Yellow));
    }
}

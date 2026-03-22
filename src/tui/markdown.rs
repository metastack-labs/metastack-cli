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
    OrderedList,
    Blockquote,
    FenceDelimiter,
    Code,
    TableHeader,
    TableDelimiter,
    TableBody,
}

/// Render a narrow markdown subset into ratatui text while preserving source line breaks.
///
/// Unsupported markdown is rendered as plain paragraph text with the provided `base_style`.
pub(crate) fn render_markdown(
    markdown: &str,
    base_style: Style,
    highlights: &[MarkdownHighlight],
) -> Text<'static> {
    let raw_lines: Vec<&str> = markdown.split('\n').collect();
    let kinds = classify_all_lines(&raw_lines);

    let mut lines = Vec::new();
    let mut line_start = 0usize;

    for (i, raw_line) in raw_lines.iter().enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let kind = kinds[i];

        let rendered = match kind {
            MarkdownLineKind::Blank => Line::from(""),
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
            MarkdownLineKind::TableDelimiter => Line::from(highlighted_spans(
                line,
                line_start,
                highlights,
                table_delimiter_style(),
            )),
            _ => {
                let style = line_base_style(kind, base_style);
                Line::from(inline_highlighted_spans(
                    line, line_start, highlights, style,
                ))
            }
        };
        lines.push(rendered);
        line_start += raw_line.len() + 1;
    }

    Text::from(lines)
}

/// Resolve the base style for a line kind.
fn line_base_style(kind: MarkdownLineKind, base_style: Style) -> Style {
    match kind {
        MarkdownLineKind::Heading { level } => heading_style(level),
        MarkdownLineKind::Bullet => bullet_style(base_style),
        MarkdownLineKind::OrderedList => ordered_list_style(base_style),
        MarkdownLineKind::Blockquote => blockquote_style(),
        MarkdownLineKind::TableHeader => table_header_style(),
        _ => base_style,
    }
}

// ---------------------------------------------------------------------------
// Line classification
// ---------------------------------------------------------------------------

fn classify_all_lines(raw_lines: &[&str]) -> Vec<MarkdownLineKind> {
    let mut kinds = Vec::with_capacity(raw_lines.len());
    let mut in_fence = false;
    let mut in_table = false;

    for (i, raw_line) in raw_lines.iter().enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);

        // Inside a fenced code block, only check for the closing fence.
        if in_fence {
            if is_fence_delimiter(line) {
                kinds.push(MarkdownLineKind::FenceDelimiter);
                in_fence = false;
            } else {
                kinds.push(MarkdownLineKind::Code);
            }
            continue;
        }

        if is_fence_delimiter(line) {
            kinds.push(MarkdownLineKind::FenceDelimiter);
            in_fence = true;
            in_table = false;
            continue;
        }

        if line.trim().is_empty() {
            kinds.push(MarkdownLineKind::Blank);
            in_table = false;
            continue;
        }

        // Continue an existing table.
        if in_table {
            if is_table_row(line) {
                if is_table_delimiter(line) {
                    kinds.push(MarkdownLineKind::TableDelimiter);
                } else {
                    kinds.push(MarkdownLineKind::TableBody);
                }
                continue;
            }
            in_table = false;
        }

        // Detect table start: a pipe row followed by a delimiter row.
        if is_table_row(line) {
            if let Some(next) = raw_lines.get(i + 1) {
                let next_line = next.strip_suffix('\r').unwrap_or(next);
                if is_table_delimiter(next_line) {
                    kinds.push(MarkdownLineKind::TableHeader);
                    in_table = true;
                    continue;
                }
            }
        }

        if let Some(level) = heading_level(line) {
            kinds.push(MarkdownLineKind::Heading { level });
            continue;
        }

        if is_blockquote(line) {
            kinds.push(MarkdownLineKind::Blockquote);
            continue;
        }

        if is_bullet(line) {
            kinds.push(MarkdownLineKind::Bullet);
            continue;
        }

        if is_ordered_list(line) {
            kinds.push(MarkdownLineKind::OrderedList);
            continue;
        }

        kinds.push(MarkdownLineKind::Paragraph);
    }

    kinds
}

// ---------------------------------------------------------------------------
// Block-level detection helpers
// ---------------------------------------------------------------------------

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

fn is_ordered_list(line: &str) -> bool {
    let trimmed = line.trim_start();
    let digit_count = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    digit_count > 0
        && trimmed.as_bytes().get(digit_count) == Some(&b'.')
        && trimmed.as_bytes().get(digit_count + 1) == Some(&b' ')
}

fn is_fence_delimiter(line: &str) -> bool {
    line.trim_start().starts_with("```")
}

fn is_table_row(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('|') && trimmed.ends_with('|') && trimmed.len() > 1
}

fn is_table_delimiter(line: &str) -> bool {
    if !is_table_row(line) {
        return false;
    }
    let trimmed = line.trim();
    let inner = &trimmed[1..trimmed.len() - 1];
    if inner.is_empty() {
        return false;
    }
    inner.split('|').all(|cell| {
        let cell = cell.trim();
        !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ':')
    })
}

// ---------------------------------------------------------------------------
// Inline span parsing
// ---------------------------------------------------------------------------

struct InlineSegment {
    start: usize,
    end: usize,
    style: Style,
}

/// Parse inline code, bold, and italic markers within a line, returning styled segments
/// that cover the entire line without gaps. Markers are kept in the output text.
fn parse_inline_segments(line: &str, base_style: Style) -> Vec<InlineSegment> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut segments = Vec::new();
    let mut pos = 0;
    let mut plain_start = 0;

    while pos < len {
        // Backtick code span: highest priority, no nesting.
        if bytes[pos] == b'`' {
            if let Some(close) = find_closing_backtick(bytes, pos + 1) {
                if pos > plain_start {
                    segments.push(InlineSegment {
                        start: plain_start,
                        end: pos,
                        style: base_style,
                    });
                }
                segments.push(InlineSegment {
                    start: pos,
                    end: close + 1,
                    style: inline_code_style(base_style),
                });
                pos = close + 1;
                plain_start = pos;
                continue;
            }
        }

        // Bold: **text** — opening must not be followed by a space, closing not preceded.
        if pos + 2 < len && bytes[pos] == b'*' && bytes[pos + 1] == b'*' && bytes[pos + 2] != b' ' {
            if let Some(close) = find_closing_double_star(bytes, pos + 2) {
                if bytes[close - 1] != b' ' {
                    if pos > plain_start {
                        segments.push(InlineSegment {
                            start: plain_start,
                            end: pos,
                            style: base_style,
                        });
                    }
                    segments.push(InlineSegment {
                        start: pos,
                        end: close + 2,
                        style: bold_inline_style(base_style),
                    });
                    pos = close + 2;
                    plain_start = pos;
                    continue;
                }
            }
        }

        // Italic: *text* — single star, not adjacent to another star.
        if bytes[pos] == b'*' && pos + 1 < len && bytes[pos + 1] != b'*' && bytes[pos + 1] != b' ' {
            if let Some(close) = find_closing_single_star(bytes, pos + 1) {
                if bytes[close - 1] != b' ' {
                    if pos > plain_start {
                        segments.push(InlineSegment {
                            start: plain_start,
                            end: pos,
                            style: base_style,
                        });
                    }
                    segments.push(InlineSegment {
                        start: pos,
                        end: close + 1,
                        style: italic_inline_style(base_style),
                    });
                    pos = close + 1;
                    plain_start = pos;
                    continue;
                }
            }
        }

        pos += 1;
    }

    if plain_start < len {
        segments.push(InlineSegment {
            start: plain_start,
            end: len,
            style: base_style,
        });
    }

    // Guarantee at least one segment so callers always produce output.
    if segments.is_empty() {
        segments.push(InlineSegment {
            start: 0,
            end: len,
            style: base_style,
        });
    }

    segments
}

fn find_closing_backtick(bytes: &[u8], start: usize) -> Option<usize> {
    (start..bytes.len()).find(|&i| bytes[i] == b'`')
}

fn find_closing_double_star(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.len() < 2 {
        return None;
    }
    (start..bytes.len() - 1).find(|&i| bytes[i] == b'*' && bytes[i + 1] == b'*')
}

fn find_closing_single_star(bytes: &[u8], start: usize) -> Option<usize> {
    (start..bytes.len()).find(|&i| {
        bytes[i] == b'*'
            && !(i + 1 < bytes.len() && bytes[i + 1] == b'*')
            && (i == 0 || bytes[i - 1] != b'*')
    })
}

// ---------------------------------------------------------------------------
// Span builders
// ---------------------------------------------------------------------------

/// Build spans for a line that may contain inline formatting, overlaid with highlights.
fn inline_highlighted_spans(
    line: &str,
    line_start: usize,
    highlights: &[MarkdownHighlight],
    base_style: Style,
) -> Vec<Span<'static>> {
    let segments = parse_inline_segments(line, base_style);
    let line_end = line_start + line.len();

    let relevant: Vec<MarkdownHighlight> = highlights
        .iter()
        .filter_map(|h| {
            if h.end <= line_start || h.start >= line_end {
                return None;
            }
            Some(MarkdownHighlight {
                start: h.start.saturating_sub(line_start),
                end: h.end.min(line_end).saturating_sub(line_start),
            })
        })
        .collect();

    if relevant.is_empty() {
        return segments
            .into_iter()
            .filter(|s| s.start < s.end)
            .map(|s| Span::styled(line[s.start..s.end].to_string(), s.style))
            .collect();
    }

    let mut spans = Vec::new();
    for seg in &segments {
        if seg.start >= seg.end {
            continue;
        }
        let mut cursor = seg.start;
        for h in &relevant {
            if h.end <= seg.start || h.start >= seg.end {
                continue;
            }
            let hl_start = h.start.max(seg.start);
            let hl_end = h.end.min(seg.end);
            if cursor < hl_start {
                spans.push(Span::styled(line[cursor..hl_start].to_string(), seg.style));
            }
            spans.push(Span::styled(
                line[hl_start..hl_end].to_string(),
                seg.style.patch(highlight_style()),
            ));
            cursor = hl_end;
        }
        if cursor < seg.end {
            spans.push(Span::styled(line[cursor..seg.end].to_string(), seg.style));
        }
    }
    spans
}

/// Build spans for a line with a single base style, overlaid with highlights.
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

// ---------------------------------------------------------------------------
// Styles
// ---------------------------------------------------------------------------

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

fn ordered_list_style(base_style: Style) -> Style {
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

fn table_header_style() -> Style {
    tone_style(Tone::Accent).add_modifier(Modifier::BOLD)
}

fn table_delimiter_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn inline_code_style(base_style: Style) -> Style {
    base_style.fg(Color::Green)
}

fn bold_inline_style(base_style: Style) -> Style {
    base_style.add_modifier(Modifier::BOLD)
}

fn italic_inline_style(base_style: Style) -> Style {
    base_style.add_modifier(Modifier::ITALIC)
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

    #[test]
    fn ordered_list_items_render_with_distinct_styling() {
        let markdown = "1. first\n2. second\n3. third";
        let rendered = render_markdown(markdown, Style::default().fg(Color::Gray), &[]);

        assert_eq!(plain_text(&rendered), markdown);
        assert!(
            rendered.lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            rendered.lines[1].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            rendered.lines[2].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn nested_lists_preserve_hierarchy() {
        let markdown = "- top\n  - nested unordered\n  1. nested ordered\n    - deeply nested";
        let rendered = render_markdown(markdown, Style::default().fg(Color::Gray), &[]);

        assert_eq!(plain_text(&rendered), markdown);
        assert!(
            rendered.lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            rendered.lines[1].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            rendered.lines[2].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            rendered.lines[3].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn inline_code_spans_render_with_code_styling() {
        let markdown = "Use `foo()` here";
        let rendered = render_markdown(markdown, Style::default().fg(Color::Gray), &[]);

        assert_eq!(plain_text(&rendered), markdown);
        assert_eq!(rendered.lines[0].spans.len(), 3);
        assert_eq!(rendered.lines[0].spans[0].content.as_ref(), "Use ");
        assert_eq!(rendered.lines[0].spans[1].content.as_ref(), "`foo()`");
        assert_eq!(rendered.lines[0].spans[1].style.fg, Some(Color::Green));
        assert_eq!(rendered.lines[0].spans[2].content.as_ref(), " here");
    }

    #[test]
    fn bold_spans_render_with_bold_modifier() {
        let markdown = "Some **bold** text";
        let rendered = render_markdown(markdown, Style::default().fg(Color::Gray), &[]);

        assert_eq!(plain_text(&rendered), markdown);
        assert_eq!(rendered.lines[0].spans.len(), 3);
        assert_eq!(rendered.lines[0].spans[1].content.as_ref(), "**bold**");
        assert!(
            rendered.lines[0].spans[1]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn italic_spans_render_with_italic_modifier() {
        let markdown = "Some *italic* text";
        let rendered = render_markdown(markdown, Style::default().fg(Color::Gray), &[]);

        assert_eq!(plain_text(&rendered), markdown);
        assert_eq!(rendered.lines[0].spans.len(), 3);
        assert_eq!(rendered.lines[0].spans[1].content.as_ref(), "*italic*");
        assert!(
            rendered.lines[0].spans[1]
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );
    }

    #[test]
    fn simple_pipe_table_renders_with_table_styling() {
        let markdown = "| Name | Value |\n| ---- | ----- |\n| foo  | 42    |";
        let rendered = render_markdown(markdown, Style::default().fg(Color::Gray), &[]);

        assert_eq!(plain_text(&rendered), markdown);
        // Header row should be styled with accent+bold.
        assert!(
            rendered.lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        // Delimiter row should be muted.
        assert_eq!(rendered.lines[1].spans[0].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn mixed_content_fixture_renders_all_supported_forms() {
        let markdown = concat!(
            "# Title\n",
            "\n",
            "Some paragraph with **bold** and *italic* and `code`.\n",
            "\n",
            "- unordered item\n",
            "  - nested unordered\n",
            "1. ordered item\n",
            "  1. nested ordered\n",
            "\n",
            "> a blockquote\n",
            "\n",
            "```bash\n",
            "echo hello\n",
            "```\n",
            "\n",
            "| Col A | Col B |\n",
            "| ----- | ----- |\n",
            "| x     | y     |\n",
            "\n",
            "Unsupported: ~~strikethrough~~ and [link](url)",
        );
        let rendered = render_markdown(markdown, Style::default().fg(Color::Gray), &[]);

        assert_eq!(plain_text(&rendered), markdown);

        // Line 0: heading
        assert!(
            rendered.lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );

        // Line 2: paragraph with inline formatting
        let para = &rendered.lines[2];
        let bold_span = para
            .spans
            .iter()
            .find(|s| s.content.contains("**bold**"))
            .expect("bold span present");
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));

        let italic_span = para
            .spans
            .iter()
            .find(|s| s.content.contains("*italic*"))
            .expect("italic span present");
        assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));

        let code_span = para
            .spans
            .iter()
            .find(|s| s.content.contains("`code`"))
            .expect("code span present");
        assert_eq!(code_span.style.fg, Some(Color::Green));

        // Lines 4-5: unordered bullets (including nested)
        assert!(
            rendered.lines[4].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            rendered.lines[5].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );

        // Lines 6-7: ordered list (including nested)
        assert!(
            rendered.lines[6].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            rendered.lines[7].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );

        // Line 9: blockquote
        assert!(
            rendered.lines[9].spans[0]
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );

        // Line 12: fenced code body
        assert_eq!(rendered.lines[12].spans[0].style.fg, Some(Color::Green));

        // Lines 15-17: table
        assert!(
            rendered.lines[15].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(rendered.lines[16].spans[0].style.fg, Some(Color::DarkGray));

        // Line 19: unsupported syntax renders as plain text
        let unsupported = &rendered.lines[19];
        assert_eq!(
            unsupported
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>(),
            "Unsupported: ~~strikethrough~~ and [link](url)"
        );
    }

    #[test]
    fn unsupported_syntax_degrades_to_plain_text() {
        let markdown = "~~strike~~ [link](url) ![img](src) _underscore_ __double__";
        let rendered = render_markdown(markdown, Style::default().fg(Color::Gray), &[]);

        assert_eq!(plain_text(&rendered), markdown);
        // All text preserved, no panics, no dropped content.
        assert!(!rendered.lines[0].spans.is_empty());
    }
}

use std::env;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

/// Semantic terminal tones shared across MetaStack dashboards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Accent,
    Info,
    Success,
    Danger,
    Muted,
}

pub(crate) const LIST_HIGHLIGHT_SYMBOL: &str = "> ";

/// Returns whether the current terminal likely supports richer RGB output.
pub(crate) fn supports_rich_color() -> bool {
    if env::var_os("NO_COLOR").is_some() {
        return false;
    }

    let term = env::var("TERM").unwrap_or_default();
    if term.eq_ignore_ascii_case("dumb") {
        return false;
    }

    let color_term = env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();
    color_term.contains("truecolor") || color_term.contains("24bit")
}

/// Shared panel block with consistent border styling.
pub(crate) fn panel(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(border_style())
        .title(title.into())
}

/// Shared title builder with an optional focus indicator.
pub(crate) fn panel_title(title: impl Into<String>, focused: bool) -> Line<'static> {
    let title = title.into();
    if focused {
        Line::from(vec![
            Span::styled(title, emphasis_style()),
            Span::raw(" "),
            badge("focus", Tone::Accent),
        ])
    } else {
        Line::from(Span::styled(title, emphasis_style()))
    }
}

/// Shared list widget styling with a consistent selection treatment.
pub(crate) fn list(
    items: Vec<ListItem<'static>>,
    title: impl Into<Line<'static>>,
) -> List<'static> {
    List::new(items)
        .block(panel(title))
        .highlight_style(list_highlight_style())
        .highlight_symbol(LIST_HIGHLIGHT_SYMBOL)
}

/// Shared paragraph widget styling with wrapping enabled.
pub(crate) fn paragraph(
    text: impl Into<Text<'static>>,
    title: impl Into<Line<'static>>,
) -> Paragraph<'static> {
    Paragraph::new(text.into())
        .wrap(Wrap { trim: true })
        .block(panel(title))
}

/// Shared empty-state copy.
pub(crate) fn empty_state(message: impl Into<String>, hint: impl Into<String>) -> Text<'static> {
    Text::from(vec![
        Line::from(Span::styled(message.into(), muted_style())),
        Line::from(""),
        Line::from(hint.into()),
    ])
}

/// Shared key hint placement and styling for dashboard headers.
pub(crate) fn key_hints(hints: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::styled("Keys: ", label_style())];
    for (index, (key, description)) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" • ", muted_style()));
        }
        spans.push(Span::styled(format!("{key} "), tone_style(Tone::Accent)));
        spans.push(Span::styled(description.to_string(), muted_style()));
    }
    Line::from(spans)
}

/// Shared badge styling for status labels.
pub(crate) fn badge(label: impl Into<String>, tone: Tone) -> Span<'static> {
    Span::styled(
        format!("[{}]", label.into()),
        tone_style(tone).add_modifier(Modifier::BOLD),
    )
}

/// Shared emphasis style for titles and active copy.
pub(crate) fn emphasis_style() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

/// Shared muted style for supporting copy.
pub(crate) fn muted_style() -> Style {
    Style::default().fg(Color::Gray)
}

/// Shared label style for metadata labels.
pub(crate) fn label_style() -> Style {
    muted_style().add_modifier(Modifier::BOLD)
}

/// Shared list selection style.
pub(crate) fn list_highlight_style() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

/// Shared border styling across panels.
pub(crate) fn border_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

/// Shared semantic style lookup.
pub(crate) fn tone_style(tone: Tone) -> Style {
    let color = match tone {
        Tone::Accent => accent_color(),
        Tone::Info => Color::Cyan,
        Tone::Success => Color::Green,
        Tone::Danger => Color::Red,
        Tone::Muted => Color::Gray,
    };
    Style::default().fg(color)
}

fn accent_color() -> Color {
    if supports_rich_color() {
        Color::Rgb(111, 214, 255)
    } else {
        Color::Cyan
    }
}

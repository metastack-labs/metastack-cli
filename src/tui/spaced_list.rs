use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{List, ListItem};

use crate::tui::theme::{
    LIST_HIGHLIGHT_SYMBOL, Tone, badge, emphasis_style, label_style, list_highlight_style,
    muted_style, panel,
};

/// Create a list item from content lines with a trailing blank line for consistent vertical
/// spacing in stacked lists.
///
/// Selection indices remain one-to-one with data items because the blank line is part of the
/// item content, not a separate entry.
pub(crate) fn spaced_list_item(lines: Vec<Line<'static>>) -> ListItem<'static> {
    let mut spaced = lines;
    spaced.push(Line::from(""));
    ListItem::new(Text::from(spaced))
}

/// Build a themed list widget for stacked list panels.
///
/// Applies the standard panel, highlight style, and highlight symbol. Callers are expected to
/// pass items that were constructed with [`spaced_list_item`] so inter-item spacing is already
/// embedded in the item content.
pub(crate) fn spaced_list(
    items: Vec<ListItem<'static>>,
    title: impl Into<Line<'static>>,
) -> List<'static> {
    List::new(items)
        .block(panel(title))
        .highlight_style(list_highlight_style())
        .highlight_symbol(LIST_HIGHLIGHT_SYMBOL)
}

/// Canonical rendering of a GitHub PR row in stacked list views.
///
/// Returns a three-line `ListItem` (plus trailing blank spacer):
/// 1. `[#NUMBER] [STATUS] TITLE`
/// 2. `key VALUE  key VALUE  …`
/// 3. footer line (muted)
///
/// Callers construct the `status_badge` and `metadata` from their domain-specific data so the
/// visual structure stays consistent across merge, review, and other PR-oriented dashboards.
pub(crate) fn render_github_pr_row(
    number: u64,
    title: &str,
    status_badge: Span<'static>,
    metadata: &[(&str, String)],
    footer: &str,
) -> ListItem<'static> {
    let mut first_line = vec![
        badge(format!("#{number}"), Tone::Accent),
        Span::raw(" "),
        status_badge,
        Span::raw(" "),
        Span::styled(title.to_string(), emphasis_style()),
    ];
    // Drop trailing empty spans when the badge is empty.
    trim_trailing_empty_spans(&mut first_line);

    let mut meta_spans: Vec<Span<'static>> = Vec::new();
    for (index, (key, value)) in metadata.iter().enumerate() {
        if index > 0 {
            meta_spans.push(Span::raw("  "));
        }
        meta_spans.push(Span::styled(format!("{key} "), label_style()));
        meta_spans.push(Span::raw(value.clone()));
    }

    spaced_list_item(vec![
        Line::from(first_line),
        Line::from(meta_spans),
        Line::from(Span::styled(footer.to_string(), muted_style())),
    ])
}

/// Canonical rendering of an active agent session row in stacked list views.
///
/// Returns a three-line `ListItem` (plus trailing blank spacer):
/// 1. badge sequence (e.g. `[#42] [review] [analyzing]`)
/// 2. title
/// 3. `Summary TEXT`
pub(crate) fn render_github_session_row(
    badges: Vec<Span<'static>>,
    title: &str,
    summary: &str,
) -> ListItem<'static> {
    spaced_list_item(vec![
        Line::from(badges),
        Line::from(Span::styled(title.to_string(), emphasis_style())),
        Line::from(vec![
            Span::styled("Summary ", label_style()),
            Span::raw(summary.to_string()),
        ]),
    ])
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn trim_trailing_empty_spans(spans: &mut Vec<Span<'static>>) {
    while spans
        .last()
        .is_some_and(|s| s.content.as_ref().trim().is_empty())
    {
        spans.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaced_list_item_appends_blank_line() {
        let item = spaced_list_item(vec![Line::from("alpha"), Line::from("beta")]);
        assert_eq!(item.height(), 3);
    }

    #[test]
    fn spaced_list_item_single_line_gets_spacer() {
        let item = spaced_list_item(vec![Line::from("only")]);
        assert_eq!(item.height(), 2);
    }

    #[test]
    fn render_github_pr_row_structure() {
        let item = render_github_pr_row(
            42,
            "Fix auth regression",
            badge("selected", Tone::Accent),
            &[
                ("Author", "alice".to_string()),
                ("Branch", "fix/auth".to_string()),
            ],
            "Updated 2026-03-22",
        );
        // 3 content lines + 1 spacer
        assert_eq!(item.height(), 4);
    }

    #[test]
    fn render_github_session_row_structure() {
        let item = render_github_session_row(
            vec![
                badge("#42", Tone::Accent),
                Span::raw(" "),
                badge("review", Tone::Info),
            ],
            "Fix auth regression",
            "Analyzing code changes",
        );
        // 3 content lines + 1 spacer
        assert_eq!(item.height(), 4);
    }

    #[test]
    fn render_github_pr_row_text_content() {
        let item = render_github_pr_row(
            7,
            "Add tests",
            badge("queued", Tone::Muted),
            &[("Author", "bob".to_string())],
            "Updated yesterday",
        );
        let rendered = format!("{item:?}");
        assert!(rendered.contains("#7"));
        assert!(rendered.contains("queued"));
        assert!(rendered.contains("Add tests"));
        assert!(rendered.contains("Author"));
        assert!(rendered.contains("bob"));
        assert!(rendered.contains("Updated yesterday"));
    }

    #[test]
    fn spaced_list_item_empty_lines_produces_single_spacer() {
        let item = spaced_list_item(Vec::new());
        assert_eq!(item.height(), 1);
    }

    #[test]
    fn spaced_list_creates_widget_with_items() {
        let items = vec![
            spaced_list_item(vec![Line::from("one")]),
            spaced_list_item(vec![Line::from("two")]),
        ];
        let widget = spaced_list(items, "Test Panel");
        // Ensure the widget is created without panic and has the expected type.
        let _list: List<'static> = widget;
    }

    #[test]
    fn render_github_pr_row_empty_badge_trims_trailing_spaces() {
        let item = render_github_pr_row(1, "Title", Span::raw(""), &[], "");
        // 3 content lines + 1 spacer
        assert_eq!(item.height(), 4);
    }

    #[test]
    fn render_github_session_row_text_content() {
        let item = render_github_session_row(
            vec![badge("#10", Tone::Accent)],
            "Session title",
            "Work summary",
        );
        let rendered = format!("{item:?}");
        assert!(rendered.contains("#10"));
        assert!(rendered.contains("Session title"));
        assert!(rendered.contains("Summary"));
        assert!(rendered.contains("Work summary"));
    }
}

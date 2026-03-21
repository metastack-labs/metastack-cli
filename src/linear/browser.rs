use std::cmp::Ordering;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::ListItem;

use crate::linear::IssueSummary;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IssueSearchResult {
    pub(crate) issue_index: usize,
    highlights: IssueFieldHighlights,
    score: IssueSearchScore,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct IssueFieldHighlights {
    identifier: Vec<HighlightRange>,
    title: Vec<HighlightRange>,
    state: Vec<HighlightRange>,
    project: Vec<HighlightRange>,
    description: Vec<HighlightRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HighlightRange {
    start: usize,
    end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct IssueSearchScore {
    exact_identifier_full: bool,
    identifier_prefix_full: bool,
    exact_field_full: bool,
    substring_full: bool,
    identifier_exact_terms: usize,
    identifier_prefix_terms: usize,
    exact_token_terms: usize,
    substring_terms: usize,
    best_field_priority_sum: usize,
    best_match_position_sum: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TermMatchTier {
    IdentifierExact,
    IdentifierPrefix,
    ExactToken,
    Substring,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchField {
    Identifier,
    Title,
    State,
    Project,
    Description,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TermMatch {
    tier: TermMatchTier,
    field: SearchField,
    position: usize,
}

#[derive(Debug, Clone)]
struct SearchFieldValue<'a> {
    kind: SearchField,
    value: &'a str,
    normalized: String,
}

/// Search issues using a deterministic case-insensitive ranking model.
///
/// Returns ranked results with per-field highlight ranges for the query terms.
pub(crate) fn search_issues(issues: &[IssueSummary], query: &str) -> Vec<IssueSearchResult> {
    let prepared = PreparedQuery::new(query);
    if prepared.is_empty() {
        return issues
            .iter()
            .enumerate()
            .map(|(issue_index, _)| IssueSearchResult {
                issue_index,
                highlights: IssueFieldHighlights::default(),
                score: IssueSearchScore::default(),
            })
            .collect();
    }

    let mut matches = issues
        .iter()
        .enumerate()
        .filter_map(|(issue_index, issue)| {
            score_issue(issue, &prepared).map(|(score, highlights)| IssueSearchResult {
                issue_index,
                highlights,
                score,
            })
        })
        .collect::<Vec<_>>();

    matches.sort_by(|left, right| compare_scores(&left.score, &right.score));
    matches
}

/// Create an empty search result for callers that need shared rendering without active highlights.
pub(crate) fn empty_search_result(issue_index: usize) -> IssueSearchResult {
    IssueSearchResult {
        issue_index,
        highlights: IssueFieldHighlights::default(),
        score: IssueSearchScore::default(),
    }
}

/// Render a shared issue row with consistent semantic styling.
pub(crate) fn render_issue_row(
    issue: &IssueSummary,
    result: Option<&IssueSearchResult>,
    sync_status: Option<&str>,
) -> ListItem<'static> {
    let identifier = highlighted_spans(
        &issue.identifier,
        result.map_or(&[], |value| value.highlights.identifier.as_slice()),
        identifier_style(),
    );
    let title = highlighted_spans(
        &issue.title,
        result.map_or(&[], |value| value.highlights.title.as_slice()),
        title_style(),
    );

    let mut first_line = Vec::new();
    first_line.extend(identifier);
    first_line.push(Span::raw("  "));
    first_line.extend(title);

    let state = issue_state_label(issue);
    let project = issue_project_label(issue);
    let mut detail_line = Vec::new();
    detail_line.extend(label_value_spans(
        "state",
        highlighted_spans(
            &state,
            result.map_or(&[], |value| value.highlights.state.as_slice()),
            state_style(issue),
        ),
    ));
    detail_line.push(Span::raw("  "));
    detail_line.extend(label_value_spans(
        "priority",
        vec![Span::styled(priority_label(issue), priority_style(issue))],
    ));
    detail_line.push(Span::raw("  "));
    detail_line.extend(label_value_spans(
        "project",
        highlighted_spans(
            &project,
            result.map_or(&[], |value| value.highlights.project.as_slice()),
            project_style(),
        ),
    ));

    if let Some(sync_status) = sync_status {
        detail_line.push(Span::raw("  "));
        detail_line.extend(label_value_spans(
            "sync",
            vec![Span::styled(
                sync_status.to_string(),
                sync_status_style(sync_status),
            )],
        ));
    }

    ListItem::new(Text::from(vec![
        Line::from(first_line),
        Line::from(detail_line),
    ]))
}

/// Render a shared issue preview with consistent semantic styling.
pub(crate) fn render_issue_preview(
    issue: &IssueSummary,
    result: Option<&IssueSearchResult>,
    sync_status: Option<&str>,
    empty_description: &str,
) -> Text<'static> {
    let state = issue_state_label(issue);
    let project = issue_project_label(issue);
    let description = issue
        .description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(empty_description);

    let mut lines = vec![
        Line::from(vec![
            styled_label("issue"),
            Span::raw(" "),
            Span::styled(issue.identifier.clone(), identifier_style()),
        ]),
        Line::from(vec![
            styled_label("title"),
            Span::raw(" "),
            Span::styled(issue.title.clone(), title_style()),
        ]),
        Line::from(vec![
            styled_label("state"),
            Span::raw(" "),
            Span::styled(state, state_style(issue)),
        ]),
        Line::from(vec![
            styled_label("priority"),
            Span::raw(" "),
            Span::styled(priority_label(issue), priority_style(issue)),
        ]),
        Line::from(vec![
            styled_label("project"),
            Span::raw(" "),
            Span::styled(project, project_style()),
        ]),
    ];

    if let Some(sync_status) = sync_status {
        lines.push(Line::from(vec![
            styled_label("sync"),
            Span::raw(" "),
            Span::styled(sync_status.to_string(), sync_status_style(sync_status)),
        ]));
    }

    lines.extend([
        Line::from(vec![
            styled_label("updated"),
            Span::raw(" "),
            Span::styled(issue.updated_at.clone(), metadata_value_style()),
        ]),
        Line::from(vec![
            styled_label("url"),
            Span::raw(" "),
            Span::styled(issue.url.clone(), metadata_value_style()),
        ]),
        Line::from(""),
        Line::from(vec![styled_label("description")]),
    ]);

    lines.push(Line::from(highlighted_spans(
        description,
        result.map_or(&[], |value| value.highlights.description.as_slice()),
        description_style(),
    )));

    Text::from(lines)
}

/// Return the normalized state label for an issue.
pub(crate) fn issue_state_label(issue: &IssueSummary) -> String {
    issue
        .state
        .as_ref()
        .map(|state| state.name.clone())
        .unwrap_or_else(|| "Unknown".to_string())
}

/// Return the normalized project label for an issue.
pub(crate) fn issue_project_label(issue: &IssueSummary) -> String {
    issue
        .project
        .as_ref()
        .map(|project| project.name.clone())
        .unwrap_or_else(|| "No project".to_string())
}

fn compare_scores(left: &IssueSearchScore, right: &IssueSearchScore) -> Ordering {
    right
        .exact_identifier_full
        .cmp(&left.exact_identifier_full)
        .then_with(|| {
            right
                .identifier_prefix_full
                .cmp(&left.identifier_prefix_full)
        })
        .then_with(|| right.exact_field_full.cmp(&left.exact_field_full))
        .then_with(|| right.substring_full.cmp(&left.substring_full))
        .then_with(|| {
            right
                .identifier_exact_terms
                .cmp(&left.identifier_exact_terms)
        })
        .then_with(|| {
            right
                .identifier_prefix_terms
                .cmp(&left.identifier_prefix_terms)
        })
        .then_with(|| right.exact_token_terms.cmp(&left.exact_token_terms))
        .then_with(|| left.substring_terms.cmp(&right.substring_terms))
        .then_with(|| {
            left.best_field_priority_sum
                .cmp(&right.best_field_priority_sum)
        })
        .then_with(|| {
            left.best_match_position_sum
                .cmp(&right.best_match_position_sum)
        })
}

fn score_issue(
    issue: &IssueSummary,
    query: &PreparedQuery,
) -> Option<(IssueSearchScore, IssueFieldHighlights)> {
    let fields = issue_fields(issue);
    let mut score = score_full_query(&fields, query.full.as_str());
    let mut highlights = IssueFieldHighlights::default();

    for term in &query.terms {
        let best_match = best_term_match(&fields, term)?;
        match best_match.tier {
            TermMatchTier::IdentifierExact => score.identifier_exact_terms += 1,
            TermMatchTier::IdentifierPrefix => score.identifier_prefix_terms += 1,
            TermMatchTier::ExactToken => score.exact_token_terms += 1,
            TermMatchTier::Substring => score.substring_terms += 1,
        }
        score.best_field_priority_sum += field_priority(best_match.field);
        score.best_match_position_sum += best_match.position;
    }

    for term in &query.terms {
        apply_highlights(&mut highlights, &fields, term);
    }

    Some((score, highlights))
}

fn score_full_query(fields: &[SearchFieldValue<'_>], query: &str) -> IssueSearchScore {
    let identifier = fields
        .iter()
        .find(|field| field.kind == SearchField::Identifier)
        .map(|field| field.normalized.as_str())
        .unwrap_or("");
    let exact_field_full = fields.iter().any(|field| {
        field.normalized == query
            || tokenize_with_positions(field.value)
                .iter()
                .any(|token| token.0 == query)
    });
    let substring_full = fields.iter().any(|field| field.normalized.contains(query));

    IssueSearchScore {
        exact_identifier_full: identifier == query,
        identifier_prefix_full: !query.is_empty() && identifier.starts_with(query),
        exact_field_full,
        substring_full,
        ..IssueSearchScore::default()
    }
}

fn best_term_match(fields: &[SearchFieldValue<'_>], term: &str) -> Option<TermMatch> {
    let identifier = fields
        .iter()
        .find(|field| field.kind == SearchField::Identifier)?;
    if identifier.normalized == term {
        return Some(TermMatch {
            tier: TermMatchTier::IdentifierExact,
            field: SearchField::Identifier,
            position: 0,
        });
    }
    if identifier.normalized.starts_with(term) {
        return Some(TermMatch {
            tier: TermMatchTier::IdentifierPrefix,
            field: SearchField::Identifier,
            position: 0,
        });
    }

    for field in fields {
        if field.normalized == term {
            return Some(TermMatch {
                tier: TermMatchTier::ExactToken,
                field: field.kind,
                position: 0,
            });
        }

        if let Some(position) = tokenize_with_positions(field.value)
            .into_iter()
            .find_map(|(token, start, _)| (token == term).then_some(start))
        {
            return Some(TermMatch {
                tier: TermMatchTier::ExactToken,
                field: field.kind,
                position,
            });
        }
    }

    fields.iter().find_map(|field| {
        field.normalized.find(term).map(|position| TermMatch {
            tier: TermMatchTier::Substring,
            field: field.kind,
            position,
        })
    })
}

fn apply_highlights(
    highlights: &mut IssueFieldHighlights,
    fields: &[SearchFieldValue<'_>],
    term: &str,
) {
    for field in fields {
        let ranges = find_match_ranges(field.value, term);
        if ranges.is_empty() {
            continue;
        }
        match field.kind {
            SearchField::Identifier => merge_ranges(&mut highlights.identifier, &ranges),
            SearchField::Title => merge_ranges(&mut highlights.title, &ranges),
            SearchField::State => merge_ranges(&mut highlights.state, &ranges),
            SearchField::Project => merge_ranges(&mut highlights.project, &ranges),
            SearchField::Description => merge_ranges(&mut highlights.description, &ranges),
        }
    }
}

fn merge_ranges(target: &mut Vec<HighlightRange>, ranges: &[HighlightRange]) {
    target.extend_from_slice(ranges);
    target.sort_by_key(|range| range.start);
    let mut merged: Vec<HighlightRange> = Vec::with_capacity(target.len());
    for range in target.drain(..) {
        if let Some(last) = merged.last_mut()
            && range.start <= last.end
        {
            last.end = last.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    *target = merged;
}

fn find_match_ranges(value: &str, term: &str) -> Vec<HighlightRange> {
    if term.is_empty() {
        return Vec::new();
    }

    let haystack = value.to_ascii_lowercase();
    let mut start = 0usize;
    let mut ranges = Vec::new();
    while let Some(offset) = haystack[start..].find(term) {
        let range_start = start + offset;
        let range_end = range_start + term.len();
        ranges.push(HighlightRange {
            start: range_start,
            end: range_end,
        });
        start = range_end;
    }
    ranges
}

fn issue_fields(issue: &IssueSummary) -> [SearchFieldValue<'_>; 5] {
    [
        SearchFieldValue {
            kind: SearchField::Identifier,
            value: &issue.identifier,
            normalized: issue.identifier.to_ascii_lowercase(),
        },
        SearchFieldValue {
            kind: SearchField::Title,
            value: &issue.title,
            normalized: issue.title.to_ascii_lowercase(),
        },
        SearchFieldValue {
            kind: SearchField::State,
            value: issue
                .state
                .as_ref()
                .map(|state| state.name.as_str())
                .unwrap_or("Unknown"),
            normalized: issue_state_label(issue).to_ascii_lowercase(),
        },
        SearchFieldValue {
            kind: SearchField::Project,
            value: issue
                .project
                .as_ref()
                .map(|project| project.name.as_str())
                .unwrap_or("No project"),
            normalized: issue_project_label(issue).to_ascii_lowercase(),
        },
        SearchFieldValue {
            kind: SearchField::Description,
            value: issue.description.as_deref().unwrap_or(""),
            normalized: issue
                .description
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase(),
        },
    ]
}

fn field_priority(field: SearchField) -> usize {
    match field {
        SearchField::Identifier => 0,
        SearchField::Title => 1,
        SearchField::State => 2,
        SearchField::Project => 3,
        SearchField::Description => 4,
    }
}

fn tokenize_with_positions(value: &str) -> Vec<(String, usize, usize)> {
    let mut tokens = Vec::new();
    let mut start = None::<usize>;

    for (index, character) in value.char_indices() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            start.get_or_insert(index);
            continue;
        }

        if let Some(token_start) = start.take() {
            tokens.push((
                value[token_start..index].to_ascii_lowercase(),
                token_start,
                index,
            ));
        }
    }

    if let Some(token_start) = start {
        tokens.push((
            value[token_start..].to_ascii_lowercase(),
            token_start,
            value.len(),
        ));
    }

    tokens
}

fn highlighted_spans(
    value: &str,
    highlights: &[HighlightRange],
    base_style: Style,
) -> Vec<Span<'static>> {
    if highlights.is_empty() {
        return vec![Span::styled(value.to_string(), base_style)];
    }

    let mut spans = Vec::new();
    let mut cursor = 0usize;
    for range in highlights {
        if cursor < range.start {
            spans.push(Span::styled(
                value[cursor..range.start].to_string(),
                base_style,
            ));
        }
        spans.push(Span::styled(
            value[range.start..range.end].to_string(),
            base_style.patch(highlight_style()),
        ));
        cursor = range.end;
    }
    if cursor < value.len() {
        spans.push(Span::styled(value[cursor..].to_string(), base_style));
    }
    spans
}

fn label_value_spans(label: &str, value: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut spans = vec![styled_label(label), Span::raw(" ")];
    spans.extend(value);
    spans
}

fn styled_label(label: &str) -> Span<'static> {
    Span::styled(format!("{label}:"), label_style())
}

fn label_style() -> Style {
    Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

fn identifier_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn title_style() -> Style {
    Style::default()
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

fn description_style() -> Style {
    Style::default().fg(Color::Gray)
}

fn metadata_value_style() -> Style {
    Style::default().fg(Color::Gray)
}

fn project_style() -> Style {
    Style::default().fg(Color::Magenta)
}

fn state_style(issue: &IssueSummary) -> Style {
    let normalized = issue_state_label(issue).to_ascii_lowercase();
    let color = if normalized.contains("done")
        || normalized.contains("complete")
        || normalized.contains("closed")
    {
        Color::Green
    } else if normalized.contains("cancel") || normalized.contains("blocked") {
        Color::Red
    } else if normalized.contains("progress") || normalized.contains("review") {
        Color::Yellow
    } else {
        Color::Blue
    };

    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn priority_style(issue: &IssueSummary) -> Style {
    match issue.priority {
        Some(1) => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        Some(2) => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Some(3) => Style::default().fg(Color::Blue),
        Some(4) => Style::default().fg(Color::Gray),
        _ => Style::default().fg(Color::DarkGray),
    }
}

fn priority_label(issue: &IssueSummary) -> String {
    match issue.priority {
        Some(1) => "Urgent (1)".to_string(),
        Some(2) => "High (2)".to_string(),
        Some(3) => "Normal (3)".to_string(),
        Some(4) => "Low (4)".to_string(),
        _ => "None".to_string(),
    }
}

fn sync_status_style(sync_status: &str) -> Style {
    match sync_status {
        "synced" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "diverged" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "local-ahead" | "remote-ahead" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::DarkGray),
    }
}

fn highlight_style() -> Style {
    Style::default()
        .bg(Color::Rgb(70, 52, 18))
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

#[derive(Debug, Clone)]
struct PreparedQuery {
    full: String,
    terms: Vec<String>,
}

impl PreparedQuery {
    fn new(query: &str) -> Self {
        let full = query.trim().to_ascii_lowercase();
        let mut terms = full
            .split_whitespace()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        terms.dedup();
        Self { full, terms }
    }

    fn is_empty(&self) -> bool {
        self.full.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IssueSearchResult, compare_scores, render_issue_preview, render_issue_row, search_issues,
    };
    use crate::linear::{IssueSummary, ProjectRef, TeamRef, WorkflowState};
    use std::cmp::Ordering;

    fn issue(
        identifier: &str,
        title: &str,
        description: &str,
        state: &str,
        project: &str,
    ) -> IssueSummary {
        IssueSummary {
            id: format!("id-{identifier}"),
            identifier: identifier.to_string(),
            title: title.to_string(),
            description: Some(description.to_string()),
            url: format!("https://linear.app/{identifier}"),
            priority: Some(2),
            estimate: Some(3.0),
            updated_at: "2026-03-18T10:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "MET".to_string(),
                name: "Metastack".to_string(),
            },
            project: Some(ProjectRef {
                id: "project-1".to_string(),
                name: project.to_string(),
            }),
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: Some(WorkflowState {
                id: "state-1".to_string(),
                name: state.to_string(),
                kind: Some("started".to_string()),
            }),
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        }
    }

    fn identifiers(results: &[IssueSearchResult], issues: &[IssueSummary]) -> Vec<String> {
        results
            .iter()
            .map(|result| issues[result.issue_index].identifier.clone())
            .collect()
    }

    #[test]
    fn search_prefers_exact_identifier_then_prefix_then_token_then_substring() {
        let issues = vec![
            issue(
                "MET-7",
                "Shared browser foundation",
                "Search browser work",
                "Todo",
                "CLI",
            ),
            issue(
                "MET-70",
                "Row polish",
                "Shared browser follow-up",
                "Todo",
                "CLI",
            ),
            issue(
                "ENG-7",
                "MET-7 migration",
                "Adopt shared browser",
                "Todo",
                "CLI",
            ),
            issue(
                "ENG-9",
                "Browser notes",
                "Reference met-7 behavior",
                "Todo",
                "CLI",
            ),
        ];

        let results = search_issues(&issues, "met-7");
        assert_eq!(
            identifiers(&results, &issues),
            vec!["MET-7", "MET-70", "ENG-7", "ENG-9"]
        );
    }

    #[test]
    fn search_matches_across_state_project_and_description() {
        let issues = vec![
            issue(
                "MET-1",
                "Alpha",
                "Background sync browser",
                "Todo",
                "Control Plane",
            ),
            issue("MET-2", "Beta", "Other work", "In Review", "MetaStack CLI"),
            issue("MET-3", "Gamma", "Other work", "Todo", "Shared Browser"),
        ];

        assert_eq!(
            identifiers(&search_issues(&issues, "review"), &issues),
            vec!["MET-2"]
        );
        assert_eq!(
            identifiers(&search_issues(&issues, "browser"), &issues),
            vec!["MET-3", "MET-1"]
        );
    }

    #[test]
    fn search_returns_no_matches_for_unmatched_query() {
        let issues = vec![issue(
            "MET-1",
            "Alpha",
            "Background sync browser",
            "Todo",
            "CLI",
        )];
        assert!(search_issues(&issues, "zzz").is_empty());
    }

    #[test]
    fn row_and_preview_render_shared_metadata() {
        let issue = issue(
            "MET-42",
            "Searchable browser",
            "Add shared search highlighting to the dashboard.",
            "In Progress",
            "MetaStack CLI",
        );
        let result = search_issues(std::slice::from_ref(&issue), "searchable")
            .into_iter()
            .next()
            .expect("issue should match");

        let row = render_issue_row(&issue, Some(&result), Some("diverged"));
        let preview =
            render_issue_preview(&issue, Some(&result), Some("diverged"), "No description");

        assert!(format!("{row:?}").contains("MET-42"));
        assert!(format!("{preview:?}").contains("description"));
        assert!(format!("{preview:?}").contains("diverged"));
    }

    #[test]
    fn compare_scores_prefers_more_precise_matches() {
        let issues = vec![
            issue(
                "MET-42",
                "Searchable browser",
                "shared browser",
                "Todo",
                "CLI",
            ),
            issue(
                "ENG-42",
                "Shared browser",
                "met-42 reference",
                "Todo",
                "CLI",
            ),
        ];
        let results = search_issues(&issues, "met-42");
        assert_eq!(issues[results[0].issue_index].identifier, "MET-42");
        assert_eq!(
            compare_scores(&results[0].score, &results[1].score),
            Ordering::Less
        );
    }
}

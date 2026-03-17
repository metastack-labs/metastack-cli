use std::fmt::Write as _;

use crate::linear::{IssueSummary, ProjectSummary};

pub fn render_projects_table(projects: &[ProjectSummary]) -> String {
    if projects.is_empty() {
        return "No projects matched the current filters.".to_string();
    }

    render_table(
        &["Project", "Teams", "Progress", "URL"],
        &projects
            .iter()
            .map(|project| {
                vec![
                    project.name.clone(),
                    project
                        .teams
                        .iter()
                        .map(|team| team.key.clone())
                        .collect::<Vec<_>>()
                        .join(", "),
                    project
                        .progress
                        .map(|progress| format!("{:.0}%", progress * 100.0))
                        .unwrap_or_else(|| "-".to_string()),
                    project.url.clone(),
                ]
            })
            .collect::<Vec<_>>(),
    )
}

pub fn render_issue_summary(prefix: &str, issue: &IssueSummary) -> String {
    let mut output = String::new();
    let _ = writeln!(&mut output, "{prefix}: {}", issue.identifier);
    let _ = writeln!(&mut output, "Title: {}", issue.title);
    let _ = writeln!(
        &mut output,
        "State: {}",
        issue
            .state
            .as_ref()
            .map(|state| state.name.as_str())
            .unwrap_or("unknown")
    );
    let _ = writeln!(
        &mut output,
        "Project: {}",
        issue
            .project
            .as_ref()
            .map(|project| project.name.as_str())
            .unwrap_or("none")
    );
    let _ = write!(&mut output, "URL: {}", issue.url);
    output
}

pub(crate) fn render_issues_list_output(
    issues: &[IssueSummary],
    team: Option<&str>,
    project: Option<&str>,
    project_id: Option<&str>,
    state: Option<&str>,
) -> String {
    let table = render_issues_table(issues);
    if !issues.is_empty() {
        return table;
    }

    let mut filters = Vec::new();
    if let Some(team) = team {
        filters.push(format!("team={team}"));
    }
    if let Some(project) = project {
        filters.push(format!("project={project}"));
    }
    if let Some(project_id) = project_id {
        filters.push(format!("project_id={project_id}"));
    }
    if let Some(state) = state {
        filters.push(format!("state={state}"));
    }

    if filters.is_empty() {
        table
    } else {
        format!("{table}\nApplied filters: {}", filters.join(", "))
    }
}

fn render_issues_table(issues: &[IssueSummary]) -> String {
    if issues.is_empty() {
        return "No issues matched the current filters.".to_string();
    }

    let headers = ["Issue", "State", "Project", "Title"];
    let mut rows = issues
        .iter()
        .map(|issue| {
            vec![
                issue.identifier.clone(),
                issue
                    .state
                    .as_ref()
                    .map(|state| state.name.clone())
                    .unwrap_or_else(|| "-".to_string()),
                issue
                    .project
                    .as_ref()
                    .map(|project| project.name.clone())
                    .unwrap_or_else(|| "-".to_string()),
                issue.title.clone(),
            ]
        })
        .collect::<Vec<_>>();

    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();
    for row in &rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.len());
        }
    }

    let mut lines = Vec::new();
    lines.push(render_row(
        &headers
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>(),
        &widths,
    ));
    lines.push(render_row(
        &widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>(),
        &widths,
    ));
    for row in rows.drain(..) {
        lines.push(render_row(&row, &widths));
    }

    lines.join("\n")
}

fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();
    for row in rows {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.len());
        }
    }

    let mut lines = Vec::new();
    lines.push(render_row(
        &headers
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>(),
        &widths,
    ));
    lines.push(render_row(
        &widths
            .iter()
            .map(|width| "-".repeat(*width))
            .collect::<Vec<_>>(),
        &widths,
    ));
    for row in rows {
        lines.push(render_row(row, &widths));
    }

    lines.join("\n")
}

fn render_row(row: &[String], widths: &[usize]) -> String {
    row.iter()
        .enumerate()
        .map(|(index, value)| format!("{value:width$}", width = widths[index]))
        .collect::<Vec<_>>()
        .join(" | ")
}

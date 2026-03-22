mod dashboard;
pub(crate) mod state;
mod store;

use std::io;
use std::io::IsTerminal;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use serde::Deserialize;

use crate::cli::ImproveArgs;
use crate::fs::{PlanningPaths, canonicalize_existing_dir};
use crate::github_pr::GhCli;

use dashboard::{
    ImproveAction, ImproveBrowserState, ImproveDashboardData, ImprovePrEntry,
    render, render_improve_dashboard_snapshot,
};
use state::ImproveState;
use store::{load_improve_state, state_file_display};

const INPUT_POLL_INTERVAL_MILLIS: u64 = 100;

/// Discovered open PR metadata from `gh pr list`.
#[derive(Debug, Clone, Deserialize)]
struct GhOpenPr {
    number: u64,
    title: String,
    url: String,
    body: Option<String>,
    author: GhPrAuthor,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "baseRefName")]
    base_ref_name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GhPrAuthor {
    login: String,
}

/// Run the `meta agents improve` command.
///
/// Returns an error when the repository root cannot be resolved, GitHub discovery fails,
/// or the TUI cannot be initialized.
pub(crate) async fn run_improve(args: &ImproveArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.root)?;
    let paths = PlanningPaths::new(&root);
    let gh = GhCli;

    let open_prs = discover_open_prs(&gh, &root)?;
    let state = load_improve_state(&paths)?;

    let data = build_dashboard_data(&root, &open_prs, &state, &paths);

    if args.render_once {
        let mut browser_state = ImproveBrowserState::default();
        for event in &args.events {
            let action = match event {
                ImproveEventArg::Up => ImproveAction::Up,
                ImproveEventArg::Down => ImproveAction::Down,
                ImproveEventArg::Tab => ImproveAction::Tab,
                ImproveEventArg::Enter => ImproveAction::Enter,
                ImproveEventArg::Back => ImproveAction::Back,
            };
            browser_state.apply_action(action, &data);
        }
        let snapshot = render_improve_dashboard_snapshot(
            args.width,
            args.height,
            &data,
            &browser_state,
        )?;
        println!("{snapshot}");
        return Ok(());
    }

    if !io::stdout().is_terminal() {
        println!("{}", render_text_summary(&data));
        return Ok(());
    }

    run_interactive_dashboard(&root, &paths, data)?;

    Ok(())
}

/// Discover open PRs for the current repository using `gh`.
///
/// Returns an error when `gh` is not available or the repository has no GitHub remote.
fn discover_open_prs(gh: &GhCli, root: &Path) -> Result<Vec<GhOpenPr>> {
    gh.run_json(
        root,
        &[
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "number,title,url,body,author,headRefName,baseRefName",
        ],
    )
    .context("failed to discover open PRs for the current repository")
}

fn build_dashboard_data(
    root: &Path,
    open_prs: &[GhOpenPr],
    state: &ImproveState,
    paths: &PlanningPaths,
) -> ImproveDashboardData {
    let scope = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repository")
        .to_string();

    let prs: Vec<ImprovePrEntry> = open_prs
        .iter()
        .map(|pr| {
            let body_preview = pr
                .body
                .as_deref()
                .unwrap_or("")
                .lines()
                .take(3)
                .collect::<Vec<_>>()
                .join(" ");
            ImprovePrEntry {
                number: pr.number,
                title: pr.title.clone(),
                url: pr.url.clone(),
                author: pr.author.login.clone(),
                head_branch: pr.head_ref_name.clone(),
                base_branch: pr.base_ref_name.clone(),
                body_preview,
            }
        })
        .collect();

    ImproveDashboardData {
        scope,
        prs,
        sessions: state.sorted_sessions(),
        now_epoch_seconds: now_epoch_seconds(),
        state_file: state_file_display(paths, root),
    }
}

fn render_text_summary(data: &ImproveDashboardData) -> String {
    let mut lines = vec![
        format!("Improve: {}", data.scope),
        format!("{} open PR(s), {} session(s)", data.prs.len(), data.sessions.len()),
        format!("State file: {}", data.state_file),
    ];
    if !data.prs.is_empty() {
        lines.push("Open PRs:".to_string());
        for pr in &data.prs {
            lines.push(format!("  #{} {} ({})", pr.number, pr.title, pr.author));
        }
    }
    if !data.sessions.is_empty() {
        lines.push("Sessions:".to_string());
        for session in &data.sessions {
            lines.push(format!(
                "  #{} [{}] {} ({})",
                session.source_pr.number,
                session.phase.display_label(),
                session.source_pr.title,
                session.age_label(data.now_epoch_seconds),
            ));
        }
    }
    lines.join("\n")
}

fn run_interactive_dashboard(
    _root: &Path,
    _paths: &PlanningPaths,
    initial_data: ImproveDashboardData,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut browser_state = ImproveBrowserState::default();
    let data = initial_data;

    loop {
        terminal.draw(|frame| render(frame, &data, &browser_state))?;

        if event::poll(std::time::Duration::from_millis(INPUT_POLL_INTERVAL_MILLIS))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q')
                    | KeyCode::Char('c')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        break;
                    }
                    KeyCode::Char('q') => break,
                    KeyCode::Esc => break,
                    KeyCode::Tab => {
                        browser_state.apply_action(ImproveAction::Tab, &data);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        browser_state.apply_action(ImproveAction::Up, &data);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        browser_state.apply_action(ImproveAction::Down, &data);
                    }
                    KeyCode::Enter => {
                        browser_state.apply_action(ImproveAction::Enter, &data);
                    }
                    KeyCode::Backspace => {
                        browser_state.apply_action(ImproveAction::Back, &data);
                    }
                    _ => {}
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn now_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

use clap::ValueEnum;

/// Event arguments for scripted render-once snapshots.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ImproveEventArg {
    Up,
    Down,
    Tab,
    Enter,
    Back,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_open_prs_parses_sample_json() {
        let json = r#"[
            {
                "number": 42,
                "title": "Test PR",
                "url": "https://example.test/pull/42",
                "body": "PR body text\nwith multiple lines",
                "author": {"login": "alice"},
                "headRefName": "feature-branch",
                "baseRefName": "main"
            }
        ]"#;
        let prs: Vec<GhOpenPr> = serde_json::from_str(json).expect("parse");
        assert_eq!(prs.len(), 1);
        assert_eq!(prs[0].number, 42);
        assert_eq!(prs[0].author.login, "alice");
        assert_eq!(prs[0].head_ref_name, "feature-branch");
    }

    #[test]
    fn text_summary_renders_cleanly() {
        let data = ImproveDashboardData {
            scope: "test/repo".to_string(),
            prs: vec![dashboard::ImprovePrEntry {
                number: 42,
                title: "Test PR".to_string(),
                url: "https://example.test/pull/42".to_string(),
                author: "alice".to_string(),
                head_branch: "feature".to_string(),
                base_branch: "main".to_string(),
                body_preview: "body".to_string(),
            }],
            sessions: vec![],
            now_epoch_seconds: 1000,
            state_file: "state.json".to_string(),
        };
        let summary = render_text_summary(&data);
        assert!(summary.contains("Improve: test/repo"));
        assert!(summary.contains("#42"));
        assert!(summary.contains("alice"));
    }
}

use std::io::{self, IsTerminal};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
#[cfg(test)]
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde::{Deserialize, Serialize};

use crate::fs::write_text_file;

pub(crate) const SPINNER_FRAMES: &[&str] = &["|", "/", "-", "\\"];

#[derive(Debug, Clone)]
pub(crate) struct LoadingPanelData {
    pub(crate) title: String,
    pub(crate) message: String,
    pub(crate) detail: String,
    pub(crate) spinner_index: usize,
    pub(crate) status_line: String,
}

pub(crate) fn render_loading_panel(frame: &mut Frame<'_>, area: Rect, data: &LoadingPanelData) {
    let [outer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(9)])
        .flex(Flex::Center)
        .areas(area);
    let [panel] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70)])
        .flex(Flex::Center)
        .areas(outer);
    let loading = Paragraph::new(Text::from(vec![
        Line::styled(
            format!(
                "{} {}",
                SPINNER_FRAMES[data.spinner_index % SPINNER_FRAMES.len()],
                data.message
            ),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::from(""),
        Line::from(data.detail.clone()),
        Line::from(""),
        Line::styled(
            data.status_line.clone(),
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(data.title.clone()),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(loading, panel);
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProgressStepState {
    Pending,
    Running,
    Complete,
    Failed,
}

impl ProgressStepState {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Complete => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ProgressRunState {
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ProgressStepRecord {
    pub(crate) key: String,
    pub(crate) label: String,
    pub(crate) state: ProgressStepState,
    pub(crate) detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ProgressEventRecord {
    pub(crate) timestamp: String,
    pub(crate) phase_key: String,
    pub(crate) phase_label: String,
    pub(crate) state: ProgressStepState,
    pub(crate) detail: Option<String>,
    pub(crate) pull_request: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ProgressArtifact {
    pub(crate) title: String,
    pub(crate) status: ProgressRunState,
    pub(crate) current_phase: Option<String>,
    pub(crate) current_phase_key: Option<String>,
    pub(crate) active_detail: Option<String>,
    pub(crate) started_at: String,
    pub(crate) updated_at: String,
    pub(crate) finished_at: Option<String>,
    pub(crate) steps: Vec<ProgressStepRecord>,
    pub(crate) events: Vec<ProgressEventRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProgressStepDefinition {
    pub(crate) key: &'static str,
    pub(crate) label: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProgressOutputMode {
    Interactive,
    Text,
    #[cfg(test)]
    Hidden,
}

#[derive(Debug, Clone)]
pub(crate) struct ProgressViewData {
    pub(crate) title: String,
    pub(crate) status_line: String,
    pub(crate) active_detail: Option<String>,
    pub(crate) steps: Vec<ProgressStepRecord>,
    pub(crate) notes: Vec<String>,
}

enum ProgressDisplay {
    Tui(Terminal<CrosstermBackend<io::Stdout>>),
    Text {
        last_line: Option<String>,
    },
    #[cfg(test)]
    Hidden,
}

impl ProgressDisplay {
    fn start(mode: ProgressOutputMode) -> Result<Self> {
        Ok(match mode {
            ProgressOutputMode::Interactive if io::stdout().is_terminal() => {
                let mut stdout = io::stdout();
                execute!(stdout, EnterAlternateScreen, Hide)?;
                let backend = CrosstermBackend::new(stdout);
                Self::Tui(Terminal::new(backend)?)
            }
            ProgressOutputMode::Interactive | ProgressOutputMode::Text => {
                Self::Text { last_line: None }
            }
            #[cfg(test)]
            ProgressOutputMode::Hidden => Self::Hidden,
        })
    }

    fn render(&mut self, data: &ProgressViewData) -> Result<()> {
        match self {
            Self::Tui(terminal) => {
                terminal.draw(|frame| render_progress_dashboard(frame, data))?;
            }
            Self::Text { last_line } => {
                let mut line = data.status_line.clone();
                if let Some(detail) = &data.active_detail
                    && !detail.is_empty()
                {
                    line.push_str(": ");
                    line.push_str(detail);
                }
                if last_line.as_ref() != Some(&line) {
                    println!("{line}");
                    *last_line = Some(line);
                }
            }
            #[cfg(test)]
            Self::Hidden => {}
        }
        Ok(())
    }
}

impl Drop for ProgressDisplay {
    fn drop(&mut self) {
        let Self::Tui(terminal) = self else {
            return;
        };

        let _ = execute!(terminal.backend_mut(), Show, LeaveAlternateScreen);
    }
}

pub(crate) struct ProgressTracker {
    artifact_path: PathBuf,
    artifact: ProgressArtifact,
    display: ProgressDisplay,
}

impl ProgressTracker {
    pub(crate) fn start(
        title: impl Into<String>,
        artifact_path: impl Into<PathBuf>,
        steps: &[ProgressStepDefinition],
        mode: ProgressOutputMode,
    ) -> Result<Self> {
        let timestamp = now_timestamp();
        let artifact = ProgressArtifact {
            title: title.into(),
            status: ProgressRunState::Running,
            current_phase: None,
            current_phase_key: None,
            active_detail: None,
            started_at: timestamp.clone(),
            updated_at: timestamp,
            finished_at: None,
            steps: steps
                .iter()
                .map(|step| ProgressStepRecord {
                    key: step.key.to_string(),
                    label: step.label.to_string(),
                    state: ProgressStepState::Pending,
                    detail: None,
                })
                .collect(),
            events: Vec::new(),
        };
        let mut tracker = Self {
            artifact_path: artifact_path.into(),
            artifact,
            display: ProgressDisplay::start(mode)?,
        };
        tracker.persist()?;
        Ok(tracker)
    }

    pub(crate) fn start_step(&mut self, key: &str, detail: impl Into<String>) -> Result<()> {
        let detail = detail.into();
        self.transition_step(key, ProgressStepState::Running, Some(detail), None)
    }

    pub(crate) fn update_detail(
        &mut self,
        key: &str,
        detail: impl Into<String>,
        pull_request: Option<u64>,
    ) -> Result<()> {
        let detail = detail.into();
        self.transition_step(key, ProgressStepState::Running, Some(detail), pull_request)
    }

    pub(crate) fn complete_step(&mut self, key: &str, detail: impl Into<String>) -> Result<()> {
        let detail = detail.into();
        self.transition_step(key, ProgressStepState::Complete, Some(detail), None)
    }

    pub(crate) fn fail_step(
        &mut self,
        key: &str,
        detail: impl Into<String>,
        pull_request: Option<u64>,
    ) -> Result<()> {
        let detail = detail.into();
        self.artifact.status = ProgressRunState::Failed;
        self.artifact.finished_at = Some(now_timestamp());
        self.transition_step(key, ProgressStepState::Failed, Some(detail), pull_request)
    }

    pub(crate) fn finish_success(&mut self, detail: impl Into<String>) -> Result<()> {
        self.artifact.status = ProgressRunState::Succeeded;
        self.artifact.finished_at = Some(now_timestamp());
        self.artifact.active_detail = Some(detail.into());
        self.persist()
    }

    pub(crate) fn artifact(&self) -> &ProgressArtifact {
        &self.artifact
    }

    fn transition_step(
        &mut self,
        key: &str,
        state: ProgressStepState,
        detail: Option<String>,
        pull_request: Option<u64>,
    ) -> Result<()> {
        if let Some(current_key) = self.artifact.current_phase_key.as_deref()
            && current_key != key
            && let Some(step) =
                self.artifact.steps.iter_mut().find(|step| {
                    step.key == current_key && step.state == ProgressStepState::Running
                })
        {
            step.state = ProgressStepState::Complete;
        }

        let current = self
            .artifact
            .steps
            .iter_mut()
            .find(|step| step.key == key)
            .context("progress step should exist")?;
        current.state = state;
        current.detail = detail.clone();
        self.artifact.current_phase = Some(current.label.clone());
        self.artifact.current_phase_key = Some(current.key.clone());
        self.artifact.active_detail = detail.clone();
        self.artifact.updated_at = now_timestamp();
        self.artifact.events.push(ProgressEventRecord {
            timestamp: self.artifact.updated_at.clone(),
            phase_key: current.key.clone(),
            phase_label: current.label.clone(),
            state,
            detail,
            pull_request,
        });
        self.persist()
    }

    fn persist(&mut self) -> Result<()> {
        let encoded = serde_json::to_string_pretty(&self.artifact)?;
        write_text_file(&self.artifact_path, &encoded, true)?;
        self.display.render(&self.view_data())
    }

    fn view_data(&self) -> ProgressViewData {
        let active_index = self
            .artifact
            .current_phase_key
            .as_deref()
            .and_then(|key| self.artifact.steps.iter().position(|step| step.key == key))
            .map(|index| index + 1);
        let total = self.artifact.steps.len();
        let status_line = match (&self.artifact.current_phase, active_index) {
            (Some(label), Some(index)) => format!("Phase {index}/{total}: {label}"),
            _ => self.artifact.title.clone(),
        };
        ProgressViewData {
            title: self.artifact.title.clone(),
            status_line,
            active_detail: self.artifact.active_detail.clone(),
            steps: self.artifact.steps.clone(),
            notes: vec![format!(
                "Progress artifact: {}",
                self.artifact_path.display()
            )],
        }
    }
}

pub(crate) fn render_progress_dashboard(frame: &mut Frame<'_>, data: &ProgressViewData) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(frame.area());
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(outer[1]);

    let header = Paragraph::new(Text::from(vec![
        Line::from(data.title.clone()),
        Line::from(data.status_line.clone()),
        Line::from("The merge runner keeps this progress view visible until success or failure."),
        Line::from("Structured progress is saved on every update for later reconstruction."),
    ]))
    .wrap(Wrap { trim: true })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Long-Running Progress"),
    );
    frame.render_widget(header, outer[0]);

    let step_items = data
        .steps
        .iter()
        .map(|step| {
            ListItem::new(Text::from(vec![
                Line::from(format!("[{}] {}", step.state.label(), step.label)),
                Line::from(step.detail.clone().unwrap_or_default()),
            ]))
        })
        .collect::<Vec<_>>();
    let steps = List::new(step_items).block(Block::default().borders(Borders::ALL).title("Phases"));
    frame.render_widget(steps, body[0]);

    let active = Paragraph::new(Text::from(vec![
        Line::styled(
            "Active substep",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::from(""),
        Line::from(
            data.active_detail
                .clone()
                .unwrap_or_else(|| "Waiting for the next update.".to_string()),
        ),
    ]))
    .wrap(Wrap { trim: true })
    .block(Block::default().borders(Borders::ALL).title("Details"));
    frame.render_widget(active, body[1]);

    let footer = Paragraph::new(data.notes.join("\n"))
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Notes"));
    frame.render_widget(footer, outer[2]);
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
pub(crate) fn render_progress_snapshot(
    data: &ProgressViewData,
    width: u16,
    height: u16,
) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render_progress_dashboard(frame, data))?;
    Ok(snapshot(terminal.backend()))
}

#[cfg(test)]
pub(crate) fn render_loading_snapshot(
    data: &LoadingPanelData,
    width: u16,
    height: u16,
) -> Result<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| render_loading_panel(frame, frame.area(), data))?;
    Ok(snapshot(terminal.backend()))
}

#[cfg(test)]
fn snapshot(backend: &TestBackend) -> String {
    let buffer = backend.buffer();
    let mut lines = Vec::new();

    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_string());
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn loading_panel_snapshot_surfaces_shared_copy() {
        let snapshot = render_loading_snapshot(
            &LoadingPanelData {
                title: "Agent Working [loading]".to_string(),
                message: "Planning backlog slice".to_string(),
                detail: "Waiting for the agent to answer.".to_string(),
                spinner_index: 1,
                status_line:
                    "State: loading. The dashboard advances automatically when the agent responds."
                        .to_string(),
            },
            100,
            20,
        )
        .expect("shared loading panel should render");

        assert!(snapshot.contains("Agent Working [loading]"));
        assert!(snapshot.contains("/ Planning backlog slice"));
        assert!(snapshot.contains("Waiting for the agent to answer."));
    }

    #[test]
    fn progress_tracker_persists_failed_progress_state() {
        let temp = tempdir().expect("tempdir should exist");
        let artifact_path = temp.path().join("progress.json");
        let steps = [
            ProgressStepDefinition {
                key: "prepare",
                label: "Workspace preparation",
            },
            ProgressStepDefinition {
                key: "validate",
                label: "Validation",
            },
        ];

        let mut tracker = ProgressTracker::start(
            "meta merge progress",
            &artifact_path,
            &steps,
            ProgressOutputMode::Hidden,
        )
        .expect("tracker should start");
        tracker
            .start_step("prepare", "Cloning the aggregate workspace")
            .expect("prepare should start");
        tracker
            .complete_step("prepare", "Workspace is ready")
            .expect("prepare should complete");
        tracker
            .start_step("validate", "Running make quality")
            .expect("validate should start");
        tracker
            .fail_step("validate", "Validation command exited with status 1", None)
            .expect("validate should fail");

        let artifact = std::fs::read_to_string(&artifact_path).expect("artifact should exist");
        assert!(artifact.contains("\"status\": \"failed\""));
        assert!(artifact.contains("\"current_phase_key\": \"validate\""));
        assert!(artifact.contains("Validation command exited with status 1"));
    }

    #[test]
    fn progress_dashboard_snapshot_shows_active_phase_and_details() {
        let snapshot = render_progress_snapshot(
            &ProgressViewData {
                title: "meta merge progress".to_string(),
                status_line: "Phase 3/6: Merge application".to_string(),
                active_detail: Some(
                    "Applying pull request #101 and waiting for git merge to finish.".to_string(),
                ),
                steps: vec![
                    ProgressStepRecord {
                        key: "prepare".to_string(),
                        label: "Workspace preparation".to_string(),
                        state: ProgressStepState::Complete,
                        detail: Some("Workspace prepared".to_string()),
                    },
                    ProgressStepRecord {
                        key: "plan".to_string(),
                        label: "Plan generation".to_string(),
                        state: ProgressStepState::Complete,
                        detail: Some("Planner order recorded".to_string()),
                    },
                    ProgressStepRecord {
                        key: "apply".to_string(),
                        label: "Merge application".to_string(),
                        state: ProgressStepState::Running,
                        detail: Some("Applying pull request #101".to_string()),
                    },
                ],
                notes: vec![
                    "Progress artifact: .metastack/merge-runs/run/progress.json".to_string(),
                ],
            },
            120,
            28,
        )
        .expect("progress dashboard should render");

        assert!(snapshot.contains("Long-Running Progress"));
        assert!(snapshot.contains("Phase 3/6: Merge application"));
        assert!(snapshot.contains("Applying pull request #101"));
        assert!(snapshot.contains("Progress artifact: .metastack/merge-runs/run/progress.json"));
    }
}

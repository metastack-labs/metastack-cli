use std::collections::BTreeSet;
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
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::ListItem;
use ratatui::{Frame, Terminal};
use serde::{Deserialize, Serialize};

use crate::fs::write_text_file;
use crate::tui::theme::{
    Tone, badge, emphasis_style, empty_state, key_hints, label_style, list, muted_style,
    panel_title, paragraph, tone_style,
};

pub(crate) const SPINNER_FRAMES: &[&str] = &[
    "[=   ]", "[==  ]", "[=== ]", "[ ===]", "[  ===]", "[   ==]", "[   =]", "[  ==]",
];

#[derive(Debug, Clone)]
pub(crate) struct LoadingPanelData {
    pub(crate) title: String,
    pub(crate) message: String,
    pub(crate) detail: String,
    pub(crate) spinner_index: usize,
    pub(crate) status_line: String,
}

pub(crate) fn render_loading_panel(frame: &mut Frame<'_>, area: Rect, data: &LoadingPanelData) {
    let panel_height = if area.height < 14 { 10 } else { 11 };
    let [outer] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(panel_height)])
        .flex(Flex::Center)
        .areas(area);
    let [panel] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(if area.width < 88 {
            92
        } else {
            70
        })])
        .flex(Flex::Center)
        .areas(outer);

    let loading = paragraph(
        Text::from(vec![
            Line::from(vec![
                badge("loading", Tone::Info),
                Span::raw(" "),
                Span::raw(format!(
                    "{} {}",
                    SPINNER_FRAMES[data.spinner_index % SPINNER_FRAMES.len()],
                    data.message
                )),
            ]),
            Line::from(""),
            Line::from(data.detail.clone()),
            Line::from(""),
            Line::from(Span::styled(
                data.status_line.clone(),
                crate::tui::theme::muted_style(),
            )),
        ]),
        panel_title(data.title.clone(), false),
    )
    .wrap(ratatui::widgets::Wrap { trim: false });
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

impl ProgressRunState {
    fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "success",
            Self::Failed => "failed",
        }
    }

    fn tone(self) -> Tone {
        match self {
            Self::Running => Tone::Info,
            Self::Succeeded => Tone::Success,
            Self::Failed => Tone::Danger,
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
    pub(crate) status: ProgressRunState,
    pub(crate) status_line: String,
    pub(crate) active_detail: Option<String>,
    pub(crate) steps: Vec<ProgressStepRecord>,
    pub(crate) notes: Vec<String>,
}

enum ProgressDisplay {
    Tui(Terminal<CrosstermBackend<io::Stdout>>),
    Text {
        last_line: Option<String>,
        last_detail: Option<String>,
        emitted_notes: BTreeSet<String>,
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
            ProgressOutputMode::Interactive | ProgressOutputMode::Text => Self::Text {
                last_line: None,
                last_detail: None,
                emitted_notes: BTreeSet::new(),
            },
            #[cfg(test)]
            ProgressOutputMode::Hidden => Self::Hidden,
        })
    }

    fn render(&mut self, data: &ProgressViewData) -> Result<()> {
        match self {
            Self::Tui(terminal) => {
                terminal.draw(|frame| render_progress_dashboard(frame, data))?;
            }
            Self::Text {
                last_line,
                last_detail,
                emitted_notes,
            } => {
                for note in &data.notes {
                    if emitted_notes.insert(note.clone()) {
                        println!("{note}");
                    }
                }
                if last_line.as_ref() != Some(&data.status_line) {
                    println!("==> {}", data.status_line);
                    *last_line = Some(data.status_line.clone());
                }
                match &data.active_detail {
                    Some(detail) if !detail.is_empty() => {
                        if last_detail.as_ref() != Some(detail) {
                            println!("    {detail}");
                            *last_detail = Some(detail.clone());
                        }
                    }
                    _ => *last_detail = None,
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
        let status_line = match (
            self.artifact.status,
            &self.artifact.current_phase,
            active_index,
        ) {
            (ProgressRunState::Running, Some(label), Some(index)) => {
                format!("Running phase {index}/{total}: {label}")
            }
            (ProgressRunState::Running, _, _) => {
                format!("Starting merge run ({total} phases queued)")
            }
            (ProgressRunState::Succeeded, _, _) => {
                format!("Merge run succeeded after {total} phases")
            }
            (ProgressRunState::Failed, Some(label), Some(index)) => {
                format!("Merge run failed during phase {index}/{total}: {label}")
            }
            (ProgressRunState::Failed, _, _) => "Merge run failed".to_string(),
        };
        ProgressViewData {
            title: self.artifact.title.clone(),
            status: self.artifact.status,
            status_line,
            active_detail: self.artifact.active_detail.clone(),
            steps: self.artifact.steps.clone(),
            notes: vec![
                format!(
                    "Run artifacts: {}",
                    self.artifact_path
                        .parent()
                        .unwrap_or(self.artifact_path.as_path())
                        .display()
                ),
                format!("Progress JSON: {}", self.artifact_path.display()),
            ],
        }
    }
}

pub(crate) fn render_progress_dashboard(frame: &mut Frame<'_>, data: &ProgressViewData) {
    let area = frame.area();
    let narrow = area.width < 100;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if narrow { 6 } else { 7 }),
            Constraint::Min(10),
            Constraint::Length(if narrow { 5 } else { 4 }),
        ])
        .split(area);
    let body = Layout::default()
        .direction(if narrow {
            Direction::Vertical
        } else {
            Direction::Horizontal
        })
        .constraints(if narrow {
            vec![Constraint::Percentage(52), Constraint::Percentage(48)]
        } else {
            vec![Constraint::Percentage(48), Constraint::Percentage(52)]
        })
        .split(outer[1]);

    let header = paragraph(
        Text::from(vec![
            Line::from(vec![
                badge(data.status.label(), data.status.tone()),
                Span::raw(" "),
                Span::styled(data.title.clone(), emphasis_style()),
            ]),
            Line::from(Span::styled(data.status_line.clone(), emphasis_style())),
            Line::from(
                "The merge runner keeps the current phase, live detail, and saved artifacts visible until exit.",
            ),
            key_hints(&[("Ctrl-C", "exit"), ("JSON", "saved every update")]),
        ]),
        panel_title("Merge Run Status", false),
    );
    frame.render_widget(header, outer[0]);

    let step_items = data
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let tone = match step.state {
                ProgressStepState::Pending => Tone::Muted,
                ProgressStepState::Running => Tone::Info,
                ProgressStepState::Complete => Tone::Success,
                ProgressStepState::Failed => Tone::Danger,
            };
            let label = if step.state == ProgressStepState::Running {
                Span::styled(step.label.clone(), emphasis_style())
            } else {
                Span::raw(step.label.clone())
            };
            ListItem::new(Text::from(vec![
                Line::from(vec![
                    Span::styled(format!("{:02}. ", index + 1), label_style()),
                    badge(step.state.label(), tone),
                    Span::raw(" "),
                    label,
                    if step.state == ProgressStepState::Running {
                        Span::raw(" ")
                    } else {
                        Span::raw("")
                    },
                    if step.state == ProgressStepState::Running {
                        badge("active", Tone::Accent)
                    } else {
                        Span::raw("")
                    },
                ]),
                Line::from(Span::styled(
                    step.detail
                        .clone()
                        .unwrap_or_else(|| "Waiting for this phase to start.".to_string()),
                    muted_style(),
                )),
            ]))
        })
        .collect::<Vec<_>>();
    let steps = list(step_items, panel_title("Phase Timeline", false));
    frame.render_widget(steps, body[0]);

    let active_text = if let Some(detail) = &data.active_detail {
        Text::from(vec![
            Line::from(vec![
                badge("active", Tone::Accent),
                Span::raw(" Current activity"),
            ]),
            Line::from(""),
            Line::from(Span::styled(detail.clone(), tone_style(Tone::Accent))),
            Line::from(""),
            Line::from(Span::styled(
                "Confirm and cancel prompts appear here as phases advance.",
                muted_style(),
            )),
        ])
    } else {
        empty_state(
            "No phase detail is active yet.",
            "The next structured progress event will appear here.",
        )
    };
    let active = paragraph(active_text, panel_title("Current Step", false));
    frame.render_widget(active, body[1]);

    let footer = paragraph(data.notes.join("\n"), panel_title("Artifacts", false));
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
        assert!(snapshot.contains("[==  ] Planning backlog slice"));
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
                status: ProgressRunState::Running,
                status_line: "Running phase 3/6: Merge application".to_string(),
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
                    "Run artifacts: .metastack/merge-runs/run".to_string(),
                    "Progress JSON: .metastack/merge-runs/run/progress.json".to_string(),
                ],
            },
            120,
            28,
        )
        .expect("progress dashboard should render");

        assert!(snapshot.contains("Merge Run Status"));
        assert!(snapshot.contains("Running phase 3/6: Merge application"));
        assert!(snapshot.contains("Applying pull request #101"));
        assert!(snapshot.contains("Progress JSON: .metastack/merge-runs/run/progress.json"));
    }
}

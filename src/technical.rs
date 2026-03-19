use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
#[cfg(test)]
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde::Deserialize;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

use crate::agents::run_agent_capture;
use crate::backlog::{
    BacklogIssueMetadata, INDEX_FILE_NAME, ManagedFileRecord, RenderedTemplateFile,
    TemplateContext, ensure_no_unresolved_placeholders, render_template_files, save_issue_metadata,
    write_rendered_backlog_item,
};
use crate::cli::{RunAgentArgs, SyncPushArgs, TechnicalArgs};
use crate::config::{AGENT_ROUTE_BACKLOG_SPLIT, load_required_planning_meta};
use crate::context::load_workflow_contract;
use crate::fs::{PlanningPaths, canonicalize_existing_dir, display_path};
use crate::linear::browser::{
    IssueSearchResult, render_issue_preview, render_issue_row, search_issues,
};
use crate::linear::{
    IssueCreateSpec, IssueListFilters, IssueSummary, PreparedIssueContext, TicketDiscussionBudgets,
    materialize_issue_context, prepare_issue_context, render_ticket_image_summary,
};
use crate::progress::{LoadingPanelData, SPINNER_FRAMES, render_loading_panel};
use crate::scaffold::{ensure_backlog_templates, ensure_planning_layout};
use crate::sync_command::run_sync_push;
use crate::tui::fields::{InputFieldState, MultiSelectFieldState};
use crate::{LinearCommandContext, load_linear_command_context};

const ISSUE_PICKER_LIMIT: usize = 250;

#[derive(Debug, Deserialize)]
struct TechnicalBacklogDraft {
    #[serde(default)]
    files: Vec<TechnicalBacklogFile>,
}

#[derive(Debug, Deserialize)]
struct TechnicalBacklogFile {
    path: String,
    contents: String,
}

#[derive(Debug, Clone)]
struct TechnicalGeneratedBacklog {
    parent: IssueSummary,
    child_title: String,
    selected_acceptance_criteria: Vec<String>,
    prepared_context: PreparedIssueContext,
    files: Vec<RenderedTemplateFile>,
}

#[derive(Debug, Clone)]
struct IssuePickerApp {
    query: InputFieldState,
    issues: Vec<IssueSummary>,
    selected: usize,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct TechnicalReviewApp {
    generated: TechnicalGeneratedBacklog,
    selected_file: usize,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct AcceptanceCriteriaApp {
    parent: IssueSummary,
    criteria: MultiSelectFieldState,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct LoadingApp {
    message: String,
    detail: String,
    spinner_index: usize,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum TechnicalStage {
    PickIssue(IssuePickerApp),
    SelectCriteria(AcceptanceCriteriaApp),
    Loading(LoadingApp),
    Review(TechnicalReviewApp),
}

struct TechnicalSessionApp {
    stage: TechnicalStage,
    pending: Option<PendingTechnicalJob>,
}

struct PendingTechnicalJob {
    receiver: Receiver<Result<TechnicalGeneratedBacklog>>,
    previous_stage: Option<TechnicalRecoveryStage>,
}

#[allow(clippy::large_enum_variant)]
enum TechnicalAction {
    None,
    SelectIssue(IssueSummary),
    Generate(TechnicalGenerationRequest),
    Confirm(TechnicalGeneratedBacklog),
}

#[derive(Debug, Clone)]
struct TechnicalGenerationRequest {
    parent: IssueSummary,
    selected_acceptance_criteria: Vec<String>,
    discussion_budgets: TicketDiscussionBudgets,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum TechnicalRecoveryStage {
    PickIssue(IssuePickerApp),
    SelectCriteria(AcceptanceCriteriaApp),
}

#[allow(clippy::large_enum_variant)]
enum InteractiveTechnicalExit {
    Cancelled,
    Confirmed(TechnicalGeneratedBacklog),
}

pub async fn run_technical(args: &TechnicalArgs) -> Result<()> {
    let root = canonicalize_existing_dir(&args.client.root)?;
    let planning_meta = load_required_planning_meta(&root, "technical")?;
    let discussion_budgets = resolve_ticket_discussion_budgets(&planning_meta);
    ensure_planning_layout(&root, false)?;
    ensure_backlog_templates(&root, false)?;
    let LinearCommandContext {
        service,
        default_team,
        default_project_id,
    } = load_linear_command_context(&args.client, None)?;
    let can_launch_tui = io::stdin().is_terminal() && io::stdout().is_terminal();

    let generated = if can_launch_tui {
        let initial_parent = match args.issue.as_ref() {
            Some(issue) => Some(service.load_issue(issue).await?),
            None => None,
        };
        let available_issues = if initial_parent.is_none() {
            service
                .list_issues(IssueListFilters {
                    team: default_team.clone(),
                    project_id: default_project_id.clone(),
                    limit: ISSUE_PICKER_LIMIT,
                    ..IssueListFilters::default()
                })
                .await?
        } else {
            Vec::new()
        };

        match run_interactive_technical_session(
            &root,
            initial_parent,
            available_issues,
            discussion_budgets,
        )? {
            InteractiveTechnicalExit::Cancelled => {
                println!("Technical generation cancelled.");
                return Ok(());
            }
            InteractiveTechnicalExit::Confirmed(generated) => generated,
        }
    } else {
        let issue = args.issue.as_ref().ok_or_else(|| {
            anyhow!("`meta backlog tech` requires an issue identifier when running without a TTY")
        })?;
        let parent = service.load_issue(issue).await?;
        let selected_acceptance_criteria =
            extract_acceptance_criteria(parent.description.as_deref());
        build_generated_backlog(
            &root,
            &parent,
            &selected_acceptance_criteria,
            discussion_budgets,
        )?
    };

    let child = service
        .create_issue(IssueCreateSpec {
            team: Some(generated.parent.team.key.clone()),
            title: generated.child_title.clone(),
            description: Some(rendered_index_contents(&generated.files)?),
            project: None,
            project_id: generated
                .parent
                .project
                .as_ref()
                .map(|project| project.id.clone()),
            parent_id: Some(generated.parent.id.clone()),
            state: None,
            priority: generated.parent.priority,
            labels: vec![planning_meta.issue_labels.technical_label()],
        })
        .await?;

    let issue_dir = write_rendered_backlog_item(&root, &child.identifier, &generated.files)?;
    let download_failures =
        materialize_issue_context(&service, &issue_dir, &generated.prepared_context).await?;
    log_ticket_image_download_failures(&child.identifier, &download_failures);
    save_issue_metadata(
        &issue_dir,
        &BacklogIssueMetadata {
            issue_id: child.id.clone(),
            identifier: child.identifier.clone(),
            title: child.title.clone(),
            url: child.url.clone(),
            team_key: child.team.key.clone(),
            project_id: child.project.as_ref().map(|project| project.id.clone()),
            project_name: child.project.as_ref().map(|project| project.name.clone()),
            parent_id: Some(generated.parent.id.clone()),
            parent_identifier: Some(generated.parent.identifier.clone()),
            local_hash: None,
            remote_hash: None,
            last_sync_at: None,
            managed_files: Vec::<ManagedFileRecord>::new(),
        },
    )?;

    run_sync_push(
        &args.client,
        &SyncPushArgs {
            issue: Some(child.identifier.clone()),
            all: false,
            update_description: false,
        },
    )
    .await?;

    println!(
        "Created technical sub-issue {} under {} at {}.",
        child.identifier,
        generated.parent.identifier,
        display_path(&issue_dir, &root),
    );

    Ok(())
}

fn run_interactive_technical_session(
    root: &Path,
    initial_parent: Option<IssueSummary>,
    issues: Vec<IssueSummary>,
    discussion_budgets: TicketDiscussionBudgets,
) -> Result<InteractiveTechnicalExit> {
    let mut app = if let Some(parent) = initial_parent {
        let criteria = extract_acceptance_criteria(parent.description.as_deref());
        if criteria.is_empty() {
            let mut app = TechnicalSessionApp {
                stage: TechnicalStage::Loading(LoadingApp {
                    message: "Generating technical backlog".to_string(),
                    detail: format!(
                        "Building `.metastack/backlog/_TEMPLATE` for {}.",
                        parent.identifier
                    ),
                    spinner_index: 0,
                }),
                pending: None,
            };
            start_generation(
                &mut app,
                root,
                TechnicalGenerationRequest {
                    parent,
                    selected_acceptance_criteria: Vec::new(),
                    discussion_budgets,
                },
                None,
            );
            app
        } else {
            TechnicalSessionApp {
                stage: TechnicalStage::SelectCriteria(AcceptanceCriteriaApp {
                    parent,
                    criteria: MultiSelectFieldState::new(criteria.clone(), 0..criteria.len()),
                    error: None,
                }),
                pending: None,
            }
        }
    } else {
        TechnicalSessionApp {
            stage: TechnicalStage::PickIssue(IssuePickerApp {
                query: InputFieldState::default(),
                issues,
                selected: 0,
                error: None,
            }),
            pending: None,
        }
    };

    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        process_pending_generation(&mut app)?;
        advance_loading_spinner(&mut app);
        terminal.draw(|frame| render_technical_session(frame, &app))?;

        if event::poll(Duration::from_millis(if app.pending.is_some() {
            120
        } else {
            250
        }))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.code == KeyCode::Esc {
                        return Ok(InteractiveTechnicalExit::Cancelled);
                    }

                    if app.pending.is_some() {
                        continue;
                    }

                    let action = match &mut app.stage {
                        TechnicalStage::PickIssue(picker) => handle_issue_picker_key(picker, key),
                        TechnicalStage::SelectCriteria(criteria) => {
                            handle_acceptance_criteria_key(criteria, key, discussion_budgets)
                        }
                        TechnicalStage::Loading(_) => TechnicalAction::None,
                        TechnicalStage::Review(review) => handle_technical_review_key(review, key),
                    };

                    match action {
                        TechnicalAction::None => {}
                        TechnicalAction::SelectIssue(parent) => {
                            let criteria =
                                extract_acceptance_criteria(parent.description.as_deref());
                            if criteria.is_empty() {
                                let previous_stage = match &app.stage {
                                    TechnicalStage::PickIssue(picker) => {
                                        Some(TechnicalRecoveryStage::PickIssue(picker.clone()))
                                    }
                                    _ => None,
                                };
                                start_generation(
                                    &mut app,
                                    root,
                                    TechnicalGenerationRequest {
                                        parent,
                                        selected_acceptance_criteria: Vec::new(),
                                        discussion_budgets,
                                    },
                                    previous_stage,
                                );
                            } else {
                                app.stage = TechnicalStage::SelectCriteria(AcceptanceCriteriaApp {
                                    parent,
                                    criteria: MultiSelectFieldState::new(
                                        criteria.clone(),
                                        0..criteria.len(),
                                    ),
                                    error: None,
                                });
                            }
                        }
                        TechnicalAction::Generate(request) => {
                            let previous_stage = match &app.stage {
                                TechnicalStage::PickIssue(picker) => {
                                    Some(TechnicalRecoveryStage::PickIssue(picker.clone()))
                                }
                                TechnicalStage::SelectCriteria(criteria) => {
                                    Some(TechnicalRecoveryStage::SelectCriteria(criteria.clone()))
                                }
                                _ => None,
                            };
                            start_generation(&mut app, root, request, previous_stage);
                        }
                        TechnicalAction::Confirm(generated) => {
                            return Ok(InteractiveTechnicalExit::Confirmed(generated));
                        }
                    }
                }
                Event::Paste(text) => {
                    if let TechnicalStage::PickIssue(picker) = &mut app.stage {
                        handle_issue_picker_paste(picker, &text);
                    }
                }
                _ => {}
            }
        }
    }
}

fn handle_issue_picker_key(
    app: &mut IssuePickerApp,
    key: crossterm::event::KeyEvent,
) -> TechnicalAction {
    match key.code {
        KeyCode::Up => {
            let filtered = search_results(app);
            if filtered.is_empty() {
                app.selected = 0;
            } else if app.selected == 0 {
                app.selected = filtered.len().saturating_sub(1);
            } else {
                app.selected -= 1;
            }
            app.error = None;
            TechnicalAction::None
        }
        KeyCode::Down => {
            let filtered = search_results(app);
            if filtered.is_empty() {
                app.selected = 0;
            } else {
                app.selected = (app.selected + 1) % filtered.len();
            }
            app.error = None;
            TechnicalAction::None
        }
        KeyCode::Enter => {
            let filtered = search_results(app);
            let Some(issue_index) = filtered.get(app.selected).map(|result| result.issue_index)
            else {
                app.error = Some("No issues match the current search.".to_string());
                return TechnicalAction::None;
            };
            app.error = None;
            TechnicalAction::SelectIssue(app.issues[issue_index].clone())
        }
        _ => {
            if app.query.handle_key(key) {
                app.selected = 0;
                app.error = None;
            }
            TechnicalAction::None
        }
    }
}

fn handle_issue_picker_paste(app: &mut IssuePickerApp, text: &str) {
    if app.query.paste(text) {
        app.selected = 0;
        app.error = None;
    }
}

fn handle_acceptance_criteria_key(
    app: &mut AcceptanceCriteriaApp,
    key: crossterm::event::KeyEvent,
    discussion_budgets: TicketDiscussionBudgets,
) -> TechnicalAction {
    match key.code {
        KeyCode::Enter => {
            let selected_acceptance_criteria = app
                .criteria
                .selected_labels()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>();
            if selected_acceptance_criteria.is_empty() {
                app.error = Some(
                    "Select at least one acceptance criterion before generating the technical backlog."
                        .to_string(),
                );
                return TechnicalAction::None;
            }
            app.error = None;
            TechnicalAction::Generate(TechnicalGenerationRequest {
                parent: app.parent.clone(),
                selected_acceptance_criteria,
                discussion_budgets,
            })
        }
        _ => {
            if app.criteria.handle_key(key) {
                app.error = None;
            }
            TechnicalAction::None
        }
    }
}

fn handle_technical_review_key(
    app: &mut TechnicalReviewApp,
    key: crossterm::event::KeyEvent,
) -> TechnicalAction {
    match key.code {
        KeyCode::Up => {
            if app.selected_file == 0 {
                app.selected_file = app.generated.files.len().saturating_sub(1);
            } else {
                app.selected_file -= 1;
            }
            app.error = None;
            TechnicalAction::None
        }
        KeyCode::Down => {
            if !app.generated.files.is_empty() {
                app.selected_file = (app.selected_file + 1) % app.generated.files.len();
            }
            app.error = None;
            TechnicalAction::None
        }
        KeyCode::Enter => TechnicalAction::Confirm(app.generated.clone()),
        _ => TechnicalAction::None,
    }
}

fn start_generation(
    app: &mut TechnicalSessionApp,
    root: &Path,
    request: TechnicalGenerationRequest,
    previous_stage: Option<TechnicalRecoveryStage>,
) {
    app.stage = TechnicalStage::Loading(LoadingApp {
        message: "Generating technical backlog".to_string(),
        detail: format!(
            "Building `.metastack/backlog/_TEMPLATE` for {}.",
            request.parent.identifier
        ),
        spinner_index: 0,
    });
    app.pending = Some(PendingTechnicalJob {
        receiver: spawn_generation_job(root.to_path_buf(), request),
        previous_stage,
    });
}

fn spawn_generation_job(
    root: PathBuf,
    request: TechnicalGenerationRequest,
) -> Receiver<Result<TechnicalGeneratedBacklog>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = build_generated_backlog(
            &root,
            &request.parent,
            &request.selected_acceptance_criteria,
            request.discussion_budgets,
        );
        let _ = sender.send(result);
    });
    receiver
}

fn process_pending_generation(app: &mut TechnicalSessionApp) -> Result<()> {
    let Some(pending) = app.pending.as_ref() else {
        return Ok(());
    };

    match pending.receiver.try_recv() {
        Ok(result) => {
            let pending = app
                .pending
                .take()
                .ok_or_else(|| anyhow!("technical generation job disappeared unexpectedly"))?;
            match result {
                Ok(generated) => {
                    app.stage = TechnicalStage::Review(TechnicalReviewApp {
                        generated,
                        selected_file: 0,
                        error: None,
                    });
                }
                Err(error) => match pending.previous_stage {
                    Some(TechnicalRecoveryStage::PickIssue(mut picker)) => {
                        picker.error = Some(error.to_string());
                        app.stage = TechnicalStage::PickIssue(picker);
                    }
                    Some(TechnicalRecoveryStage::SelectCriteria(mut criteria)) => {
                        criteria.error = Some(error.to_string());
                        app.stage = TechnicalStage::SelectCriteria(criteria);
                    }
                    None => return Err(error),
                },
            }
        }
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => {
            let pending = app
                .pending
                .take()
                .ok_or_else(|| anyhow!("technical generation job disappeared unexpectedly"))?;
            match pending.previous_stage {
                Some(TechnicalRecoveryStage::PickIssue(mut picker)) => {
                    picker.error = Some(
                        "technical generation worker exited before returning a result".to_string(),
                    );
                    app.stage = TechnicalStage::PickIssue(picker);
                }
                Some(TechnicalRecoveryStage::SelectCriteria(mut criteria)) => {
                    criteria.error = Some(
                        "technical generation worker exited before returning a result".to_string(),
                    );
                    app.stage = TechnicalStage::SelectCriteria(criteria);
                }
                None => bail!("technical generation worker exited before returning a result"),
            }
        }
    }

    Ok(())
}

fn advance_loading_spinner(app: &mut TechnicalSessionApp) {
    if let TechnicalStage::Loading(loading) = &mut app.stage {
        loading.spinner_index = (loading.spinner_index + 1) % SPINNER_FRAMES.len();
    }
}

fn build_generated_backlog(
    root: &Path,
    parent: &IssueSummary,
    selected_acceptance_criteria: &[String],
    discussion_budgets: TicketDiscussionBudgets,
) -> Result<TechnicalGeneratedBacklog> {
    let prepared_context = prepare_issue_context(parent, discussion_budgets);
    let child_title = format!("Technical: {}", parent.title);
    let template_files = render_template_files(
        root,
        &TemplateContext {
            issue_title: Some(child_title.clone()),
            parent_identifier: Some(parent.identifier.clone()),
            parent_title: Some(parent.title.clone()),
            parent_url: Some(parent.url.clone()),
            parent_description: prepared_context.issue.description.clone(),
            ..TemplateContext::default()
        },
    )?;
    let files = generate_backlog_files(
        root,
        &prepared_context,
        &child_title,
        selected_acceptance_criteria,
        &template_files,
    )?;
    Ok(TechnicalGeneratedBacklog {
        parent: parent.clone(),
        child_title,
        selected_acceptance_criteria: selected_acceptance_criteria.to_vec(),
        prepared_context,
        files,
    })
}

fn rendered_index_contents(rendered_files: &[RenderedTemplateFile]) -> Result<String> {
    rendered_files
        .iter()
        .find(|file| file.relative_path == INDEX_FILE_NAME)
        .map(|file| file.contents.clone())
        .ok_or_else(|| anyhow!("the technical backlog template must contain `{INDEX_FILE_NAME}`"))
}

fn generate_backlog_files(
    root: &Path,
    prepared_context: &PreparedIssueContext,
    child_title: &str,
    selected_acceptance_criteria: &[String],
    template_files: &[RenderedTemplateFile],
) -> Result<Vec<RenderedTemplateFile>> {
    let prompt = render_technical_prompt(
        root,
        prepared_context,
        child_title,
        selected_acceptance_criteria,
        &slugify(child_title),
        &current_local_date()?,
        template_files,
    )?;
    let output = run_agent_capture(&RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_BACKLOG_SPLIT.to_string()),
        agent: None,
        prompt,
        instructions: None,
        model: None,
        reasoning: None,
        transport: None,
        attachments: Vec::new(),
    })
    .with_context(|| {
        "meta backlog tech requires a configured local agent to generate backlog content from `.metastack/backlog/_TEMPLATE`"
    })?;
    let draft: TechnicalBacklogDraft =
        parse_agent_json(&output.stdout, "technical backlog generation")?;

    validate_generated_files(draft.files, template_files)
}

fn render_technical_prompt(
    root: &Path,
    prepared_context: &PreparedIssueContext,
    child_title: &str,
    selected_acceptance_criteria: &[String],
    backlog_slug: &str,
    today: &str,
    template_files: &[RenderedTemplateFile],
) -> Result<String> {
    let context = load_context_bundle(root)?;
    let workflow_contract = load_workflow_contract(root)?;
    let repository_snapshot = render_repository_snapshot(root)?;
    let acceptance_criteria_block = if selected_acceptance_criteria.is_empty() {
        "_No acceptance criteria were selected for this technical sub-ticket._".to_string()
    } else {
        selected_acceptance_criteria
            .iter()
            .map(|criterion| format!("- {criterion}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let template_block = template_files
        .iter()
        .map(|file| {
            format!(
                "### `{}`\n```md\n{}\n```",
                file.relative_path, file.contents
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let parent = &prepared_context.issue;
    let parent_description_block = parent
        .description
        .as_deref()
        .unwrap_or("_No Linear description was provided._");
    let parent_context_block = parent
        .parent
        .as_ref()
        .and_then(|issue| issue.description.as_deref())
        .unwrap_or("_No parent description was provided._");
    let discussion_block = if prepared_context.prompt_discussion.trim().is_empty() {
        "_No Linear comments were provided._".to_string()
    } else {
        prepared_context.prompt_discussion.clone()
    };
    let image_summary = render_ticket_image_summary(&prepared_context.images);

    Ok(format!(
        "You are generating a technical backlog item for the active repository.\n\n\
Injected workflow contract:\n{workflow_contract}\n\n\
Parent Linear issue:\n\
- Identifier: `{}`\n\
- Title: {}\n\
- State: {}\n\
- URL: {}\n\
- Description:\n{}\n\n\
Parent issue context:\n{}\n\n\
Ticket discussion context:\n{}\n\n\
Localized ticket images:\n{}\n\n\
Derived backlog values:\n\
- `BACKLOG_TITLE`: {}\n\
- `BACKLOG_SLUG`: {}\n\
- `TODAY`: {}\n\n\
Selected acceptance criteria for this technical sub-ticket:\n\
{}\n\n\
Repository planning context:\n{}\n\n\
Repository directory snapshot:\n{}\n\n\
Template files to convert into a concrete backlog item:\n{}\n\n\
Instructions:\n\
1. Produce concrete Markdown content for every listed template file.\n\
2. Preserve the file paths exactly as provided.\n\
3. Use the template structure as guidance, but replace placeholder prose with issue-specific, repo-specific content for the target repository only.\n\
4. Do not leave unresolved placeholders such as `{{BACKLOG_TITLE}}`, `{{BACKLOG_SLUG}}`, `{{TODAY}}`, `{{issue_title}}`, or `{{parent_identifier}}`.\n\
5. Keep links relative to the file that contains them.\n\
6. Default scope to the full repository root unless the user explicitly requested a narrower subproject, and create backlog content only for work inside this repository directory.\n\
7. Return JSON only with this exact shape:\n\
{{\"files\":[{{\"path\":\"index.md\",\"contents\":\"# ...\"}}]}}",
        parent.identifier,
        parent.title,
        parent
            .state
            .as_ref()
            .map(|state| state.name.as_str())
            .unwrap_or("Unknown"),
        parent.url,
        parent_description_block,
        parent_context_block,
        discussion_block,
        image_summary,
        child_title,
        backlog_slug,
        today,
        acceptance_criteria_block,
        context,
        repository_snapshot,
        template_block,
    ))
}

fn resolve_ticket_discussion_budgets(
    planning_meta: &crate::config::PlanningMeta,
) -> TicketDiscussionBudgets {
    TicketDiscussionBudgets {
        prompt_chars: planning_meta
            .linear
            .ticket_context
            .discussion_prompt_chars
            .unwrap_or(TicketDiscussionBudgets::default().prompt_chars),
        persisted_chars: planning_meta
            .linear
            .ticket_context
            .discussion_persisted_chars
            .unwrap_or(TicketDiscussionBudgets::default().persisted_chars),
    }
}

fn log_ticket_image_download_failures(
    identifier: &str,
    failures: &[crate::linear::TicketImageDownloadFailure],
) {
    for failure in failures {
        eprintln!(
            "warning: failed to localize ticket image for {identifier}: {} from {} ({})",
            failure.filename, failure.source_label, failure.error
        );
    }
}

fn validate_generated_files(
    generated_files: Vec<TechnicalBacklogFile>,
    template_files: &[RenderedTemplateFile],
) -> Result<Vec<RenderedTemplateFile>> {
    let expected_paths = template_files
        .iter()
        .map(|file| file.relative_path.clone())
        .collect::<BTreeSet<_>>();
    let mut actual_files = BTreeMap::new();

    for file in generated_files {
        let path = file.path.trim().replace('\\', "/");
        if path.is_empty() {
            bail!("technical backlog agent returned a file entry without a path");
        }
        if actual_files
            .insert(path.clone(), file.contents.replace("\r\n", "\n"))
            .is_some()
        {
            bail!("technical backlog agent returned duplicate file `{path}`");
        }
    }

    let actual_paths = actual_files.keys().cloned().collect::<BTreeSet<_>>();
    if actual_paths != expected_paths {
        let missing = expected_paths
            .difference(&actual_paths)
            .cloned()
            .collect::<Vec<_>>();
        let extra = actual_paths
            .difference(&expected_paths)
            .cloned()
            .collect::<Vec<_>>();
        bail!(
            "technical backlog agent returned the wrong file set (missing: {}; extra: {})",
            format_path_list(&missing),
            format_path_list(&extra),
        );
    }

    let rendered_files = template_files
        .iter()
        .map(|template| {
            let contents = actual_files
                .remove(&template.relative_path)
                .ok_or_else(|| {
                    anyhow!(
                        "technical backlog agent omitted `{}`",
                        template.relative_path
                    )
                })?;

            if contents.trim().is_empty() {
                bail!(
                    "technical backlog agent returned empty contents for `{}`",
                    template.relative_path
                );
            }

            Ok(RenderedTemplateFile {
                relative_path: template.relative_path.clone(),
                contents,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure_no_unresolved_placeholders(&rendered_files)?;
    Ok(rendered_files)
}

fn render_technical_session(frame: &mut Frame<'_>, app: &TechnicalSessionApp) {
    match &app.stage {
        TechnicalStage::PickIssue(picker) => render_issue_picker_frame(frame, picker),
        TechnicalStage::SelectCriteria(criteria) => {
            render_acceptance_criteria_frame(frame, criteria)
        }
        TechnicalStage::Loading(loading) => render_loading_frame(frame, loading),
        TechnicalStage::Review(review) => render_review_frame(frame, review),
    }
}

fn render_issue_picker_frame(frame: &mut Frame<'_>, app: &IssuePickerApp) {
    let layout = base_layout(frame);
    let body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(layout[0]);
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(body[1]);

    let query_block = Block::default()
        .borders(Borders::ALL)
        .title("Select Parent Issue [search]")
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let query_inner = query_block.inner(body[0]);
    let rendered_query = app.query.render_with_width(
        "Search by identifier, title, state, project, or description...",
        true,
        query_inner.width,
    );
    let query = Paragraph::new(rendered_query.text.clone())
        .block(query_block)
        .wrap(Wrap { trim: false });
    frame.render_widget(query, body[0]);
    rendered_query.set_cursor(frame, query_inner);

    let filtered = search_results(app);
    let mut issue_state = ListState::default();
    issue_state.select(Some(app.selected.min(filtered.len().saturating_sub(1))));
    let issue_items = if filtered.is_empty() {
        vec![ListItem::new("No issues match the current search.")]
    } else {
        filtered
            .iter()
            .filter_map(|result| {
                app.issues
                    .get(result.issue_index)
                    .map(|issue| render_issue_row(issue, Some(result), None))
            })
            .collect::<Vec<_>>()
    };
    let issue_list = List::new(issue_items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            "Issues ({}/{})",
            filtered.len(),
            app.issues.len()
        )))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(issue_list, content[0], &mut issue_state);

    let preview = filtered
        .get(app.selected)
        .and_then(|result| {
            app.issues.get(result.issue_index).map(|issue| {
                render_issue_preview(
                    issue,
                    Some(result),
                    None,
                    "_No Linear description was provided._",
                )
            })
        })
        .unwrap_or_else(|| {
            Text::from(vec![
                Line::from("Search results appear here."),
                Line::from(""),
                Line::styled(
                    "Type to narrow the ticket list, then press Enter to generate the technical backlog draft.",
                    Style::default().add_modifier(Modifier::DIM),
                ),
            ])
        });
    let preview = Paragraph::new(preview)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Issue Preview"),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(preview, content[1]);

    render_footer(
        frame,
        layout[1],
        app.error.as_deref(),
        "Type to search issues by identifier, title, state, project, or description. Up/Down moves the selection. Enter generates the technical backlog draft. Esc cancels.",
    );
}

fn render_acceptance_criteria_frame(frame: &mut Frame<'_>, app: &AcceptanceCriteriaApp) {
    let layout = base_layout(frame);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(layout[0]);

    let selected = app.criteria.selected_indices();
    let mut criteria_state = ListState::default();
    criteria_state.select(Some(
        app.criteria
            .cursor()
            .min(app.criteria.options().len().saturating_sub(1)),
    ));
    let criteria_items = if app.criteria.options().is_empty() {
        vec![ListItem::new(
            "No acceptance criteria were found in the issue description.",
        )]
    } else {
        app.criteria
            .options()
            .iter()
            .enumerate()
            .map(|(index, criterion)| {
                let marker = if selected.contains(&index) {
                    "[x]"
                } else {
                    "[ ]"
                };
                ListItem::new(format!("{marker} {criterion}"))
            })
            .collect::<Vec<_>>()
    };
    let criteria_list = List::new(criteria_items)
        .block(Block::default().borders(Borders::ALL).title(format!(
            "Acceptance Criteria ({}/{})",
            selected.len(),
            app.criteria.options().len()
        )))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(criteria_list, body[0], &mut criteria_state);

    let mut summary_lines = vec![
        Line::from(format!("Parent: {}", app.parent.identifier)),
        Line::from(app.parent.title.clone()),
        Line::from(""),
        Line::styled(
            "Selected criteria will be carried into the technical prompt alongside the repository scan and planning context.",
            Style::default().add_modifier(Modifier::DIM),
        ),
        Line::from(""),
        Line::from("Selected"),
    ];

    if selected.is_empty() {
        summary_lines.push(Line::styled(
            "_No acceptance criteria selected yet._",
            Style::default().add_modifier(Modifier::DIM),
        ));
    } else {
        for criterion in app.criteria.selected_labels() {
            summary_lines.push(Line::from(format!("- {criterion}")));
        }
    }

    let summary = Paragraph::new(Text::from(summary_lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Selection Summary"),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(summary, body[1]);

    render_footer(
        frame,
        layout[1],
        app.error.as_deref(),
        "Up/Down moves between acceptance criteria. Space toggles each criterion. Enter generates the technical backlog draft from the selected criteria. Esc cancels.",
    );
}

fn render_review_frame(frame: &mut Frame<'_>, app: &TechnicalReviewApp) {
    let layout = base_layout(frame);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(layout[0]);
    let sidebar = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(14), Constraint::Min(0)])
        .split(body[0]);

    let criteria_summary = if app.generated.selected_acceptance_criteria.is_empty() {
        "Criteria: using the full issue context".to_string()
    } else {
        format!(
            "Criteria: {} selected",
            app.generated.selected_acceptance_criteria.len()
        )
    };

    let summary = Paragraph::new(Text::from(vec![
        Line::from(format!("Parent: {}", app.generated.parent.identifier)),
        Line::from(app.generated.parent.title.clone()),
        Line::from(""),
        Line::from(format!("Child: {}", app.generated.child_title)),
        Line::from(format!("Files: {}", app.generated.files.len())),
        Line::from(criteria_summary),
        Line::from(""),
        Line::styled(
            "Review every generated Markdown file before creating the technical child issue.",
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Technical Draft"),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(summary, sidebar[0]);

    let mut file_state = ListState::default();
    file_state.select(Some(
        app.selected_file
            .min(app.generated.files.len().saturating_sub(1)),
    ));
    let file_items = app
        .generated
        .files
        .iter()
        .map(|file| ListItem::new(file.relative_path.clone()))
        .collect::<Vec<_>>();
    let file_list = List::new(file_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Generated Files"),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(file_list, sidebar[1], &mut file_state);

    let selected_file = &app.generated.files[app.selected_file];
    let preview = Paragraph::new(selected_file.contents.clone())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Preview: {}", selected_file.relative_path)),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(preview, body[1]);

    render_footer(
        frame,
        layout[1],
        app.error.as_deref(),
        "Up/Down moves between generated files. Enter creates the technical child issue and syncs the reviewed Markdown files. Esc cancels.",
    );
}

fn render_loading_frame(frame: &mut Frame<'_>, app: &LoadingApp) {
    render_loading_panel(
        frame,
        frame.area(),
        &LoadingPanelData {
            title: "Agent Working [loading]".to_string(),
            message: app.message.clone(),
            detail: app.detail.clone(),
            spinner_index: app.spinner_index,
            status_line:
                "State: loading. The dashboard advances automatically when the agent responds."
                    .to_string(),
        },
    );
}

fn search_results(app: &IssuePickerApp) -> Vec<IssueSearchResult> {
    search_issues(&app.issues, app.query.value().trim())
}

fn base_layout(frame: &mut Frame<'_>) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(4)])
        .split(frame.area())
        .to_vec()
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, error: Option<&str>, help: &str) {
    let mut lines = vec![Line::from(help.to_string())];
    if let Some(error) = error {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("Error: {error}"),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    let footer = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL).title("Controls"))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, area);
}

fn current_local_date() -> Result<String> {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    OffsetDateTime::now_utc()
        .to_offset(offset)
        .format(&format_description!("[year]-[month]-[day]"))
        .context("failed to format the current date for the technical backlog prompt")
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "technical-item".to_string()
    } else {
        slug
    }
}

fn load_context_bundle(root: &Path) -> Result<String> {
    let paths = PlanningPaths::new(root);
    let sections = [
        ("SCAN.md", paths.scan_path()),
        ("ARCHITECTURE.md", paths.architecture_path()),
        ("CONCERNS.md", paths.concerns_path()),
        ("CONVENTIONS.md", paths.conventions_path()),
        ("INTEGRATIONS.md", paths.integrations_path()),
        ("STACK.md", paths.stack_path()),
        ("STRUCTURE.md", paths.structure_path()),
        ("TESTING.md", paths.testing_path()),
    ];
    let mut lines = Vec::new();

    for (title, path) in sections {
        lines.push(format!("## {title}"));
        lines.push(String::new());
        lines.push(read_context(&path)?);
        lines.push(String::new());
    }

    Ok(lines.join("\n"))
}

fn render_repository_snapshot(root: &Path) -> Result<String> {
    let mut lines = Vec::new();
    let mut remaining = 80usize;
    collect_directory_snapshot(root, root, 0, 2, &mut remaining, &mut lines)?;

    if lines.is_empty() {
        Ok("_Repository snapshot is empty._".to_string())
    } else if remaining == 0 {
        lines.push("... (truncated)".to_string());
        Ok(lines.join("\n"))
    } else {
        Ok(lines.join("\n"))
    }
}

fn collect_directory_snapshot(
    root: &Path,
    current: &Path,
    depth: usize,
    max_depth: usize,
    remaining: &mut usize,
    lines: &mut Vec<String>,
) -> Result<()> {
    if *remaining == 0 || depth > max_depth {
        return Ok(());
    }

    let mut entries = fs::read_dir(current)
        .with_context(|| format!("failed to read directory `{}`", current.display()))?
        .filter_map(|entry| entry.ok())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if *remaining == 0 {
            break;
        }

        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if should_skip_snapshot_entry(&file_name) {
            continue;
        }

        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect `{}`", path.display()))?;
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let indent = "  ".repeat(depth);
        let display = if file_type.is_dir() {
            format!("{indent}- {}/", relative.display())
        } else {
            format!("{indent}- {}", relative.display())
        };
        lines.push(display);
        *remaining = remaining.saturating_sub(1);

        if file_type.is_dir() {
            collect_directory_snapshot(root, &path, depth + 1, max_depth, remaining, lines)?;
        }
    }

    Ok(())
}

fn should_skip_snapshot_entry(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | ".next" | "dist" | "build" | "coverage"
    )
}

fn read_context(path: &PathBuf) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(format!(
            "_Missing `{}`. Run `meta scan` to generate it._",
            path.file_name()
                .map(|value| value.to_string_lossy())
                .unwrap_or_default()
        )),
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

fn parse_agent_json<T>(raw: &str, phase: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let trimmed = raw.trim();
    let mut candidates = vec![trimmed.to_string()];

    if let Some(stripped) = strip_code_fence(trimmed) {
        candidates.push(stripped);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && start <= end
    {
        candidates.push(trimmed[start..=end].to_string());
    }

    for candidate in candidates {
        if let Ok(parsed) = serde_json::from_str::<T>(&candidate) {
            return Ok(parsed);
        }
    }

    bail!(
        "technical backlog agent returned invalid JSON during {phase}: {}",
        preview_text(trimmed)
    )
}

fn strip_code_fence(raw: &str) -> Option<String> {
    let stripped = raw.strip_prefix("```")?;
    let stripped = stripped
        .strip_prefix("json\n")
        .or_else(|| stripped.strip_prefix("JSON\n"))
        .or_else(|| stripped.strip_prefix('\n'))
        .unwrap_or(stripped);
    let stripped = stripped.strip_suffix("```")?;
    Some(stripped.trim().to_string())
}

fn preview_text(value: &str) -> String {
    const MAX_PREVIEW_LEN: usize = 240;
    if value.len() <= MAX_PREVIEW_LEN {
        value.to_string()
    } else {
        format!("{}...", &value[..MAX_PREVIEW_LEN])
    }
}

fn format_path_list(paths: &[String]) -> String {
    if paths.is_empty() {
        "none".to_string()
    } else {
        paths.join(", ")
    }
}

fn extract_acceptance_criteria(description: Option<&str>) -> Vec<String> {
    let Some(description) = description else {
        return Vec::new();
    };

    let mut in_acceptance_criteria = false;
    let mut current_item = None::<String>;
    let mut items = Vec::new();

    for line in description.lines() {
        let trimmed = line.trim();

        if is_markdown_header(trimmed) {
            let is_acceptance_header = header_title(trimmed)
                .map(|title| {
                    title
                        .trim_end_matches(':')
                        .eq_ignore_ascii_case("acceptance criteria")
                })
                .unwrap_or(false);

            if in_acceptance_criteria && !is_acceptance_header {
                if let Some(item) = current_item.take()
                    && !item.trim().is_empty()
                {
                    items.push(item);
                }
                break;
            }

            in_acceptance_criteria = is_acceptance_header;
            continue;
        }

        if !in_acceptance_criteria {
            continue;
        }

        if let Some(item) = parse_markdown_list_item(trimmed) {
            if let Some(previous) = current_item.replace(item)
                && !previous.trim().is_empty()
            {
                items.push(previous);
            }
            continue;
        }

        if trimmed.is_empty() {
            if let Some(previous) = current_item.take()
                && !previous.trim().is_empty()
            {
                items.push(previous);
            }
            continue;
        }

        if let Some(existing) = current_item.as_mut() {
            existing.push(' ');
            existing.push_str(trimmed);
        }
    }

    if let Some(item) = current_item
        && !item.trim().is_empty()
    {
        items.push(item);
    }

    items
}

fn is_markdown_header(line: &str) -> bool {
    line.starts_with('#')
}

fn header_title(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches('#').trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn parse_markdown_list_item(line: &str) -> Option<String> {
    let stripped = line.trim_start();
    for prefix in ["- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "* [X] "] {
        if let Some(rest) = stripped.strip_prefix(prefix) {
            return normalized_list_item(rest);
        }
    }

    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = stripped.strip_prefix(prefix) {
            return normalized_list_item(rest);
        }
    }

    let digit_count = stripped
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .count();
    if digit_count > 0 {
        let remainder = &stripped[digit_count..];
        if let Some(rest) = remainder
            .strip_prefix(". ")
            .or_else(|| remainder.strip_prefix(") "))
        {
            return normalized_list_item(rest);
        }
    }

    None
}

fn normalized_list_item(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen);
    }
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
    use super::{
        AcceptanceCriteriaApp, IssuePickerApp, LoadingApp, TechnicalGeneratedBacklog,
        TechnicalReviewApp, extract_acceptance_criteria, handle_issue_picker_paste,
        render_acceptance_criteria_frame, render_issue_picker_frame, render_loading_frame,
        render_review_frame, render_technical_prompt, search_results, snapshot,
    };
    use crate::backlog::RenderedTemplateFile;
    use crate::fs::PlanningPaths;
    use crate::linear::{
        IssueSummary, ProjectRef, TeamRef, TicketDiscussionBudgets, WorkflowState,
        prepare_issue_context,
    };
    use crate::tui::fields::{InputFieldState, MultiSelectFieldState};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::fs;
    use tempfile::tempdir;

    fn issue(identifier: &str, title: &str, description: &str) -> IssueSummary {
        IssueSummary {
            id: format!("id-{identifier}"),
            identifier: identifier.to_string(),
            title: title.to_string(),
            description: Some(description.to_string()),
            url: format!("https://linear.app/{identifier}"),
            priority: Some(2),
            estimate: None,
            updated_at: "2026-03-14T12:00:00Z".to_string(),
            team: TeamRef {
                id: "team-1".to_string(),
                key: "MET".to_string(),
                name: "Metastack".to_string(),
            },
            project: Some(ProjectRef {
                id: "project-1".to_string(),
                name: "MetaStack CLI".to_string(),
            }),
            assignee: None,
            labels: Vec::new(),
            comments: Vec::new(),
            state: Some(WorkflowState {
                id: "state-1".to_string(),
                name: "Todo".to_string(),
                kind: Some("unstarted".to_string()),
            }),
            attachments: Vec::new(),
            parent: None,
            children: Vec::new(),
        }
    }

    fn render_picker_snapshot(app: &IssuePickerApp) -> String {
        let backend = TestBackend::new(140, 36);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_issue_picker_frame(frame, app))
            .expect("picker should render");
        snapshot(terminal.backend())
    }

    fn render_review_snapshot(app: &TechnicalReviewApp) -> String {
        let backend = TestBackend::new(140, 36);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_review_frame(frame, app))
            .expect("review should render");
        snapshot(terminal.backend())
    }

    fn render_criteria_snapshot(app: &AcceptanceCriteriaApp) -> String {
        let backend = TestBackend::new(140, 36);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_acceptance_criteria_frame(frame, app))
            .expect("criteria selector should render");
        snapshot(terminal.backend())
    }

    fn render_loading_snapshot(app: &LoadingApp) -> String {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_loading_frame(frame, app))
            .expect("loading should render");
        snapshot(terminal.backend())
    }

    #[test]
    fn picker_search_prefers_identifier_and_title_matches() {
        let picker = IssuePickerApp {
            query: InputFieldState::new("met-42 terminal"),
            issues: vec![
                issue("MET-12", "Cleanup docs", "Documentation cleanup"),
                issue("MET-42", "Terminal experience", "Improve terminal flow"),
            ],
            selected: 0,
            error: None,
        };

        let filtered = search_results(&picker);
        assert_eq!(
            filtered
                .iter()
                .map(|result| result.issue_index)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn issue_picker_snapshot_shows_search_and_preview() {
        let snapshot = render_picker_snapshot(&IssuePickerApp {
            query: InputFieldState::new("terminal"),
            issues: vec![
                issue(
                    "MET-42",
                    "Terminal experience",
                    "Improve the terminal planning flow.",
                ),
                issue("MET-43", "Sync polish", "Refine sync previews."),
            ],
            selected: 0,
            error: None,
        });

        assert!(snapshot.contains("Select Parent Issue [search]"));
        assert!(snapshot.contains("MET-42  Terminal experience"));
        assert!(snapshot.contains("Issue Preview"));
    }

    #[test]
    fn issue_picker_paste_updates_the_search_query() {
        let mut app = IssuePickerApp {
            query: InputFieldState::new("tech"),
            issues: vec![issue(
                "MET-42",
                "Terminal experience",
                "Improve planning flow.",
            )],
            selected: 1,
            error: Some("stale".to_string()),
        };

        handle_issue_picker_paste(&mut app, " backlog\n generator\n");

        assert_eq!(app.query.value(), "tech backlog generator");
        assert_eq!(app.selected, 0);
        assert_eq!(app.error, None);
    }

    #[test]
    fn review_snapshot_lists_generated_files_and_preview() {
        let snapshot = render_review_snapshot(&TechnicalReviewApp {
            generated: TechnicalGeneratedBacklog {
                parent: issue(
                    "MET-35",
                    "Create the technical command",
                    "Parent description",
                ),
                child_title: "Technical: Create the technical command".to_string(),
                selected_acceptance_criteria: vec![
                    "The command generates backlog docs".to_string(),
                    "The docs stay in sync".to_string(),
                ],
                prepared_context: prepare_issue_context(
                    &issue(
                        "MET-35",
                        "Create the technical command",
                        "Parent description",
                    ),
                    TicketDiscussionBudgets::default(),
                ),
                files: vec![
                    RenderedTemplateFile {
                        relative_path: "index.md".to_string(),
                        contents: "# Technical draft".to_string(),
                    },
                    RenderedTemplateFile {
                        relative_path: "specification.md".to_string(),
                        contents: "# Specification".to_string(),
                    },
                ],
            },
            selected_file: 1,
            error: None,
        });

        assert!(snapshot.contains("Technical Draft"));
        assert!(snapshot.contains("Generated Files"));
        assert!(snapshot.contains("Preview: specification.md"));
        assert!(snapshot.contains("Criteria: 2 selected"));
    }

    #[test]
    fn loading_snapshot_matches_plan_style() {
        let snapshot = render_loading_snapshot(&LoadingApp {
            message: "Generating technical backlog".to_string(),
            detail: "Building `.metastack/backlog/_TEMPLATE` for MET-35.".to_string(),
            spinner_index: 2,
        });

        assert!(snapshot.contains("- Generating technical backlog"));
        assert!(snapshot.contains("Agent Working [loading]"));
    }

    #[test]
    fn issue_picker_snapshot_shows_zero_results_state() {
        let snapshot = render_picker_snapshot(&IssuePickerApp {
            query: InputFieldState::new("zzz"),
            issues: vec![issue(
                "MET-42",
                "Terminal experience",
                "Improve planning flow.",
            )],
            selected: 0,
            error: None,
        });

        assert!(snapshot.contains("No issues match the current search."));
        assert!(snapshot.contains("Search results appear here."));
    }

    #[test]
    fn acceptance_criteria_parser_collects_markdown_list_items() {
        let description = r#"
# Context
Some setup.

## Acceptance Criteria
- [ ] Script exists in the repository root
- [x] Script exits cleanly on interruption
1. Usage is documented
   with a wrapped continuation line

## Notes
Ignored.
"#;

        assert_eq!(
            extract_acceptance_criteria(Some(description)),
            vec![
                "Script exists in the repository root".to_string(),
                "Script exits cleanly on interruption".to_string(),
                "Usage is documented with a wrapped continuation line".to_string(),
            ]
        );
    }

    #[test]
    fn acceptance_criteria_selector_snapshot_shows_selected_items() {
        let snapshot = render_criteria_snapshot(&AcceptanceCriteriaApp {
            parent: issue(
                "MET-56",
                "Create a Merry Christmas script",
                "## Acceptance Criteria\n- festive scene\n- graceful exit",
            ),
            criteria: MultiSelectFieldState::new(
                vec!["festive scene".to_string(), "graceful exit".to_string()],
                [0usize],
            ),
            error: None,
        });

        assert!(snapshot.contains("Acceptance Criteria (1/2)"));
        assert!(snapshot.contains("[x] festive scene"));
        assert!(snapshot.contains("Selection Summary"));
    }

    #[test]
    fn technical_prompt_includes_selected_criteria_and_repo_snapshot() {
        let temp = tempdir().expect("tempdir should be created");
        let root = temp.path();
        let paths = PlanningPaths::new(root);
        fs::create_dir_all(&paths.codebase_dir).expect("codebase dir should be created");
        fs::create_dir_all(root.join("src")).expect("src dir should be created");
        fs::write(paths.scan_path(), "# Scan\nCLI layout").expect("scan context should be written");
        fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("repo file should be written");
        let prepared_context = prepare_issue_context(
            &issue(
                "MET-35",
                "Create the technical command",
                "## Acceptance Criteria\n- Render docs\n- Keep sync safe",
            ),
            TicketDiscussionBudgets::default(),
        );

        let prompt = render_technical_prompt(
            root,
            &prepared_context,
            "Technical: Create the technical command",
            &["Render docs".to_string(), "Keep sync safe".to_string()],
            "technical-create-the-technical-command",
            "2026-03-14",
            &[RenderedTemplateFile {
                relative_path: "index.md".to_string(),
                contents: "# {{BACKLOG_TITLE}}".to_string(),
            }],
        )
        .expect("prompt should render");

        assert!(prompt.contains("Selected acceptance criteria for this technical sub-ticket"));
        assert!(prompt.contains("- Render docs"));
        assert!(prompt.contains("Injected workflow contract:"));
        assert!(prompt.contains("## Built-in Workflow Contract"));
        assert!(
            prompt
                .contains("create backlog content only for work inside this repository directory")
        );
        assert!(prompt.contains("Repository directory snapshot"));
        assert!(prompt.contains("- src/"));
        assert!(prompt.contains("- src/main.rs"));
        assert!(prompt.contains("## SCAN.md"));
        assert!(!prompt.contains("MetaStack CLI"));
    }
}

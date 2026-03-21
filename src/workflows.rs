use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::io::IsTerminal;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::agents::{
    render_invocation_diagnostics, resolve_agent_invocation_for_planning, run_agent_capture,
};
use crate::cli::{
    LinearClientArgs, RunAgentArgs, WorkflowCommands, WorkflowRunArgs, WorkflowRunEventArg,
    WorkflowsArgs,
};
use crate::config::{
    AGENT_ROUTE_AGENTS_WORKFLOWS_RUN, AppConfig, PlanningMeta, is_no_agent_selected_error,
};
use crate::context::{
    load_codebase_context_bundle, load_effective_instructions, load_project_rules_bundle,
    load_workflow_contract, render_repo_map,
};
use crate::fs::{
    FileWriteStatus, PlanningPaths, canonicalize_existing_dir, display_path, write_text_file,
};
use crate::linear::IssueSummary;
use crate::load_linear_command_context;
use crate::tui::fields::InputFieldState;
use crate::tui::scroll::{ScrollState, scrollable_paragraph, wrapped_rows};

const BUILTIN_WORKFLOWS: [(&str, &str); 4] = [
    (
        "builtin/backlog-planning.md",
        include_str!("artifacts/workflows/backlog-planning.md"),
    ),
    (
        "builtin/ticket-implementation.md",
        include_str!("artifacts/workflows/ticket-implementation.md"),
    ),
    (
        "builtin/pr-review.md",
        include_str!("artifacts/workflows/pr-review.md"),
    ),
    (
        "builtin/incident-triage.md",
        include_str!("artifacts/workflows/incident-triage.md"),
    ),
];

#[derive(Debug, Clone, Deserialize)]
struct WorkflowFrontMatter {
    name: String,
    summary: String,
    provider: String,
    #[serde(default)]
    parameters: Vec<WorkflowParameter>,
    #[serde(default)]
    validation: Vec<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    linear_issue_parameter: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkflowParameter {
    name: String,
    description: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    default: Option<String>,
}

#[derive(Debug, Clone)]
enum WorkflowSource {
    Builtin(&'static str),
    Local(PathBuf),
}

#[derive(Debug, Clone)]
struct WorkflowPlaybook {
    name: String,
    summary: String,
    provider: String,
    parameters: Vec<WorkflowParameter>,
    validation: Vec<String>,
    instructions_template: Option<String>,
    prompt_template: String,
    linear_issue_parameter: Option<String>,
    source: WorkflowSource,
}

#[derive(Debug, Default)]
struct WorkflowLibrary {
    workflows: BTreeMap<String, WorkflowPlaybook>,
}

#[derive(Debug, Clone)]
struct PreparedWorkflowRun {
    workflow: WorkflowPlaybook,
    values: BTreeMap<String, String>,
    instructions: Option<String>,
    prompt: String,
    run_args: RunAgentArgs,
    provider: String,
    diagnostics: Vec<String>,
}

#[derive(Debug, Clone)]
struct WorkflowArtifact {
    markdown: String,
    provider: String,
    diagnostics: Vec<String>,
    default_output: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowRunScreen {
    Wizard,
    Review,
    Edit,
    SavePath,
    ConfirmOverwrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewFocus {
    Artifact,
    Details,
}

#[derive(Debug, Clone)]
struct WorkflowParameterField {
    parameter: WorkflowParameter,
    input: InputFieldState,
}

#[derive(Debug, Clone)]
struct WorkflowRunApp {
    repo_root: PathBuf,
    workflow: WorkflowPlaybook,
    fields: Vec<WorkflowParameterField>,
    screen: WorkflowRunScreen,
    step_index: usize,
    review_focus: ReviewFocus,
    artifact_scroll: ScrollState,
    detail_scroll: ScrollState,
    save_path_input: InputFieldState,
    markdown_editor: InputFieldState,
    artifact: Option<WorkflowArtifact>,
    overwrite: bool,
    preferred_output: Option<PathBuf>,
    save_message: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowUiCommand {
    None,
    Cancel,
    Generate,
    Save { overwrite: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SaveDecision {
    Created,
    Updated,
    Unchanged,
    NeedsOverwrite,
}

/// List, explain, or run reusable workflow playbooks.
///
/// Returns rendered text for the selected command mode. Errors when the repository root is
/// missing, a playbook cannot be parsed, required parameters are absent, generation fails, or
/// the requested output path cannot be resolved or written safely.
pub async fn run_workflows(args: &WorkflowsArgs) -> Result<String> {
    match &args.command {
        WorkflowCommands::List(list_args) => {
            let root = canonicalize_existing_dir(&list_args.root.root)?;
            let library = WorkflowLibrary::load(&root)?;
            Ok(render_workflow_list(&root, &library))
        }
        WorkflowCommands::Explain(explain_args) => {
            let root = canonicalize_existing_dir(&explain_args.root.root)?;
            let library = WorkflowLibrary::load(&root)?;
            let workflow = library.named(&explain_args.name)?;
            Ok(render_workflow_explanation(&root, workflow))
        }
        WorkflowCommands::Run(run_args) => run_workflow(run_args).await,
    }
}

async fn run_workflow(args: &WorkflowRunArgs) -> Result<String> {
    let root = canonicalize_existing_dir(&args.root.root)?;
    let library = WorkflowLibrary::load(&root)?;
    let workflow = library.named(&args.name)?.clone();
    let provided_params = parse_param_assignments(&args.params)?;
    validate_param_names(&workflow, &provided_params)?;

    if args.dry_run {
        let prepared = prepare_workflow_run(&root, &workflow, args, provided_params).await?;
        return Ok(render_dry_run(
            &root,
            &prepared.workflow,
            &prepared.provider,
            &prepared.diagnostics,
            prepared.instructions.as_deref(),
            &prepared.prompt,
        ));
    }

    let can_launch_tui = io::stdin().is_terminal() && io::stdout().is_terminal();
    if args.render_once || (!args.no_interactive && can_launch_tui) {
        return run_workflow_tui(root, workflow, args, provided_params).await;
    }

    let resolved_output = args
        .output
        .as_ref()
        .map(|output| resolve_output_path(&root, output))
        .transpose()?;
    let prepared = prepare_workflow_run(&root, &workflow, args, provided_params).await?;
    let artifact = execute_prepared_workflow(&root, &prepared)?;

    if let Some(output) = resolved_output.as_ref() {
        let decision = save_artifact(output, &artifact.markdown, args.overwrite)?;
        if decision == SaveDecision::NeedsOverwrite {
            bail!(
                "refusing to overwrite `{}` without `--overwrite`",
                display_path(output, &root)
            );
        }
        return Ok(render_saved_artifact_message(
            &workflow, &root, output, decision,
        ));
    }

    Ok(render_execution_output(
        &root,
        &prepared.workflow,
        &artifact,
    ))
}

async fn run_workflow_tui(
    root: PathBuf,
    workflow: WorkflowPlaybook,
    args: &WorkflowRunArgs,
    provided_params: BTreeMap<String, String>,
) -> Result<String> {
    let app_config = AppConfig::load()?;
    let resolved_output = args
        .output
        .as_ref()
        .map(|output| resolve_output_path(&root, output))
        .transpose()?;
    let mut app = WorkflowRunApp::new(
        &root,
        workflow,
        provided_params,
        resolved_output,
        args.overwrite,
        app_config.vim_mode_enabled(),
    );
    prime_workflow_tui(root.as_path(), args, &mut app).await?;

    if args.render_once {
        return render_workflow_snapshot(root, args, &mut app).await;
    }

    let mut stdout = io::stdout();
    enable_raw_mode().context("failed to enable raw mode for workflow TUI")?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("failed to initialize workflow TUI terminal")?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create workflow TUI terminal")?;

    loop {
        terminal
            .draw(|frame| render_workflow_run(frame, &app))
            .context("failed to draw workflow TUI")?;

        if !event::poll(Duration::from_millis(250)).context("failed to poll workflow TUI input")? {
            continue;
        }

        match event::read().context("failed to read workflow TUI input")? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let command = app.handle_key(key)?;
                if let Some(message) =
                    apply_workflow_ui_command(&root, args, &mut app, command, false).await?
                {
                    return Ok(message);
                }
            }
            Event::Paste(text) => {
                app.handle_paste(&text);
            }
            Event::Mouse(mouse) => {
                let size = terminal
                    .size()
                    .context("failed to read workflow TUI size")?;
                let _ = app.handle_mouse(mouse, size.into());
            }
            _ => {}
        }
    }
}

async fn prime_workflow_tui(
    root: &Path,
    args: &WorkflowRunArgs,
    app: &mut WorkflowRunApp,
) -> Result<()> {
    if !app.has_wizard_steps() {
        let _ =
            apply_workflow_ui_command(root, args, app, WorkflowUiCommand::Generate, true).await?;
    }
    Ok(())
}

async fn render_workflow_snapshot(
    root: PathBuf,
    args: &WorkflowRunArgs,
    app: &mut WorkflowRunApp,
) -> Result<String> {
    let backend = TestBackend::new(args.width, args.height);
    let mut terminal =
        Terminal::new(backend).context("failed to create workflow snapshot terminal")?;

    for event in &args.events {
        let command = apply_render_once_event(app, *event)?;
        apply_workflow_ui_command(&root, args, app, command, true).await?;
    }

    terminal
        .draw(|frame| render_workflow_run(frame, app))
        .context("failed to draw workflow snapshot")?;
    Ok(snapshot(terminal.backend()))
}

async fn prepare_workflow_run(
    root: &Path,
    workflow: &WorkflowPlaybook,
    args: &WorkflowRunArgs,
    provided_params: BTreeMap<String, String>,
) -> Result<PreparedWorkflowRun> {
    let values = resolve_template_values(root, workflow, args, provided_params).await?;
    let instructions = workflow
        .instructions_template
        .as_deref()
        .map(|template| render_template(template, &values))
        .transpose()?
        .filter(|value| !value.trim().is_empty());
    let prompt = render_template(&workflow.prompt_template, &values)?;
    let app_config = AppConfig::load()?;
    let planning_meta = PlanningMeta::load(root)?;
    let run_args = RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_AGENTS_WORKFLOWS_RUN.to_string()),
        agent: args.provider.clone(),
        prompt: prompt.clone(),
        instructions: instructions.clone(),
        model: args.model.clone(),
        reasoning: args.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    };
    let invocation =
        match resolve_agent_invocation_for_planning(&app_config, &planning_meta, &run_args) {
            Ok(invocation) => invocation,
            Err(error) if args.provider.is_none() && is_no_agent_selected_error(&error) => {
                resolve_agent_invocation_for_planning(
                    &app_config,
                    &planning_meta,
                    &RunAgentArgs {
                        agent: Some(workflow.provider.clone()),
                        ..run_args.clone()
                    },
                )?
            }
            Err(error) => return Err(error),
        };

    Ok(PreparedWorkflowRun {
        workflow: workflow.clone(),
        values: values.clone(),
        instructions,
        prompt,
        run_args,
        provider: invocation.agent.clone(),
        diagnostics: render_invocation_diagnostics(&invocation),
    })
}

fn execute_prepared_workflow(
    root: &Path,
    prepared: &PreparedWorkflowRun,
) -> Result<WorkflowArtifact> {
    let output = run_agent_capture(&RunAgentArgs {
        agent: Some(prepared.provider.clone()),
        ..prepared.run_args.clone()
    })?;
    let markdown = output.stdout.trim().to_string();
    if markdown.is_empty() {
        bail!(
            "workflow `{}` completed without emitting Markdown output",
            prepared.workflow.name
        );
    }

    Ok(WorkflowArtifact {
        markdown,
        provider: prepared.provider.clone(),
        diagnostics: prepared.diagnostics.clone(),
        default_output: default_output_path(root, &prepared.workflow, &prepared.values),
    })
}

async fn resolve_template_values(
    root: &Path,
    workflow: &WorkflowPlaybook,
    args: &WorkflowRunArgs,
    mut values: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    apply_parameter_defaults(workflow, &mut values)?;

    values.insert("repo_root".to_string(), root.display().to_string());
    values.insert(
        "effective_instructions".to_string(),
        load_effective_instructions(root)?,
    );
    values.insert(
        "workflow_contract".to_string(),
        load_workflow_contract(root)?,
    );
    values.insert(
        "project_rules".to_string(),
        load_project_rules_bundle(root)?,
    );
    values.insert(
        "context_bundle".to_string(),
        load_codebase_context_bundle(root)?,
    );
    values.insert("repo_map".to_string(), render_repo_map(root)?);
    values.insert(
        "validation_steps".to_string(),
        if workflow.validation.is_empty() {
            "No explicit validation steps were defined for this workflow.".to_string()
        } else {
            workflow
                .validation
                .iter()
                .map(|step| format!("- {step}"))
                .collect::<Vec<_>>()
                .join("\n")
        },
    );

    if let Some(issue) = resolve_linear_issue(root, workflow, args, &values).await? {
        values.insert("issue_identifier".to_string(), issue.identifier.clone());
        values.insert("issue_title".to_string(), issue.title.clone());
        values.insert("issue_url".to_string(), issue.url.clone());
        values.insert(
            "issue_state".to_string(),
            issue
                .state
                .as_ref()
                .map(|state| state.name.clone())
                .unwrap_or_else(|| "Unknown".to_string()),
        );
        values.insert(
            "issue_description".to_string(),
            issue
                .description
                .clone()
                .unwrap_or_else(|| "_No Linear description was provided._".to_string()),
        );
    }

    Ok(values)
}

async fn resolve_linear_issue(
    root: &Path,
    workflow: &WorkflowPlaybook,
    args: &WorkflowRunArgs,
    values: &BTreeMap<String, String>,
) -> Result<Option<IssueSummary>> {
    let Some(parameter_name) = workflow.linear_issue_parameter.as_deref() else {
        return Ok(None);
    };
    let identifier = values
        .get(parameter_name)
        .map(String::as_str)
        .unwrap_or_default()
        .trim();
    if identifier.is_empty() {
        bail!(
            "workflow `{}` requires the `{parameter_name}` parameter to resolve Linear issue context",
            workflow.name
        );
    }

    let context = load_linear_command_context(
        &LinearClientArgs {
            api_key: args.api_key.clone(),
            api_url: args.api_url.clone(),
            profile: args.profile.clone(),
            root: root.to_path_buf(),
        },
        args.team.clone(),
    )?;
    let issue = context.service.load_issue(identifier).await?;
    Ok(Some(issue))
}

fn validate_param_names(
    workflow: &WorkflowPlaybook,
    values: &BTreeMap<String, String>,
) -> Result<()> {
    let parameter_map = workflow
        .parameters
        .iter()
        .map(|parameter| (parameter.name.as_str(), parameter))
        .collect::<BTreeMap<_, _>>();
    for provided in values.keys() {
        if !parameter_map.contains_key(provided.as_str()) {
            bail!(
                "workflow `{}` does not define a parameter named `{provided}`",
                workflow.name
            );
        }
    }
    Ok(())
}

fn apply_parameter_defaults(
    workflow: &WorkflowPlaybook,
    values: &mut BTreeMap<String, String>,
) -> Result<()> {
    let mut missing = Vec::new();
    for parameter in &workflow.parameters {
        if !values.contains_key(&parameter.name) {
            if let Some(default) = parameter.default.as_ref() {
                values.insert(parameter.name.clone(), default.clone());
            } else if parameter.required {
                missing.push(parameter.name.clone());
            } else {
                values.insert(parameter.name.clone(), String::new());
            }
        }

        let value = values
            .get(&parameter.name)
            .map(String::as_str)
            .unwrap_or_default();
        validate_parameter_value(workflow, parameter, value)?;
    }

    if !missing.is_empty() {
        bail!(
            "workflow `{}` is missing required parameters: {}",
            workflow.name,
            missing.join(", ")
        );
    }

    Ok(())
}

fn validate_parameter_value(
    workflow: &WorkflowPlaybook,
    parameter: &WorkflowParameter,
    value: &str,
) -> Result<()> {
    let trimmed = value.trim();
    if parameter.required && trimmed.is_empty() {
        bail!(
            "workflow `{}` requires a value for `{}`",
            workflow.name,
            parameter.name
        );
    }

    let expects_linear_identifier = workflow.linear_issue_parameter.as_deref()
        == Some(parameter.name.as_str())
        || parameter.name == "issue";
    if expects_linear_identifier && !trimmed.is_empty() && !looks_like_linear_identifier(trimmed) {
        bail!(
            "`{}` must look like a Linear issue identifier such as `MET-50`",
            parameter.name
        );
    }

    Ok(())
}

fn render_workflow_list(root: &Path, library: &WorkflowLibrary) -> String {
    let mut lines = vec![format!(
        "Available workflows ({}):",
        library.workflows.len()
    )];
    for workflow in library.workflows.values() {
        lines.push(format!(
            "- `{}`: {} [provider: `{}`; source: `{}`]",
            workflow.name,
            workflow.summary,
            workflow.provider,
            workflow.source_label(root)
        ));
    }
    lines.join("\n")
}

fn render_workflow_explanation(root: &Path, workflow: &WorkflowPlaybook) -> String {
    let mut lines = vec![
        format!("# Workflow: {}", workflow.name),
        String::new(),
        format!("Summary: {}", workflow.summary),
        format!("Source: `{}`", workflow.source_label(root)),
        format!("Provider: `{}`", workflow.provider),
        "Interactive mode: TTY runs open a guided wizard, then a review/export dashboard."
            .to_string(),
        "Fallback mode: use `--no-interactive` with explicit `--param key=value` pairs for scripts."
            .to_string(),
    ];

    if let Some(parameter) = workflow.linear_issue_parameter.as_deref() {
        lines.push(format!("Linear issue parameter: `{parameter}`"));
    }

    lines.extend([String::new(), "## Parameters".to_string(), String::new()]);
    if workflow.parameters.is_empty() {
        lines.push("- _This workflow does not define any explicit parameters._".to_string());
    } else {
        for parameter in &workflow.parameters {
            let requirement = if parameter.required {
                "required"
            } else {
                "optional"
            };
            let default_suffix = parameter
                .default
                .as_deref()
                .map(|default| format!("; default: `{default}`"))
                .unwrap_or_default();
            lines.push(format!(
                "- `{}` ({requirement}): {}{}",
                parameter.name, parameter.description, default_suffix
            ));
        }
    }

    lines.extend([
        String::new(),
        "## Review And Save".to_string(),
        String::new(),
        "- Review mode shows the generated Markdown plus the resolved input values and validation checklist.".to_string(),
        "- `e` enters multiline edit mode for the generated Markdown.".to_string(),
        "- `s` opens a one-off save-path prompt with a `.metastack/workflows/generated/` default.".to_string(),
        "- Existing files require explicit overwrite confirmation in the TUI or `--overwrite` in non-interactive mode.".to_string(),
    ]);

    lines.extend([String::new(), "## Validation".to_string(), String::new()]);
    if workflow.validation.is_empty() {
        lines.push("- _No explicit validation steps were defined._".to_string());
    } else {
        for step in &workflow.validation {
            lines.push(format!("- {step}"));
        }
    }

    lines.extend([
        String::new(),
        "## Instructions Template".to_string(),
        String::new(),
        workflow
            .instructions_template
            .clone()
            .unwrap_or_else(|| "_None_".to_string()),
        String::new(),
        "## Prompt Template".to_string(),
        String::new(),
        workflow.prompt_template.clone(),
    ]);

    lines.join("\n")
}

fn render_dry_run(
    root: &Path,
    workflow: &WorkflowPlaybook,
    provider: &str,
    diagnostics: &[String],
    instructions: Option<&str>,
    prompt: &str,
) -> String {
    let mut lines = vec![
        format!("Workflow: `{}`", workflow.name),
        format!("Provider: `{provider}`"),
        format!("Source: `{}`", workflow.source_label(root)),
        String::new(),
        "Validation steps:".to_string(),
    ];
    lines.extend(diagnostics.iter().cloned());
    if workflow.validation.is_empty() {
        lines.push("- No explicit validation steps were defined.".to_string());
    } else {
        for step in &workflow.validation {
            lines.push(format!("- {step}"));
        }
    }

    lines.extend([String::new(), "Instructions:".to_string(), String::new()]);
    lines.push(
        instructions
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("_None_")
            .to_string(),
    );
    lines.extend([
        String::new(),
        "Prompt:".to_string(),
        String::new(),
        prompt.to_string(),
    ]);
    lines.join("\n")
}

fn render_execution_output(
    root: &Path,
    workflow: &WorkflowPlaybook,
    artifact: &WorkflowArtifact,
) -> String {
    let mut lines = vec![
        format!(
            "Ran workflow `{}` with provider `{}`.",
            workflow.name, artifact.provider
        ),
        format!("Source: `{}`", workflow.source_label(root)),
    ];
    lines.extend(artifact.diagnostics.iter().cloned());

    if !workflow.validation.is_empty() {
        lines.push(String::new());
        lines.push("Validation steps:".to_string());
        for step in &workflow.validation {
            lines.push(format!("- {step}"));
        }
    }

    lines.push(String::new());
    lines.push(artifact.markdown.clone());
    lines.join("\n")
}

fn render_saved_artifact_message(
    workflow: &WorkflowPlaybook,
    root: &Path,
    output: &Path,
    decision: SaveDecision,
) -> String {
    let status = match decision {
        SaveDecision::Created => "created",
        SaveDecision::Updated => "updated",
        SaveDecision::Unchanged => "reused",
        SaveDecision::NeedsOverwrite => "blocked",
    };
    let display = display_output_path(root, output);
    format!(
        "Workflow `{}` artifact {status} at `{}`.",
        workflow.name, display
    )
}

fn display_output_path(root: &Path, output: &Path) -> String {
    let canonical_root = root.canonicalize().ok();
    let canonical_output = canonicalize_for_repo_check(output).ok();

    if let (Some(canonical_root), Some(canonical_output)) = (canonical_root, canonical_output) {
        if let Ok(relative) = canonical_output.strip_prefix(&canonical_root) {
            return relative.display().to_string();
        }
    }

    display_path(output, root)
}

impl WorkflowRunApp {
    fn new(
        root: &Path,
        workflow: WorkflowPlaybook,
        provided_params: BTreeMap<String, String>,
        output: Option<PathBuf>,
        overwrite: bool,
        _vim_mode: bool,
    ) -> Self {
        let fields = workflow
            .parameters
            .iter()
            .map(|parameter| WorkflowParameterField {
                parameter: parameter.clone(),
                input: InputFieldState::multiline(
                    provided_params
                        .get(&parameter.name)
                        .cloned()
                        .or_else(|| parameter.default.clone())
                        .unwrap_or_default(),
                ),
            })
            .collect::<Vec<_>>();

        let default_output = output
            .clone()
            .unwrap_or_else(|| default_output_path(root, &workflow, &provided_params));

        Self {
            repo_root: root.to_path_buf(),
            workflow,
            fields,
            screen: WorkflowRunScreen::Wizard,
            step_index: 0,
            review_focus: ReviewFocus::Artifact,
            artifact_scroll: ScrollState::default(),
            detail_scroll: ScrollState::default(),
            save_path_input: InputFieldState::new(display_path(&default_output, root)),
            markdown_editor: InputFieldState::multiline(String::new()),
            artifact: None,
            overwrite,
            preferred_output: output,
            save_message: None,
            error: None,
        }
    }

    fn values(&self) -> BTreeMap<String, String> {
        self.fields
            .iter()
            .map(|field| {
                (
                    field.parameter.name.clone(),
                    field.input.value().to_string(),
                )
            })
            .collect()
    }

    fn has_wizard_steps(&self) -> bool {
        !self.fields.is_empty()
    }

    fn set_error(&mut self, message: String) {
        self.error = Some(message);
    }

    fn clear_status(&mut self) {
        self.error = None;
        self.save_message = None;
    }

    fn enter_review(&mut self, artifact: WorkflowArtifact) {
        self.markdown_editor = InputFieldState::multiline(artifact.markdown.clone());
        let save_path = self
            .preferred_output
            .clone()
            .unwrap_or_else(|| artifact.default_output.clone());
        self.save_path_input =
            InputFieldState::new(display_output_path(&self.repo_root, &save_path));
        self.artifact = Some(artifact);
        self.screen = WorkflowRunScreen::Review;
        self.review_focus = ReviewFocus::Artifact;
        self.artifact_scroll.reset();
        self.detail_scroll.reset();
        self.error = None;
        self.save_message =
            Some("Generation complete. Review, edit, or save the Markdown artifact.".to_string());
    }

    fn artifact_markdown(&self) -> Result<&str> {
        self.artifact
            .as_ref()
            .map(|artifact| artifact.markdown.as_str())
            .ok_or_else(|| anyhow!("no generated artifact is available yet"))
    }

    fn requested_output_path(&mut self, root: &Path) -> Result<Option<PathBuf>> {
        let raw = self.save_path_input.value().trim();
        if raw.is_empty() {
            self.set_error("save path cannot be empty".to_string());
            return Ok(None);
        }
        resolve_output_path(root, Path::new(raw)).map(Some)
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<WorkflowUiCommand> {
        if matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(WorkflowUiCommand::Cancel);
        }

        match self.screen {
            WorkflowRunScreen::Wizard => self.handle_wizard_key(key),
            WorkflowRunScreen::Review => self.handle_review_key(key),
            WorkflowRunScreen::Edit => self.handle_edit_key(key),
            WorkflowRunScreen::SavePath => self.handle_save_path_key(key),
            WorkflowRunScreen::ConfirmOverwrite => self.handle_confirm_overwrite_key(key),
        }
    }

    fn handle_wizard_key(&mut self, key: KeyEvent) -> Result<WorkflowUiCommand> {
        let width = wizard_input_viewport(Rect::new(0, 0, 120, 24)).width;
        let height = 16;

        match key.code {
            KeyCode::Esc => Ok(WorkflowUiCommand::Cancel),
            KeyCode::BackTab => {
                self.clear_status();
                if self.step_index > 0 {
                    self.step_index -= 1;
                }
                Ok(WorkflowUiCommand::None)
            }
            KeyCode::Tab | KeyCode::Enter => {
                self.clear_status();
                self.validate_current_step()?;
                if self.step_index + 1 < self.fields.len() {
                    self.step_index += 1;
                    Ok(WorkflowUiCommand::None)
                } else {
                    Ok(WorkflowUiCommand::Generate)
                }
            }
            _ if self
                .current_input_mut()
                .handle_key_with_viewport(key, width, height) =>
            {
                Ok(WorkflowUiCommand::None)
            }
            _ => Ok(WorkflowUiCommand::None),
        }
    }

    fn handle_review_key(&mut self, key: KeyEvent) -> Result<WorkflowUiCommand> {
        let artifact_viewport = review_artifact_viewport(Rect::new(0, 0, 120, 34));
        let detail_viewport = review_detail_viewport(Rect::new(0, 0, 120, 34));
        match key.code {
            KeyCode::Esc => Ok(WorkflowUiCommand::Cancel),
            KeyCode::Tab => {
                self.review_focus = match self.review_focus {
                    ReviewFocus::Artifact => ReviewFocus::Details,
                    ReviewFocus::Details => ReviewFocus::Artifact,
                };
                Ok(WorkflowUiCommand::None)
            }
            KeyCode::Char('e') if key.modifiers.is_empty() => {
                self.clear_status();
                self.screen = WorkflowRunScreen::Edit;
                Ok(WorkflowUiCommand::None)
            }
            KeyCode::Char('s') if key.modifiers.is_empty() => {
                self.clear_status();
                self.screen = WorkflowRunScreen::SavePath;
                Ok(WorkflowUiCommand::None)
            }
            _ => {
                match self.review_focus {
                    ReviewFocus::Artifact => {
                        let rows =
                            wrapped_rows(self.markdown_editor.value(), artifact_viewport.width);
                        self.artifact_scroll
                            .apply_key_in_viewport(key, artifact_viewport, rows);
                    }
                    ReviewFocus::Details => {
                        let rows = wrapped_rows(&self.review_details_text(), detail_viewport.width);
                        self.detail_scroll
                            .apply_key_in_viewport(key, detail_viewport, rows);
                    }
                }
                Ok(WorkflowUiCommand::None)
            }
        }
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> Result<WorkflowUiCommand> {
        let viewport = review_artifact_viewport(Rect::new(0, 0, 120, 34));
        match key.code {
            KeyCode::Esc => {
                if let Some(artifact) = self.artifact.as_ref() {
                    self.markdown_editor = InputFieldState::multiline(artifact.markdown.clone());
                }
                self.screen = WorkflowRunScreen::Review;
                self.error = None;
                Ok(WorkflowUiCommand::None)
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(artifact) = self.artifact.as_mut() {
                    artifact.markdown = self.markdown_editor.value().to_string();
                }
                self.screen = WorkflowRunScreen::Review;
                self.save_message = Some("Accepted Markdown edits.".to_string());
                Ok(WorkflowUiCommand::None)
            }
            _ if self.markdown_editor.handle_key_with_viewport(
                key,
                viewport.width,
                viewport.height,
            ) =>
            {
                Ok(WorkflowUiCommand::None)
            }
            _ => Ok(WorkflowUiCommand::None),
        }
    }

    fn handle_save_path_key(&mut self, key: KeyEvent) -> Result<WorkflowUiCommand> {
        let viewport = save_prompt_viewport(Rect::new(0, 0, 120, 34));
        match key.code {
            KeyCode::Esc => {
                self.screen = WorkflowRunScreen::Review;
                Ok(WorkflowUiCommand::None)
            }
            KeyCode::Enter => Ok(WorkflowUiCommand::Save {
                overwrite: self.overwrite,
            }),
            _ if self.save_path_input.handle_key_with_viewport(
                key,
                viewport.width,
                viewport.height,
            ) =>
            {
                Ok(WorkflowUiCommand::None)
            }
            _ => Ok(WorkflowUiCommand::None),
        }
    }

    fn handle_confirm_overwrite_key(&mut self, key: KeyEvent) -> Result<WorkflowUiCommand> {
        match key.code {
            KeyCode::Esc => {
                self.screen = WorkflowRunScreen::SavePath;
                Ok(WorkflowUiCommand::None)
            }
            KeyCode::Enter => Ok(WorkflowUiCommand::Save { overwrite: true }),
            _ => Ok(WorkflowUiCommand::None),
        }
    }

    fn handle_paste(&mut self, text: &str) {
        match self.screen {
            WorkflowRunScreen::Wizard => {
                let _ = self.current_input_mut().paste(text);
            }
            WorkflowRunScreen::Edit => {
                let _ = self.markdown_editor.paste(text);
            }
            WorkflowRunScreen::SavePath => {
                let _ = self.save_path_input.paste(text);
            }
            WorkflowRunScreen::Review | WorkflowRunScreen::ConfirmOverwrite => {}
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, area: Rect) -> bool {
        if !matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            return false;
        }

        match self.screen {
            WorkflowRunScreen::Review => match self.review_focus {
                ReviewFocus::Artifact => self.artifact_scroll.apply_mouse_in_viewport(
                    mouse,
                    review_artifact_viewport(area),
                    wrapped_rows(
                        self.artifact_markdown().unwrap_or_default(),
                        review_artifact_viewport(area).width,
                    ),
                ),
                ReviewFocus::Details => self.detail_scroll.apply_mouse_in_viewport(
                    mouse,
                    review_detail_viewport(area),
                    wrapped_rows(
                        &self.review_details_text(),
                        review_detail_viewport(area).width,
                    ),
                ),
            },
            WorkflowRunScreen::Edit => self.markdown_editor.handle_mouse_scroll(
                mouse,
                review_artifact_viewport(area),
                review_artifact_viewport(area).width,
                review_artifact_viewport(area).height,
            ),
            WorkflowRunScreen::Wizard => self.current_input_mut().handle_mouse_scroll(
                mouse,
                wizard_input_viewport(area),
                wizard_input_viewport(area).width,
                wizard_input_viewport(area).height,
            ),
            WorkflowRunScreen::SavePath => self.save_path_input.handle_mouse_scroll(
                mouse,
                save_prompt_viewport(area),
                save_prompt_viewport(area).width,
                save_prompt_viewport(area).height,
            ),
            WorkflowRunScreen::ConfirmOverwrite => false,
        }
    }

    fn validate_current_step(&self) -> Result<()> {
        let field = &self.fields[self.step_index];
        validate_parameter_value(&self.workflow, &field.parameter, field.input.value())
    }

    fn current_input_mut(&mut self) -> &mut InputFieldState {
        &mut self.fields[self.step_index].input
    }

    fn review_details_text(&self) -> String {
        let mut lines = vec![
            format!("Workflow: {}", self.workflow.name),
            format!("Summary: {}", self.workflow.summary),
            String::new(),
            "Inputs".to_string(),
        ];
        for field in &self.fields {
            lines.push(format!(
                "- {}: {}",
                field.parameter.name,
                if field.input.value().trim().is_empty() {
                    "<empty>".to_string()
                } else {
                    field.input.value().trim().replace('\n', " ")
                }
            ));
        }

        lines.push(String::new());
        lines.push("Validation".to_string());
        if self.workflow.validation.is_empty() {
            lines.push("- No explicit validation steps were defined.".to_string());
        } else {
            for step in &self.workflow.validation {
                lines.push(format!("- {step}"));
            }
        }

        if let Some(artifact) = self.artifact.as_ref() {
            lines.push(String::new());
            lines.push(format!("Provider: {}", artifact.provider));
            for line in &artifact.diagnostics {
                lines.push(line.clone());
            }
        }

        lines.join("\n")
    }
}

fn apply_render_once_event(
    app: &mut WorkflowRunApp,
    event: WorkflowRunEventArg,
) -> Result<WorkflowUiCommand> {
    let key = match event {
        WorkflowRunEventArg::Enter => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        WorkflowRunEventArg::Tab => KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        WorkflowRunEventArg::BackTab => KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        WorkflowRunEventArg::Esc => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        WorkflowRunEventArg::Edit => KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE),
        WorkflowRunEventArg::Save => KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE),
        WorkflowRunEventArg::AcceptEdit => KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
        WorkflowRunEventArg::DiscardEdit => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    };
    app.handle_key(key)
}

async fn apply_workflow_ui_command(
    root: &Path,
    args: &WorkflowRunArgs,
    app: &mut WorkflowRunApp,
    command: WorkflowUiCommand,
    snapshot_mode: bool,
) -> Result<Option<String>> {
    match command {
        WorkflowUiCommand::None => Ok(None),
        WorkflowUiCommand::Cancel => {
            if snapshot_mode {
                app.save_message = Some("Workflow run canceled.".to_string());
                Ok(None)
            } else {
                Ok(Some("Workflow run canceled.".to_string()))
            }
        }
        WorkflowUiCommand::Generate => {
            let prepared = match prepare_workflow_run(root, &app.workflow, args, app.values()).await
            {
                Ok(prepared) => prepared,
                Err(error) => {
                    app.set_error(error.to_string());
                    return Ok(None);
                }
            };
            match execute_prepared_workflow(root, &prepared) {
                Ok(artifact) => app.enter_review(artifact),
                Err(error) => app.set_error(error.to_string()),
            }
            Ok(None)
        }
        WorkflowUiCommand::Save { overwrite } => {
            let Some(output) = app.requested_output_path(root)? else {
                return Ok(None);
            };
            match save_artifact(&output, app.artifact_markdown()?, overwrite) {
                Ok(SaveDecision::NeedsOverwrite) => {
                    app.screen = WorkflowRunScreen::ConfirmOverwrite;
                    app.set_error(format!(
                        "`{}` already exists. Confirm overwrite to replace it.",
                        display_path(&output, root)
                    ));
                    Ok(None)
                }
                Ok(decision) => {
                    let message =
                        render_saved_artifact_message(&app.workflow, root, &output, decision);
                    if snapshot_mode {
                        app.screen = WorkflowRunScreen::Review;
                        app.error = None;
                        app.save_message = Some(message);
                        Ok(None)
                    } else {
                        Ok(Some(message))
                    }
                }
                Err(error) => {
                    app.set_error(error.to_string());
                    Ok(None)
                }
            }
        }
    }
}

fn render_workflow_run(frame: &mut ratatui::Frame<'_>, app: &WorkflowRunApp) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(4),
        ])
        .split(frame.area());

    let header = Paragraph::new(Text::from(vec![
        Line::from(format!("Workflow Run ({})", app.workflow.name)),
        Line::from(format!("TTY-first wizard for `{}`", app.workflow.summary)),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Workflow"));
    frame.render_widget(header, layout[0]);

    match app.screen {
        WorkflowRunScreen::Wizard => render_wizard(frame, app, layout[1]),
        WorkflowRunScreen::Review => render_review(frame, app, layout[1]),
        WorkflowRunScreen::Edit => render_edit(frame, app, layout[1]),
        WorkflowRunScreen::SavePath => render_save_prompt(frame, app, layout[1]),
        WorkflowRunScreen::ConfirmOverwrite => render_overwrite_prompt(frame, app, layout[1]),
    }

    render_footer(frame, app, layout[2]);
}

fn render_wizard(frame: &mut ratatui::Frame<'_>, app: &WorkflowRunApp, area: Rect) {
    if app.fields.is_empty() {
        let waiting = Paragraph::new(Text::from(vec![
            Line::from("This workflow does not require any explicit inputs."),
            Line::from("Generation runs immediately and then opens the review/export dashboard."),
        ]))
        .wrap(Wrap { trim: false })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Wizard")
                .border_style(Style::default().add_modifier(Modifier::BOLD)),
        );
        frame.render_widget(waiting, area);
        return;
    }

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(28),
            Constraint::Min(48),
            Constraint::Length(36),
        ])
        .split(area);

    let mut state = ListState::default();
    state.select(Some(app.step_index));
    let steps = app
        .fields
        .iter()
        .map(|field| {
            let suffix = if field.parameter.required {
                "required"
            } else {
                "optional"
            };
            ListItem::new(format!("{} ({suffix})", field.parameter.name))
        })
        .collect::<Vec<_>>();
    let step_list = List::new(steps)
        .block(Block::default().borders(Borders::ALL).title("Wizard Steps"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(step_list, body[0], &mut state);

    let field = &app.fields[app.step_index];
    let title = format!(
        "Step {} of {}: {}",
        app.step_index + 1,
        app.fields.len().max(1),
        field.parameter.name
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let inner = block.inner(body[1]);
    let rendered = field.input.render_with_viewport(
        &field.parameter.description,
        true,
        inner.width,
        inner.height,
    );
    frame.render_widget(rendered.paragraph(block), body[1]);
    rendered.set_cursor(frame, inner);

    let summary = Paragraph::new(Text::from(vec![
        Line::from(field.parameter.description.clone()),
        Line::from(String::new()),
        Line::from(format!(
            "Validation: {}",
            validation_label(&app.workflow, &field.parameter)
        )),
        Line::from(String::new()),
        Line::from("Current Values"),
        Line::from(app.review_details_text()),
    ]))
    .wrap(Wrap { trim: false })
    .block(Block::default().borders(Borders::ALL).title("Review"));
    frame.render_widget(summary, body[2]);
}

fn render_review(frame: &mut ratatui::Frame<'_>, app: &WorkflowRunApp, area: Rect) {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(0)])
        .split(area);

    let detail = scrollable_paragraph(
        app.review_details_text(),
        match app.review_focus {
            ReviewFocus::Artifact => "Details",
            ReviewFocus::Details => "Details [focus]",
        },
        &app.detail_scroll,
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(detail, body[0]);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(match app.review_focus {
            ReviewFocus::Artifact => "Generated Markdown [focus]",
            ReviewFocus::Details => "Generated Markdown",
        });
    let markdown = app
        .artifact
        .as_ref()
        .map(|artifact| artifact.markdown.clone())
        .unwrap_or_default();
    let paragraph = Paragraph::new(markdown)
        .block(block)
        .scroll((app.artifact_scroll.offset(), 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, body[1]);
}

fn render_edit(frame: &mut ratatui::Frame<'_>, app: &WorkflowRunApp, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Edit Markdown [Ctrl+S accepts, Esc discards]")
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let inner = block.inner(area);
    let rendered = app.markdown_editor.render_with_viewport(
        "Edit the generated Markdown...",
        true,
        inner.width,
        inner.height,
    );
    frame.render_widget(rendered.paragraph(block), area);
    rendered.set_cursor(frame, inner);
}

fn render_save_prompt(frame: &mut ratatui::Frame<'_>, app: &WorkflowRunApp, area: Rect) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Length(7),
            Constraint::Percentage(30),
        ])
        .split(area);
    let prompt_area = centered_rect(vertical[1], 90, 7);
    let block = Block::default()
        .borders(Borders::ALL)
        .title("Save Artifact Path")
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let inner = block.inner(prompt_area);
    let rendered = app.save_path_input.render_with_viewport(
        ".metastack/workflows/generated/...",
        true,
        inner.width,
        inner.height,
    );
    frame.render_widget(rendered.paragraph(block), prompt_area);
    rendered.set_cursor(frame, inner);
}

fn render_overwrite_prompt(frame: &mut ratatui::Frame<'_>, _app: &WorkflowRunApp, area: Rect) {
    let prompt_area = centered_rect(area, 78, 7);
    let prompt = Paragraph::new(Text::from(vec![
        Line::from("The selected file already exists."),
        Line::from("Press Enter to overwrite it or Esc to return to the save prompt."),
    ]))
    .wrap(Wrap { trim: false })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Confirm Overwrite")
            .border_style(Style::default().add_modifier(Modifier::BOLD)),
    );
    frame.render_widget(prompt, prompt_area);
}

fn render_footer(frame: &mut ratatui::Frame<'_>, app: &WorkflowRunApp, area: Rect) {
    let controls = match app.screen {
        WorkflowRunScreen::Wizard => {
            "Type each required input. Enter advances, Shift+Enter inserts a newline, Shift+Tab goes back, and the last Enter generates the workflow."
        }
        WorkflowRunScreen::Review => {
            "Review the generated Markdown. Tab switches panes, e edits, s saves, and Esc exits without saving."
        }
        WorkflowRunScreen::Edit => {
            "Edit mode supports multiline navigation. Ctrl+S accepts edits, Esc discards them."
        }
        WorkflowRunScreen::SavePath => {
            "Enter saves to the shown path. Esc returns to review. Paths must stay inside the repository root."
        }
        WorkflowRunScreen::ConfirmOverwrite => {
            "Enter confirms overwrite. Esc returns to the save prompt."
        }
    };
    let mut lines = vec![Line::from(controls)];
    if let Some(message) = app.save_message.as_ref() {
        lines.push(Line::from(format!("Status: {message}")));
    } else if let Some(error) = app.error.as_ref() {
        lines.push(Line::from(format!("Error: {error}")));
    } else {
        lines.push(Line::from("Ready."));
    }
    let footer = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("Controls"));
    frame.render_widget(footer, area);
}

fn wizard_input_viewport(area: Rect) -> Rect {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(28),
            Constraint::Min(48),
            Constraint::Length(36),
        ])
        .split(area);
    Block::default().borders(Borders::ALL).inner(body[1])
}

fn review_artifact_viewport(area: Rect) -> Rect {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(0)])
        .split(area);
    Block::default().borders(Borders::ALL).inner(body[1])
}

fn review_detail_viewport(area: Rect) -> Rect {
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(36), Constraint::Min(0)])
        .split(area);
    Block::default().borders(Borders::ALL).inner(body[0])
}

fn save_prompt_viewport(area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Length(7),
            Constraint::Percentage(30),
        ])
        .split(area);
    let prompt_area = centered_rect(vertical[1], 90, 7);
    Block::default().borders(Borders::ALL).inner(prompt_area)
}

fn centered_rect(area: Rect, width_percent: u16, height: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}

fn validation_label(workflow: &WorkflowPlaybook, parameter: &WorkflowParameter) -> String {
    let mut parts = Vec::new();
    if parameter.required {
        parts.push("required".to_string());
    } else {
        parts.push("optional".to_string());
    }
    if workflow.linear_issue_parameter.as_deref() == Some(parameter.name.as_str())
        || parameter.name == "issue"
    {
        parts.push("must look like MET-50".to_string());
    }
    parts.join("; ")
}

fn save_artifact(path: &Path, contents: &str, overwrite: bool) -> Result<SaveDecision> {
    let existing = fs::read_to_string(path);
    match existing {
        Ok(existing) if existing == contents => Ok(SaveDecision::Unchanged),
        Ok(_) if !overwrite => Ok(SaveDecision::NeedsOverwrite),
        Ok(_) => Ok(match write_text_file(path, contents, true)? {
            FileWriteStatus::Updated => SaveDecision::Updated,
            FileWriteStatus::Created => SaveDecision::Created,
            FileWriteStatus::Unchanged => SaveDecision::Unchanged,
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(match write_text_file(path, contents, true)? {
                FileWriteStatus::Updated => SaveDecision::Updated,
                FileWriteStatus::Created => SaveDecision::Created,
                FileWriteStatus::Unchanged => SaveDecision::Unchanged,
            })
        }
        Err(error) => {
            Err(error).with_context(|| format!("failed to read existing file `{}`", path.display()))
        }
    }
}

fn resolve_output_path(root: &Path, output: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve repository root `{}`", root.display()))?;
    let candidate = if output.is_absolute() {
        output.to_path_buf()
    } else {
        root.join(output)
    };
    let normalized = normalize_path(&candidate)?;
    let comparable = canonicalize_for_repo_check(&normalized)?;
    if !comparable.starts_with(&root) {
        bail!(
            "refusing to write outside the repository root: `{}`",
            normalized.display()
        );
    }
    if normalized.file_name().is_none() {
        bail!("output path must reference a file, not a directory");
    }
    Ok(normalized)
}

fn normalize_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    bail!(
                        "path `{}` escapes above the repository root",
                        path.display()
                    );
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Ok(normalized)
}

fn canonicalize_for_repo_check(path: &Path) -> Result<PathBuf> {
    if path.exists() {
        return path
            .canonicalize()
            .with_context(|| format!("failed to resolve `{}`", path.display()));
    }

    let mut existing_ancestor = path.parent().ok_or_else(|| {
        anyhow!(
            "failed to resolve parent directory for `{}`",
            path.display()
        )
    })?;
    while !existing_ancestor.exists() {
        existing_ancestor = existing_ancestor.parent().ok_or_else(|| {
            anyhow!(
                "failed to resolve an existing ancestor for `{}`",
                path.display()
            )
        })?;
    }

    let canonical_ancestor = existing_ancestor
        .canonicalize()
        .with_context(|| format!("failed to resolve `{}`", existing_ancestor.display()))?;
    let suffix = path
        .strip_prefix(existing_ancestor)
        .with_context(|| format!("failed to compare `{}`", path.display()))?;
    Ok(canonical_ancestor.join(suffix))
}

fn default_output_path(
    root: &Path,
    workflow: &WorkflowPlaybook,
    values: &BTreeMap<String, String>,
) -> PathBuf {
    let paths = PlanningPaths::new(root);
    let stem = if let Some(parameter) = workflow.linear_issue_parameter.as_deref() {
        values
            .get(parameter)
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!("{}-{}", slugify(&workflow.name), slugify(value)))
            .unwrap_or_else(|| slugify(&workflow.name))
    } else {
        slugify(&workflow.name)
    };
    paths
        .workflows_dir
        .join("generated")
        .join(format!("{stem}.md"))
}

fn looks_like_linear_identifier(value: &str) -> bool {
    let Some((team, number)) = value.split_once('-') else {
        return false;
    };
    !team.is_empty()
        && team.chars().all(|ch| ch.is_ascii_uppercase())
        && !number.is_empty()
        && number.chars().all(|ch| ch.is_ascii_digit())
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut previous_dash = false;
    for ch in value.chars() {
        let next = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if next == '-' {
            if !previous_dash && !slug.is_empty() {
                slug.push(next);
            }
            previous_dash = true;
        } else {
            slug.push(next);
            previous_dash = false;
        }
    }
    slug.trim_matches('-').to_string()
}

fn parse_param_assignments(raw_params: &[String]) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for raw in raw_params {
        let (key, value) = raw
            .split_once('=')
            .ok_or_else(|| anyhow!("workflow parameters must use `key=value`, got `{raw}`"))?;
        let key = key.trim();
        if key.is_empty() {
            bail!("workflow parameter names cannot be empty");
        }
        if values.insert(key.to_string(), value.to_string()).is_some() {
            bail!("workflow parameter `{key}` was provided more than once");
        }
    }
    Ok(values)
}

fn render_template(template: &str, values: &BTreeMap<String, String>) -> Result<String> {
    let unresolved = collect_missing_placeholders(template, values);
    if !unresolved.is_empty() {
        bail!(
            "workflow template left unresolved placeholders: {}",
            unresolved.join(", ")
        );
    }

    let mut rendered = template.replace("\r\n", "\n");
    for (key, value) in values {
        rendered = rendered.replace(&format!("{{{{{key}}}}}"), value);
    }

    Ok(rendered.trim().to_string())
}

fn collect_missing_placeholders(template: &str, values: &BTreeMap<String, String>) -> Vec<String> {
    let mut placeholders = Vec::new();
    let mut remainder = template;

    while let Some(start) = remainder.find("{{") {
        let after_start = &remainder[start + 2..];
        let Some(end) = after_start.find("}}") else {
            break;
        };
        let name = after_start[..end].trim();
        if !name.is_empty() && !values.contains_key(name) {
            placeholders.push(name.to_string());
        }
        remainder = &after_start[end + 2..];
    }

    placeholders.sort();
    placeholders.dedup();
    placeholders
}

impl WorkflowLibrary {
    fn load(root: &Path) -> Result<Self> {
        let mut workflows = BTreeMap::new();

        for (source_name, contents) in BUILTIN_WORKFLOWS {
            let workflow = parse_playbook(contents, WorkflowSource::Builtin(source_name))?;
            workflows.insert(workflow.name.clone(), workflow);
        }

        let workflows_dir = PlanningPaths::new(root).workflows_dir;
        if workflows_dir.is_dir() {
            for entry in WalkDir::new(&workflows_dir) {
                let entry = entry?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                let is_markdown = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("md"));
                if !is_markdown {
                    continue;
                }
                if path
                    .strip_prefix(&workflows_dir)
                    .ok()
                    .is_some_and(|relative| {
                        relative
                            .components()
                            .any(|component| component.as_os_str() == "generated")
                    })
                {
                    continue;
                }
                if path.file_name().and_then(|value| value.to_str()) == Some("README.md") {
                    continue;
                }
                let contents = fs::read_to_string(path)
                    .with_context(|| format!("failed to read `{}`", path.display()))?;
                let workflow =
                    parse_playbook(&contents, WorkflowSource::Local(path.to_path_buf()))?;
                workflows.insert(workflow.name.clone(), workflow);
            }
        }

        Ok(Self { workflows })
    }

    fn named(&self, name: &str) -> Result<&WorkflowPlaybook> {
        self.workflows.get(name).ok_or_else(|| {
            let available = self
                .workflows
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            anyhow!("workflow `{name}` was not found. Available workflows: {available}")
        })
    }
}

impl WorkflowPlaybook {
    fn source_label(&self, root: &Path) -> String {
        match &self.source {
            WorkflowSource::Builtin(path) => path.to_string(),
            WorkflowSource::Local(path) => display_path(path, root),
        }
    }
}

fn parse_playbook(raw: &str, source: WorkflowSource) -> Result<WorkflowPlaybook> {
    let normalized = raw.replace("\r\n", "\n");
    let (front_matter, body) = split_front_matter(&normalized).ok_or_else(|| {
        anyhow!("workflow playbooks must start with YAML front matter delimited by `---`")
    })?;
    let front_matter: WorkflowFrontMatter =
        serde_yaml::from_str(front_matter).context("failed to parse workflow front matter")?;

    let name = front_matter.name.trim().to_string();
    if name.is_empty() {
        bail!("workflow playbook name cannot be empty");
    }
    if front_matter.summary.trim().is_empty() {
        bail!("workflow `{name}` is missing a summary");
    }
    if front_matter.provider.trim().is_empty() {
        bail!("workflow `{name}` is missing a provider");
    }
    let prompt_template = body.trim().to_string();
    if prompt_template.is_empty() {
        bail!("workflow `{name}` is missing a prompt template body");
    }

    let mut parameters = Vec::new();
    for parameter in front_matter.parameters {
        let parameter_name = parameter.name.trim().to_string();
        if parameter_name.is_empty() {
            bail!("workflow `{name}` defines a parameter with an empty name");
        }
        if parameter.description.trim().is_empty() {
            bail!("workflow `{name}` defines parameter `{parameter_name}` without a description");
        }
        parameters.push(WorkflowParameter {
            name: parameter_name,
            description: parameter.description.trim().to_string(),
            required: parameter.required,
            default: parameter.default.map(|value| value.trim().to_string()),
        });
    }

    Ok(WorkflowPlaybook {
        name,
        summary: front_matter.summary.trim().to_string(),
        provider: front_matter.provider.trim().to_string(),
        parameters,
        validation: front_matter
            .validation
            .into_iter()
            .map(|step| step.trim().to_string())
            .filter(|step| !step.is_empty())
            .collect(),
        instructions_template: front_matter
            .instructions
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        prompt_template,
        linear_issue_parameter: front_matter
            .linear_issue_parameter
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        source,
    })
}

fn split_front_matter(raw: &str) -> Option<(&str, &str)> {
    let raw = raw.strip_prefix("---\n")?;
    let divider = raw.find("\n---\n")?;
    Some((&raw[..divider], &raw[divider + 5..]))
}

fn snapshot(backend: &TestBackend) -> String {
    let width = backend
        .size()
        .map(|size| size.width as usize)
        .unwrap_or(1usize);
    backend
        .buffer()
        .content
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_linear_identifier_requires_team_and_number() {
        assert!(looks_like_linear_identifier("MET-50"));
        assert!(!looks_like_linear_identifier("met-50"));
        assert!(!looks_like_linear_identifier("MET-fifty"));
    }

    #[test]
    fn resolve_output_path_rejects_escape() {
        let root = PathBuf::from("/tmp/repo");
        let result = resolve_output_path(&root, Path::new("../outside.md"));
        assert!(result.is_err());
    }

    #[test]
    fn default_output_path_uses_workflow_and_issue_slug() {
        let root = PathBuf::from("/tmp/repo");
        let workflow = WorkflowPlaybook {
            name: "ticket-implementation".to_string(),
            summary: "summary".to_string(),
            provider: "codex".to_string(),
            parameters: Vec::new(),
            validation: Vec::new(),
            instructions_template: None,
            prompt_template: "Prompt".to_string(),
            linear_issue_parameter: Some("issue".to_string()),
            source: WorkflowSource::Builtin("builtin/test.md"),
        };
        let values = BTreeMap::from([(String::from("issue"), String::from("MET-50"))]);
        assert_eq!(
            default_output_path(&root, &workflow, &values),
            root.join(".metastack/workflows/generated/ticket-implementation-met-50.md")
        );
    }

    #[test]
    fn edit_mode_discards_or_accepts_markdown_changes() {
        let root = PathBuf::from("/tmp/repo");
        let workflow = WorkflowPlaybook {
            name: "ticket-implementation".to_string(),
            summary: "summary".to_string(),
            provider: "codex".to_string(),
            parameters: vec![WorkflowParameter {
                name: "issue".to_string(),
                description: "Issue".to_string(),
                required: true,
                default: None,
            }],
            validation: Vec::new(),
            instructions_template: None,
            prompt_template: "Prompt".to_string(),
            linear_issue_parameter: Some("issue".to_string()),
            source: WorkflowSource::Builtin("builtin/test.md"),
        };
        let mut app = WorkflowRunApp::new(
            &root,
            workflow,
            BTreeMap::from([(String::from("issue"), String::from("MET-50"))]),
            None,
            false,
            false,
        );
        app.enter_review(WorkflowArtifact {
            markdown: "initial".to_string(),
            provider: "codex".to_string(),
            diagnostics: Vec::new(),
            default_output: root
                .join(".metastack/workflows/generated/ticket-implementation-met-50.md"),
        });

        app.screen = WorkflowRunScreen::Edit;
        let _ = app.handle_edit_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        assert_eq!(app.markdown_editor.value(), "initial!");
        let _ = app.handle_edit_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(
            app.artifact_markdown().expect("artifact markdown"),
            "initial"
        );

        app.screen = WorkflowRunScreen::Edit;
        let _ = app.handle_edit_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE));
        let _ = app.handle_edit_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(
            app.artifact_markdown().expect("artifact markdown"),
            "initial!"
        );
    }
}

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use walkdir::WalkDir;

use crate::agents::{
    render_invocation_diagnostics, resolve_agent_invocation_for_planning, run_agent_capture,
};
use crate::cli::{
    LinearClientArgs, RunAgentArgs, WorkflowCommands, WorkflowRunArgs, WorkflowsArgs,
};
use crate::config::{
    AGENT_ROUTE_AGENTS_WORKFLOWS_RUN, AppConfig, PlanningMeta, is_no_agent_selected_error,
};
use crate::context::{
    load_codebase_context_bundle, load_effective_instructions, load_project_rules_bundle,
    load_workflow_contract, render_repo_map,
};
use crate::fs::{PlanningPaths, canonicalize_existing_dir, display_path};
use crate::linear::IssueSummary;
use crate::load_linear_command_context;

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
    let workflow = library.named(&args.name)?;
    let values = resolve_template_values(&root, workflow, args).await?;
    let instructions = workflow
        .instructions_template
        .as_deref()
        .map(|template| render_template(template, &values))
        .transpose()?
        .filter(|value| !value.trim().is_empty());
    let prompt = render_template(&workflow.prompt_template, &values)?;
    let app_config = AppConfig::load()?;
    let planning_meta = PlanningMeta::load(&root)?;
    let run_args = RunAgentArgs {
        root: Some(root.clone()),
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
    let diagnostics = render_invocation_diagnostics(&invocation);

    if args.dry_run {
        return Ok(render_dry_run(
            &root,
            workflow,
            &invocation.agent,
            &diagnostics,
            instructions.as_deref(),
            &prompt,
        ));
    }

    let output = run_agent_capture(&RunAgentArgs {
        agent: Some(invocation.agent.clone()),
        ..run_args
    })?;

    let mut lines = vec![
        format!(
            "Ran workflow `{}` with provider `{}`.",
            workflow.name, invocation.agent
        ),
        format!("Source: `{}`", workflow.source_label(&root)),
    ];
    lines.extend(diagnostics);

    if !workflow.validation.is_empty() {
        lines.push(String::new());
        lines.push("Validation steps:".to_string());
        for step in &workflow.validation {
            lines.push(format!("- {step}"));
        }
    }

    let stdout = output.stdout.trim();
    if !stdout.is_empty() {
        lines.push(String::new());
        lines.push(stdout.to_string());
    }

    Ok(lines.join("\n"))
}

async fn resolve_template_values(
    root: &Path,
    workflow: &WorkflowPlaybook,
    args: &WorkflowRunArgs,
) -> Result<BTreeMap<String, String>> {
    let mut values = parse_param_assignments(&args.params)?;
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
    }
    if !missing.is_empty() {
        bail!(
            "workflow `{}` is missing required parameters: {}",
            workflow.name,
            missing.join(", ")
        );
    }

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

mod runtime;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use walkdir::WalkDir;

use crate::cli::{
    CronArgs, CronCommands, CronInitArgs, CronInitEventArg, CronListArgs, CronValidateArgs,
};
use crate::config::{
    AGENT_ROUTE_RUNTIME_CRON_PROMPT, AgentConfigOverrides, AppConfig, PlanningMeta,
    detect_supported_agents, normalize_agent_name, resolve_agent_route, resolve_data_root,
};
use crate::cron_dashboard::{
    CronInitAction, CronInitFormContext, CronInitFormExit, CronInitFormOptions,
    CronInitFormPrefill, CronInitFormValues, run_cron_init_form,
};
use crate::fs::{
    PlanningPaths, canonicalize_existing_dir, display_path, ensure_dir, write_text_file,
};
use crate::output::render_json_success;

const CRON_README: &str = r#"# Cron Jobs

Use this directory for repository-local automation jobs managed by `meta cron`.

- One Markdown file per job, such as `nightly.md`
- YAML front matter stores the schedule and command metadata
- Markdown body stores operator notes and future-agent context
- `.runtime/` is created on demand for PID files, logs, and scheduler state
"#;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CronJobFrontMatter {
    schedule: String,
    #[serde(default)]
    mode: CronJobMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(default = "runtime::default_shell")]
    shell: String,
    #[serde(default = "runtime::default_working_directory")]
    working_directory: String,
    #[serde(default = "runtime::default_timeout_seconds")]
    timeout_seconds: u64,
    #[serde(default = "runtime::default_enabled")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    steps: Vec<CronStepDefinition>,
    #[serde(default, skip_serializing_if = "CronRetryPolicy::is_default")]
    retry: CronRetryPolicy,
}

#[derive(Debug, Clone)]
pub(super) struct CronJob {
    name: String,
    relative_path: String,
    absolute_path: PathBuf,
    source: CronDefinitionSource,
    front_matter: CronJobFrontMatter,
    prompt_markdown: Option<String>,
    steps: Vec<CronStepDefinition>,
}

#[derive(Debug)]
pub(super) struct DiscoveredJob {
    name: String,
    relative_path: String,
    source: CronDefinitionSource,
    result: Result<CronJob>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub(super) enum CronJobMode {
    #[default]
    Legacy,
    Workflow,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum CronStepKind {
    Shell,
    Agent,
    Cli,
    Approval,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CronRetryPolicy {
    #[serde(default = "default_max_attempts")]
    max_attempts: u32,
    #[serde(default)]
    backoff_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct CronStepWhen {
    step: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    equals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    not_equals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exists: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct CronStepGuardrails {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mutates: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CronStepDefinition {
    id: String,
    #[serde(rename = "type")]
    kind: CronStepKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(
        default = "runtime::default_shell",
        skip_serializing_if = "is_default_shell"
    )]
    shell: String,
    #[serde(
        default = "runtime::default_working_directory",
        skip_serializing_if = "is_default_working_directory"
    )]
    working_directory: String,
    #[serde(
        default = "runtime::default_timeout_seconds",
        skip_serializing_if = "is_default_timeout_seconds"
    )]
    timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    approval_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    when: Option<CronStepWhen>,
    #[serde(default, skip_serializing_if = "CronStepGuardrails::is_empty")]
    guardrails: CronStepGuardrails,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum CronDefinitionSourceKind {
    Install,
    Repository,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct CronDefinitionSource {
    kind: CronDefinitionSourceKind,
    label: String,
    path: String,
}

impl CronRetryPolicy {
    fn is_default(&self) -> bool {
        self.max_attempts == default_max_attempts() && self.backoff_seconds == 0
    }
}

impl Default for CronRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            backoff_seconds: 0,
        }
    }
}

impl CronStepGuardrails {
    fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.mutates.is_empty()
    }
}

fn default_max_attempts() -> u32 {
    1
}

fn is_default_shell(value: &str) -> bool {
    value == runtime::default_shell()
}

fn is_default_working_directory(value: &str) -> bool {
    value == runtime::default_working_directory()
}

fn is_default_timeout_seconds(value: &u64) -> bool {
    *value == runtime::default_timeout_seconds()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SchedulerState {
    pid: Option<u32>,
    poll_interval_seconds: Option<u64>,
    started_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    log_path: Option<String>,
    jobs: BTreeMap<String, JobRuntimeState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct JobRuntimeState {
    path: String,
    enabled: bool,
    schedule: String,
    log_path: Option<String>,
    next_run_at: Option<DateTime<Utc>>,
    last_started_at: Option<DateTime<Utc>>,
    last_finished_at: Option<DateTime<Utc>>,
    last_succeeded_at: Option<DateTime<Utc>>,
    last_exit_code: Option<i32>,
    last_error: Option<String>,
    last_parse_error: Option<String>,
}

#[derive(Debug, Clone)]
struct ScheduledJob {
    schedule_source: String,
    next_due_at: DateTime<Local>,
}

#[derive(Debug, Clone)]
struct CommandPhaseOutcome {
    executed: bool,
    exit_code: Option<i32>,
    error: Option<String>,
    timed_out: bool,
}

pub fn run_cron(args: &CronArgs) -> Result<Option<String>> {
    let root = canonicalize_existing_dir(&args.root)?;

    match &args.command {
        CronCommands::Init(command) => Ok(Some(run_init(&root, command)?)),
        CronCommands::List(command) => Ok(Some(run_list(&root, command)?)),
        CronCommands::Validate(command) => Ok(Some(run_validate(&root, command)?)),
        CronCommands::Start(command) => runtime::run_start(&root, command),
        CronCommands::Stop => Ok(Some(runtime::run_stop(&root)?)),
        CronCommands::Status => Ok(Some(runtime::run_status(&root)?)),
        CronCommands::Run(command) => Ok(Some(runtime::run_job_now(&root, command)?)),
        CronCommands::Resume(command) => Ok(Some(runtime::run_resume(&root, command)?)),
        CronCommands::Approvals(command) => Ok(Some(runtime::run_approvals(&root, command)?)),
        CronCommands::Approve(command) => Ok(Some(runtime::run_approve(&root, command)?)),
        CronCommands::Reject(command) => Ok(Some(runtime::run_reject(&root, command)?)),
        CronCommands::Daemon(command) => runtime::run_daemon(&root, command),
    }
}

fn run_init(root: &Path, args: &CronInitArgs) -> Result<String> {
    ensure_cron_layout(root)?;
    let config = AppConfig::load()?;
    let planning_meta = PlanningMeta::load(root)?;
    let mut agent_options = available_agent_names(&config);
    if let Some(agent) = args
        .agent
        .as_deref()
        .map(normalize_agent_name)
        .filter(|value| !value.trim().is_empty())
        && !agent_options
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&agent))
    {
        agent_options.push(agent);
    }

    let default_agent = resolve_default_agent_name(&config, &planning_meta, &agent_options);
    let prefill = build_init_prefill(root, args, default_agent.clone())?;
    let options = CronInitFormOptions {
        render_once: args.render_once,
        width: args.width,
        height: args.height,
        actions: args
            .events
            .iter()
            .copied()
            .map(CronInitAction::from)
            .collect(),
        vim_mode: config.vim_mode_enabled(),
    };

    if args.render_once {
        return match run_cron_init_form(CronInitFormContext { agent_options }, prefill, options)? {
            CronInitFormExit::Snapshot(snapshot) => Ok(snapshot),
            CronInitFormExit::Cancelled => Ok("Cancelled cron init.".to_string()),
            CronInitFormExit::Submitted(values) => {
                write_cron_job(root, values, args.force, args.json)
            }
        };
    }

    let can_launch_tui = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let interactive = !args.no_interactive && can_launch_tui;
    let values = if interactive {
        match run_cron_init_form(CronInitFormContext { agent_options }, prefill, options)? {
            CronInitFormExit::Cancelled => return Ok("Cancelled cron init.".to_string()),
            CronInitFormExit::Submitted(values) => values,
            CronInitFormExit::Snapshot(snapshot) => return Ok(snapshot),
        }
    } else {
        run_init_non_interactive(args, default_agent)?
    };

    write_cron_job(
        root,
        values,
        interactive || args.force,
        args.no_interactive || args.json,
    )
}

fn run_list(root: &Path, args: &CronListArgs) -> Result<String> {
    ensure_cron_layout(root)?;
    let discovered = discover_jobs(root)?;

    #[derive(Serialize)]
    struct ListedJob<'a> {
        name: &'a str,
        source: &'a CronDefinitionSource,
        path: &'a str,
        enabled: bool,
        mode: String,
        schedule: &'a str,
        steps: usize,
        valid: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    }

    let listed = discovered
        .iter()
        .map(|item| match &item.result {
            Ok(job) => ListedJob {
                name: &job.name,
                source: &job.source,
                path: &job.relative_path,
                enabled: job.front_matter.enabled,
                mode: mode_label(&job.front_matter.mode),
                schedule: &job.front_matter.schedule,
                steps: job.steps.len(),
                valid: true,
                error: None,
            },
            Err(error) => ListedJob {
                name: &item.name,
                source: &item.source,
                path: &item.relative_path,
                enabled: false,
                mode: mode_label(&CronJobMode::Workflow),
                schedule: "",
                steps: 0,
                valid: false,
                error: Some(format!("{error:#}")),
            },
        })
        .collect::<Vec<_>>();

    if args.json {
        return render_json_success("runtime.cron.list", &listed);
    }

    if listed.is_empty() {
        return Ok("No cron workflow definitions found.".to_string());
    }

    let mut lines = Vec::new();
    for entry in listed {
        lines.push(format!(
            "- {} [{}] {}",
            entry.name,
            if entry.valid { "valid" } else { "invalid" },
            if entry.enabled { "enabled" } else { "disabled" }
        ));
        lines.push(format!("  source: {}", entry.source.label));
        lines.push(format!("  file: {}", entry.path));
        if !entry.schedule.is_empty() {
            lines.push(format!("  schedule: {}", entry.schedule));
            lines.push(format!("  mode: {}", entry.mode));
            lines.push(format!("  steps: {}", entry.steps));
        }
        if let Some(error) = entry.error {
            lines.push(format!("  error: {error}"));
        }
    }
    Ok(lines.join("\n"))
}

fn run_validate(root: &Path, args: &CronValidateArgs) -> Result<String> {
    ensure_cron_layout(root)?;
    let discovered = discover_jobs(root)?;
    let failures = discovered
        .iter()
        .filter_map(|item| item.result.as_ref().err().map(|error| (item, error)))
        .map(|(item, error)| format!("{}: {error:#}", item.relative_path))
        .collect::<Vec<_>>();

    #[derive(Serialize)]
    struct ValidationResult<'a> {
        valid: bool,
        files_checked: usize,
        failures: &'a [String],
    }

    if args.json {
        return render_json_success(
            "runtime.cron.validate",
            &ValidationResult {
                valid: failures.is_empty(),
                files_checked: discovered.len(),
                failures: &failures,
            },
        );
    }

    if failures.is_empty() {
        Ok(format!(
            "Validated {} cron workflow definition(s) successfully.",
            discovered.len()
        ))
    } else {
        bail!("cron workflow validation failed:\n{}", failures.join("\n"))
    }
}

fn run_init_non_interactive(
    args: &CronInitArgs,
    default_agent: Option<String>,
) -> Result<CronInitFormValues> {
    let name = args.name.as_deref().ok_or_else(|| {
        anyhow!(
            "`NAME` is required when `--no-interactive` is used or when `meta cron init` runs without a TTY"
        )
    })?;
    let schedule = args.schedule.as_deref().ok_or_else(|| {
        anyhow!(
            "`--schedule` is required when `--no-interactive` is used or when `meta cron init` runs without a TTY"
        )
    })?;
    validate_job_name(name)?;
    parse_schedule(schedule)?;

    if args.shell.trim().is_empty() {
        bail!("`--shell` must not be empty");
    }
    if args.working_directory.trim().is_empty() {
        bail!("`--working-directory` must not be empty");
    }
    if args.timeout_seconds == 0 {
        bail!("`--timeout-seconds` must be at least 1");
    }

    let prompt = normalize_prompt(args.prompt.as_deref());
    let command = normalize_optional(args.command.as_deref());
    if command.is_none() && prompt.is_none() {
        bail!("either `--command` or `--prompt` is required");
    }
    let explicit_agent = args
        .agent
        .as_deref()
        .map(normalize_agent_name)
        .filter(|value| !value.trim().is_empty());
    if prompt.is_none() && explicit_agent.is_some() {
        bail!("`--prompt` is required when `--agent` is provided");
    }

    let agent = if prompt.is_some() {
        explicit_agent.or(default_agent).ok_or_else(|| {
            anyhow!(
                "an agent is required when `--prompt` is set; pass `--agent <NAME>` or run `meta runtime config`"
            )
        })?
    } else {
        String::new()
    };

    Ok(CronInitFormValues {
        name: name.trim().to_string(),
        schedule: schedule.trim().to_string(),
        command: command.unwrap_or_default(),
        agent: if prompt.is_some() { Some(agent) } else { None },
        prompt,
        shell: args.shell.trim().to_string(),
        working_directory: args.working_directory.trim().to_string(),
        timeout_seconds: args.timeout_seconds,
        enabled: !args.disabled,
    })
}

fn write_cron_job(
    root: &Path,
    values: CronInitFormValues,
    force: bool,
    json_output: bool,
) -> Result<String> {
    let paths = PlanningPaths::new(root);
    let path = paths.cron_job_path(&values.name);
    let front_matter = CronJobFrontMatter {
        schedule: values.schedule,
        mode: CronJobMode::Legacy,
        command: normalize_optional(Some(values.command.as_str())),
        agent: values.agent,
        prompt: None,
        shell: values.shell,
        working_directory: values.working_directory,
        timeout_seconds: values.timeout_seconds,
        enabled: values.enabled,
        steps: Vec::new(),
        retry: CronRetryPolicy::default(),
    };
    let contents = render_job_markdown(&front_matter, values.prompt.as_deref())?;
    let status = write_text_file(&path, &contents, force)?;

    let status_label = match status {
        crate::fs::FileWriteStatus::Created => "created",
        crate::fs::FileWriteStatus::Updated => "updated",
        crate::fs::FileWriteStatus::Unchanged => "reused",
    };

    if json_output {
        #[derive(Serialize)]
        struct CronInitResult<'a> {
            status: &'a str,
            name: &'a str,
            path: String,
            schedule: &'a str,
            enabled: bool,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            command: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            agent: Option<String>,
        }

        return render_json_success(
            "runtime.cron.init",
            &CronInitResult {
                status: status_label,
                name: &values.name,
                path: display_path(&path, root),
                schedule: &front_matter.schedule,
                enabled: front_matter.enabled,
                command: front_matter.command.clone(),
                agent: front_matter.agent.clone(),
            },
        );
    }

    let prefix = match status_label {
        "created" => "Created",
        "updated" => "Updated",
        _ => "Reused",
    };
    Ok(format!(
        "{prefix} cron job template at {}",
        display_path(&path, root)
    ))
}

fn build_init_prefill(
    root: &Path,
    args: &CronInitArgs,
    default_agent: Option<String>,
) -> Result<CronInitFormPrefill> {
    let existing = load_existing_prefill(root, args.name.as_deref())?;
    let mut prefill = existing.unwrap_or_default();

    prefill.name = args.name.clone().or(prefill.name);
    prefill.schedule = args.schedule.clone().or(prefill.schedule);
    prefill.command = args.command.clone().or(prefill.command);
    prefill.agent = args.agent.clone().or(prefill.agent).or(default_agent);
    prefill.prompt = args.prompt.clone().or(prefill.prompt);
    prefill.shell = if args.shell != runtime::default_shell() {
        Some(args.shell.clone())
    } else {
        prefill.shell.or_else(|| Some(args.shell.clone()))
    };
    prefill.working_directory = if args.working_directory != runtime::default_working_directory() {
        Some(args.working_directory.clone())
    } else {
        prefill
            .working_directory
            .or_else(|| Some(args.working_directory.clone()))
    };
    prefill.timeout_seconds = if args.timeout_seconds != runtime::default_timeout_seconds() {
        Some(args.timeout_seconds)
    } else {
        prefill.timeout_seconds.or(Some(args.timeout_seconds))
    };
    prefill.disabled = args.disabled || prefill.disabled;

    Ok(prefill)
}

fn load_existing_prefill(root: &Path, name: Option<&str>) -> Result<Option<CronInitFormPrefill>> {
    let Some(name) = name else {
        return Ok(None);
    };

    let path = PlanningPaths::new(root).cron_job_path(name);
    if !path.exists() {
        return Ok(None);
    }

    let job = load_job(
        root,
        &path,
        CronDefinitionSource {
            kind: CronDefinitionSourceKind::Repository,
            label: "repository".to_string(),
            path: PlanningPaths::new(root).cron_dir.display().to_string(),
        },
    )?;
    Ok(Some(CronInitFormPrefill {
        name: Some(job.name),
        schedule: Some(job.front_matter.schedule),
        command: Some(job.front_matter.command.unwrap_or_default()),
        agent: job.front_matter.agent,
        prompt: job.prompt_markdown,
        shell: Some(job.front_matter.shell),
        working_directory: Some(job.front_matter.working_directory),
        timeout_seconds: Some(job.front_matter.timeout_seconds),
        disabled: !job.front_matter.enabled,
    }))
}

fn available_agent_names(config: &AppConfig) -> Vec<String> {
    let mut names = BTreeSet::new();

    if let Some(default_agent) = config
        .agents
        .default_agent
        .as_deref()
        .map(normalize_agent_name)
        .filter(|value| !value.is_empty())
    {
        names.insert(default_agent);
    }

    for name in config.agents.commands.keys() {
        names.insert(normalize_agent_name(name));
    }
    for route in config.agents.routing.families.values() {
        if let Some(provider) = route.provider.as_deref().map(normalize_agent_name) {
            names.insert(provider);
        }
    }
    for route in config.agents.routing.commands.values() {
        if let Some(provider) = route.provider.as_deref().map(normalize_agent_name) {
            names.insert(provider);
        }
    }

    for name in detect_supported_agents() {
        names.insert(normalize_agent_name(&name));
    }

    names.into_iter().collect()
}

fn resolve_default_agent_name(
    config: &AppConfig,
    planning_meta: &PlanningMeta,
    available_agents: &[String],
) -> Option<String> {
    resolve_agent_route(
        config,
        planning_meta,
        AGENT_ROUTE_RUNTIME_CRON_PROMPT,
        AgentConfigOverrides::default(),
    )
    .ok()
    .map(|resolved| resolved.provider)
    .filter(|candidate| {
        available_agents
            .iter()
            .any(|available| available.eq_ignore_ascii_case(candidate))
    })
    .or_else(|| available_agents.first().cloned())
}

fn render_command_error(job: &CronJob, outcome: &CommandPhaseOutcome) -> Option<String> {
    if !outcome.executed {
        return None;
    }

    if outcome.timed_out {
        return Some(format!(
            "shell command timed out after {}s",
            job.front_matter.timeout_seconds
        ));
    }

    outcome
        .error
        .as_ref()
        .map(|error| format!("shell command failed: {error}"))
}

fn render_agent_prompt(
    job: &CronJob,
    working_directory: &Path,
    display_log_path: &str,
    prompt: &str,
    command_outcome: &CommandPhaseOutcome,
) -> String {
    let command_summary = job
        .front_matter
        .command
        .as_deref()
        .unwrap_or("<not configured>");
    let exit_code_summary = if command_outcome.executed {
        command_outcome
            .exit_code
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unavailable".to_string())
    } else {
        "skipped".to_string()
    };
    let command_phase_summary = if command_outcome.executed {
        "executed"
    } else {
        "skipped"
    };
    let mut lines = vec![
        prompt.trim().to_string(),
        String::new(),
        "## Cron Execution Context".to_string(),
        format!("- Job: {}", job.name),
        format!("- Schedule: {}", job.front_matter.schedule),
        format!("- Working directory: {}", working_directory.display()),
        format!("- Shell command: {command_summary}"),
        format!("- Command phase: {command_phase_summary}"),
        format!("- Command exit code: {exit_code_summary}"),
        format!(
            "- Command timed out: {}",
            if command_outcome.timed_out {
                "yes"
            } else {
                "no"
            }
        ),
        format!("- Log path: {display_log_path}"),
    ];

    if let Some(error) = command_outcome.error.as_deref() {
        lines.push(format!("- Command error: {error}"));
    }
    lines.push(String::new());
    lines.push(
        "Use the shared working directory and log path above when reviewing outputs or updating files."
            .to_string(),
    );

    lines.join("\n")
}

pub(super) fn discover_jobs(root: &Path) -> Result<Vec<DiscoveredJob>> {
    let mut discovered = BTreeMap::new();

    for source in discover_definition_sources(root)? {
        let source_jobs = discover_jobs_in_source(root, &source)?;
        for (name, relative_path, absolute_path) in source_jobs {
            discovered.insert(
                name.clone(),
                DiscoveredJob {
                    name,
                    relative_path,
                    source: source.clone(),
                    result: load_job(root, &absolute_path, source.clone()),
                },
            );
        }
    }

    Ok(discovered.into_values().collect())
}

fn discover_definition_sources(root: &Path) -> Result<Vec<CronDefinitionSource>> {
    let mut sources = Vec::new();
    let install_root = resolve_data_root()?.join("cron");
    if install_root.is_dir() {
        sources.push(CronDefinitionSource {
            kind: CronDefinitionSourceKind::Install,
            label: "install".to_string(),
            path: install_root.display().to_string(),
        });
    }

    let repo_root = PlanningPaths::new(root).cron_dir;
    sources.push(CronDefinitionSource {
        kind: CronDefinitionSourceKind::Repository,
        label: "repository".to_string(),
        path: repo_root.display().to_string(),
    });
    Ok(sources)
}

fn discover_jobs_in_source(
    root: &Path,
    source: &CronDefinitionSource,
) -> Result<Vec<(String, String, PathBuf)>> {
    let base = PathBuf::from(&source.path);
    if !base.is_dir() {
        return Ok(Vec::new());
    }

    let mut paths_to_load = Vec::new();
    for entry in WalkDir::new(&base) {
        let entry = entry.with_context(|| format!("failed to walk `{}`", base.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .components()
            .any(|component| component.as_os_str() == ".runtime")
        {
            continue;
        }
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
        paths_to_load.push(path.to_path_buf());
    }
    paths_to_load.sort();

    let mut discovered = Vec::with_capacity(paths_to_load.len());
    for path in paths_to_load {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow!("cron job path `{}` has no valid file stem", path.display()))?
            .to_string();
        let relative_path = match source.kind {
            CronDefinitionSourceKind::Repository => display_path(&path, root),
            CronDefinitionSourceKind::Install => display_path(&path, &base),
        };
        discovered.push((name, relative_path, path));
    }
    Ok(discovered)
}

pub(super) fn load_job(root: &Path, path: &Path, source: CronDefinitionSource) -> Result<CronJob> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    let (front_matter, body) = split_front_matter(&contents)?;
    let mut front_matter: CronJobFrontMatter =
        serde_yaml::from_str(&front_matter).context("failed to parse YAML front matter")?;
    parse_schedule(&front_matter.schedule)?;
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| anyhow!("cron job path `{}` has no valid file stem", path.display()))?
        .to_string();
    validate_job_name(&name)?;
    let prompt_markdown = normalize_prompt(Some(body.trim()));
    let steps = synthesize_steps(&front_matter, prompt_markdown.as_deref())
        .with_context(|| format!("invalid cron workflow `{}`", path.display()))?;
    validate_front_matter(&front_matter, &steps, path)?;
    if !front_matter.steps.is_empty() {
        front_matter.mode = CronJobMode::Workflow;
    }

    Ok(CronJob {
        name,
        relative_path: match source.kind {
            CronDefinitionSourceKind::Repository => display_path(path, root),
            CronDefinitionSourceKind::Install => {
                let install_root = PathBuf::from(&source.path);
                display_path(path, &install_root)
            }
        },
        absolute_path: path.to_path_buf(),
        source,
        front_matter,
        prompt_markdown,
        steps,
    })
}

fn synthesize_steps(
    front_matter: &CronJobFrontMatter,
    prompt_markdown: Option<&str>,
) -> Result<Vec<CronStepDefinition>> {
    if !front_matter.steps.is_empty() {
        return Ok(front_matter.steps.clone());
    }

    let mut steps = Vec::new();
    if let Some(command) = normalize_optional(front_matter.command.as_deref()) {
        steps.push(CronStepDefinition {
            id: "command".to_string(),
            kind: CronStepKind::Shell,
            name: Some("Command".to_string()),
            command: Some(command),
            args: None,
            agent: None,
            prompt: None,
            shell: front_matter.shell.clone(),
            working_directory: front_matter.working_directory.clone(),
            timeout_seconds: front_matter.timeout_seconds,
            route_key: None,
            approval_message: None,
            when: None,
            guardrails: CronStepGuardrails::default(),
        });
    }
    if let Some(prompt) = normalize_prompt(prompt_markdown.or(front_matter.prompt.as_deref())) {
        steps.push(CronStepDefinition {
            id: "agent".to_string(),
            kind: CronStepKind::Agent,
            name: Some("Agent".to_string()),
            command: None,
            args: None,
            agent: front_matter.agent.clone(),
            prompt: Some(prompt),
            shell: runtime::default_shell(),
            working_directory: front_matter.working_directory.clone(),
            timeout_seconds: front_matter.timeout_seconds,
            route_key: Some(AGENT_ROUTE_RUNTIME_CRON_PROMPT.to_string()),
            approval_message: None,
            when: None,
            guardrails: CronStepGuardrails::default(),
        });
    }
    if steps.is_empty() {
        bail!("either legacy `command`/prompt fields or explicit `steps` are required");
    }
    Ok(steps)
}

fn validate_front_matter(
    front_matter: &CronJobFrontMatter,
    steps: &[CronStepDefinition],
    path: &Path,
) -> Result<()> {
    if front_matter.schedule.trim().is_empty() {
        bail!("{}: `schedule` must not be empty", path.display());
    }
    if front_matter.retry.max_attempts == 0 {
        bail!(
            "{}: `retry.max_attempts` must be at least 1",
            path.display()
        );
    }
    if steps.is_empty() {
        bail!(
            "{}: workflow must contain at least one step",
            path.display()
        );
    }
    let mut seen_ids = BTreeSet::new();
    for step in steps {
        if step.id.trim().is_empty() {
            bail!("{}: step `id` must not be empty", path.display());
        }
        if !seen_ids.insert(step.id.clone()) {
            bail!("{}: duplicate step id `{}`", path.display(), step.id);
        }
        validate_step(step, path, &seen_ids)?;
    }
    Ok(())
}

fn validate_step(
    step: &CronStepDefinition,
    path: &Path,
    seen_ids: &BTreeSet<String>,
) -> Result<()> {
    for target in &step.guardrails.mutates {
        if !step
            .guardrails
            .allow
            .iter()
            .any(|allowed| allowed == target)
        {
            bail!(
                "{}: step `{}` mutates `{target}` but does not allow it in `guardrails.allow`",
                path.display(),
                step.id
            );
        }
    }
    if let Some(condition) = &step.when
        && condition.step.trim().is_empty()
    {
        bail!(
            "{}: step `{}` has `when.step` but it is empty",
            path.display(),
            step.id
        );
    }
    if let Some(condition) = &step.when {
        validate_when_condition(step, condition, path, seen_ids)?;
    }
    match step.kind {
        CronStepKind::Shell => {
            if normalize_optional(step.command.as_deref()).is_none() {
                bail!(
                    "{}: shell step `{}` requires `command`",
                    path.display(),
                    step.id
                );
            }
        }
        CronStepKind::Agent => {
            if normalize_prompt(step.prompt.as_deref()).is_none() {
                bail!(
                    "{}: agent step `{}` requires `prompt`",
                    path.display(),
                    step.id
                );
            }
        }
        CronStepKind::Cli => {
            if normalize_optional(step.command.as_deref()).is_none() {
                bail!(
                    "{}: cli step `{}` requires `command`",
                    path.display(),
                    step.id
                );
            }
        }
        CronStepKind::Approval => {
            if normalize_prompt(step.approval_message.as_deref()).is_none() {
                bail!(
                    "{}: approval step `{}` requires `approval_message`",
                    path.display(),
                    step.id
                );
            }
        }
    }
    Ok(())
}

fn validate_when_condition(
    step: &CronStepDefinition,
    condition: &CronStepWhen,
    path: &Path,
    seen_ids: &BTreeSet<String>,
) -> Result<()> {
    if condition.step == step.id {
        bail!(
            "{}: step `{}` cannot reference itself in `when.step`",
            path.display(),
            step.id
        );
    }
    if !seen_ids.contains(&condition.step) {
        bail!(
            "{}: step `{}` references unknown or later step `{}` in `when.step`",
            path.display(),
            step.id,
            condition.step
        );
    }
    if let Some(path_value) = condition.path.as_deref()
        && path_value.trim().is_empty()
    {
        bail!(
            "{}: step `{}` has `when.path` but it is empty",
            path.display(),
            step.id
        );
    }

    let configured_checks = [
        condition.equals.is_some(),
        condition.not_equals.is_some(),
        condition.exists.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();

    match configured_checks {
        0 => bail!(
            "{}: step `{}` must set one of `when.equals`, `when.not_equals`, or `when.exists`",
            path.display(),
            step.id
        ),
        1 => Ok(()),
        _ => bail!(
            "{}: step `{}` must set only one of `when.equals`, `when.not_equals`, or `when.exists`",
            path.display(),
            step.id
        ),
    }
}

fn render_job_markdown(
    front_matter: &CronJobFrontMatter,
    prompt_markdown: Option<&str>,
) -> Result<String> {
    let mut yaml = serde_yaml::to_string(front_matter).context("failed to render YAML")?;
    if let Some(stripped) = yaml.strip_prefix("---\n") {
        yaml = stripped.to_string();
    }

    let prompt_block = normalize_prompt(prompt_markdown)
        .map(|prompt| format!("\n{prompt}\n"))
        .unwrap_or_else(|| "\n".to_string());
    Ok(format!("---\n{}---\n{}", yaml, prompt_block))
}

fn normalize_prompt(prompt: Option<&str>) -> Option<String> {
    prompt.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn normalize_optional(value: Option<&str>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn mode_label(mode: &CronJobMode) -> String {
    match mode {
        CronJobMode::Legacy => "legacy".to_string(),
        CronJobMode::Workflow => "workflow".to_string(),
    }
}

fn split_front_matter(contents: &str) -> Result<(String, String)> {
    let normalized = contents.replace("\r\n", "\n");
    let remainder = normalized
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow!("cron job Markdown must start with YAML front matter"))?;
    let (front_matter, body) = remainder
        .split_once("\n---\n")
        .ok_or_else(|| anyhow!("cron job Markdown must end front matter with `---`"))?;

    Ok((front_matter.to_string(), body.to_string()))
}

fn parse_schedule(schedule: &str) -> Result<Schedule> {
    let normalized = normalize_schedule(schedule)?;
    Schedule::from_str(&normalized)
        .with_context(|| format!("failed to parse cron schedule `{}`", schedule.trim()))
}

fn normalize_schedule(schedule: &str) -> Result<String> {
    let trimmed = schedule.trim();
    let fields = trimmed.split_whitespace().count();
    match fields {
        5 => Ok(format!("0 {trimmed}")),
        6 | 7 => Ok(trimmed.to_string()),
        _ => bail!(
            "cron schedules must use 5 fields (minute hour day month weekday) or a full 6/7-field expression"
        ),
    }
}

fn next_run_after_now(schedule: &str) -> Result<DateTime<Local>> {
    let schedule = parse_schedule(schedule)?;
    next_after(&schedule, Local::now())
}

fn next_after(schedule: &Schedule, now: DateTime<Local>) -> Result<DateTime<Local>> {
    schedule
        .after(&now)
        .next()
        .ok_or_else(|| anyhow!("schedule has no future execution time"))
}

fn validate_job_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("cron job names must not be empty");
    }

    if !name
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        bail!("cron job names may only contain ASCII letters, digits, `-`, and `_`");
    }

    Ok(())
}

fn ensure_cron_layout(root: &Path) -> Result<()> {
    let paths = PlanningPaths::new(root);
    ensure_dir(&paths.metastack_dir)?;
    ensure_dir(&paths.cron_dir)?;
    write_text_file(&paths.cron_readme_path(), CRON_README, false)?;
    Ok(())
}

impl From<CronInitEventArg> for CronInitAction {
    fn from(value: CronInitEventArg) -> Self {
        match value {
            CronInitEventArg::Up => Self::Up,
            CronInitEventArg::Down => Self::Down,
            CronInitEventArg::Left => Self::Left,
            CronInitEventArg::Right => Self::Right,
            CronInitEventArg::Tab => Self::Tab,
            CronInitEventArg::BackTab => Self::BackTab,
            CronInitEventArg::Save => Self::Save,
            CronInitEventArg::Esc => Self::Esc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CronJobFrontMatter, normalize_schedule, render_job_markdown, split_front_matter};

    #[test]
    fn normalize_schedule_accepts_standard_five_field_input() {
        assert_eq!(
            normalize_schedule("0 * * * *").expect("schedule should normalize"),
            "0 0 * * * *"
        );
    }

    #[test]
    fn rendered_job_round_trips_yaml_front_matter() {
        let front_matter = CronJobFrontMatter {
            schedule: "0 * * * *".to_string(),
            mode: super::CronJobMode::Legacy,
            command: Some("echo hello".to_string()),
            agent: Some("codex".to_string()),
            prompt: None,
            shell: "/bin/sh".to_string(),
            working_directory: ".".to_string(),
            timeout_seconds: 90,
            enabled: true,
            steps: Vec::new(),
            retry: super::CronRetryPolicy::default(),
        };
        let markdown = render_job_markdown(&front_matter, Some("Review the command output"))
            .expect("markdown");
        let (yaml, body) = split_front_matter(&markdown).expect("front matter");
        let reparsed: CronJobFrontMatter = serde_yaml::from_str(&yaml).expect("yaml should parse");

        assert_eq!(reparsed.schedule, "0 * * * *");
        assert_eq!(reparsed.command.as_deref(), Some("echo hello"));
        assert_eq!(reparsed.agent.as_deref(), Some("codex"));
        assert_eq!(reparsed.prompt, None);
        assert!(body.contains("Review the command output"));
    }
}

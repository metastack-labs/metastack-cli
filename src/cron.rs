mod runtime;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::IsTerminal;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};

use crate::cli::{CronArgs, CronCommands, CronInitArgs, CronInitEventArg};
use crate::config::{
    AGENT_ROUTE_RUNTIME_CRON_PROMPT, AgentConfigOverrides, AppConfig, PlanningMeta,
    detect_supported_agents, normalize_agent_name, resolve_agent_route,
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
}

#[derive(Debug, Clone)]
struct CronJob {
    name: String,
    relative_path: String,
    front_matter: CronJobFrontMatter,
    prompt_markdown: Option<String>,
}

#[derive(Debug)]
struct DiscoveredJob {
    name: String,
    relative_path: String,
    result: Result<CronJob>,
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
struct JobExecutionOutcome {
    exit_code: Option<i32>,
    error: Option<String>,
    timed_out: bool,
    log_path: String,
}

#[derive(Debug, Clone)]
struct CommandPhaseOutcome {
    executed: bool,
    exit_code: Option<i32>,
    error: Option<String>,
    timed_out: bool,
}

impl JobExecutionOutcome {
    fn succeeded(&self) -> bool {
        !self.timed_out && self.error.is_none() && self.exit_code.is_none_or(|code| code == 0)
    }
}

pub fn run_cron(args: &CronArgs) -> Result<Option<String>> {
    let root = canonicalize_existing_dir(&args.root)?;

    match &args.command {
        CronCommands::Init(command) => Ok(Some(run_init(&root, command)?)),
        CronCommands::Start(command) => runtime::run_start(&root, command),
        CronCommands::Stop => Ok(Some(runtime::run_stop(&root)?)),
        CronCommands::Status => Ok(Some(runtime::run_status(&root)?)),
        CronCommands::Run(command) => Ok(Some(runtime::run_job_now(&root, command)?)),
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
        command: normalize_optional(Some(values.command.as_str())),
        agent: values.agent,
        prompt: None,
        shell: values.shell,
        working_directory: values.working_directory,
        timeout_seconds: values.timeout_seconds,
        enabled: values.enabled,
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

    let job = load_job(root, &path)?;
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

fn combine_phase_errors(
    command_error: Option<String>,
    agent_error: Option<String>,
) -> Option<String> {
    match (command_error, agent_error) {
        (Some(command_error), Some(agent_error)) => Some(format!("{command_error}; {agent_error}")),
        (Some(command_error), None) => Some(command_error),
        (None, Some(agent_error)) => Some(agent_error),
        (None, None) => None,
    }
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

fn discover_jobs(root: &Path) -> Result<Vec<DiscoveredJob>> {
    let paths = PlanningPaths::new(root);
    if !paths.cron_dir.exists() {
        return Ok(Vec::new());
    }

    let mut paths_to_load = fs::read_dir(&paths.cron_dir)
        .with_context(|| format!("failed to read `{}`", paths.cron_dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .filter(|path| {
            path.file_name()
                .is_some_and(|file_name| file_name != "README.md")
        })
        .collect::<Vec<_>>();
    paths_to_load.sort();

    let mut discovered = Vec::with_capacity(paths_to_load.len());
    for path in paths_to_load {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow!("cron job path `{}` has no valid file stem", path.display()))?
            .to_string();
        let relative_path = display_path(&path, root);
        discovered.push(DiscoveredJob {
            name,
            relative_path,
            result: load_job(root, &path),
        });
    }

    Ok(discovered)
}

fn load_job(root: &Path, path: &Path) -> Result<CronJob> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read `{}`", path.display()))?;
    let (front_matter, body) = split_front_matter(&contents)?;
    let front_matter: CronJobFrontMatter =
        serde_yaml::from_str(&front_matter).context("failed to parse YAML front matter")?;
    parse_schedule(&front_matter.schedule)?;
    validate_job_name(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow!("cron job path `{}` has no valid file stem", path.display()))?,
    )?;

    Ok(CronJob {
        name: path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("validated file stem")
            .to_string(),
        relative_path: display_path(path, root),
        front_matter,
        prompt_markdown: normalize_prompt(Some(body.trim())),
    })
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
            command: Some("echo hello".to_string()),
            agent: Some("codex".to_string()),
            prompt: None,
            shell: "/bin/sh".to_string(),
            working_directory: ".".to_string(),
            timeout_seconds: 90,
            enabled: true,
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

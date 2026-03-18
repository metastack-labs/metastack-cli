use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local, Utc};

use crate::agents::{
    apply_invocation_environment, command_args_for_invocation, render_invocation_diagnostics,
    resolve_agent_invocation_for_planning, validate_invocation_command_surface,
};
use crate::cli::{CronDaemonArgs, CronRunArgs, CronStartArgs, RunAgentArgs};
use crate::config::{
    AGENT_ROUTE_RUNTIME_CRON_PROMPT, AppConfig, PlanningMeta, normalize_agent_name,
};
use crate::fs::{PlanningPaths, display_path, ensure_dir};

use super::{
    CommandPhaseOutcome, CronJob, JobExecutionOutcome, ScheduledJob, SchedulerState,
    combine_phase_errors, discover_jobs, load_job, normalize_prompt, parse_schedule,
    render_agent_prompt, render_command_error,
};

pub(super) fn run_start(root: &Path, args: &CronStartArgs) -> Result<Option<String>> {
    super::ensure_cron_layout(root)?;
    let paths = PlanningPaths::new(root);
    ensure_runtime_layout(&paths)?;

    if let Some(pid) = read_pid(&paths)? {
        if pid_is_running(pid) {
            return Ok(Some(format!(
                "Cron scheduler is already running with pid {pid}. Use `meta cron status` for details."
            )));
        }

        cleanup_stale_pid(&paths)?;
    }

    if args.foreground {
        run_scheduler_loop(root, args.poll_interval_seconds, Some(std::process::id()))?;
        return Ok(None);
    }

    #[cfg(not(unix))]
    {
        bail!(
            "detached cron scheduling is only supported on Unix-like hosts today; rerun with `meta cron start --foreground`"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let current_exe =
            env::current_exe().context("failed to resolve the current `meta` executable")?;
        let log_path = paths.cron_scheduler_log_path();
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("failed to open `{}`", log_path.display()))?;
        let stderr = stdout
            .try_clone()
            .with_context(|| format!("failed to clone `{}`", log_path.display()))?;
        let mut command = ProcessCommand::new(current_exe);
        command
            .current_dir(root)
            .arg("cron")
            .arg("--root")
            .arg(root)
            .arg("daemon")
            .arg("--poll-interval-seconds")
            .arg(args.poll_interval_seconds.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let child = command
            .spawn()
            .context("failed to launch detached cron scheduler")?;

        thread::sleep(Duration::from_millis(250));

        if !pid_is_running(child.id()) {
            bail!(
                "detached cron scheduler exited immediately; inspect `{}` for details or rerun with `meta cron start --foreground`",
                display_path(&log_path, root)
            );
        }

        write_pid(&paths, child.id())?;
        let mut state = load_scheduler_state(&paths)?;
        state.pid = Some(child.id());
        state.poll_interval_seconds = Some(args.poll_interval_seconds.max(1));
        state.started_at = Some(Utc::now());
        state.updated_at = Some(Utc::now());
        state.log_path = Some(display_path(&log_path, root));
        persist_scheduler_state(&paths, &state)?;

        Ok(Some(format!(
            "Started cron scheduler in the background with pid {}. Log: {}",
            child.id(),
            display_path(&log_path, root)
        )))
    }
}

pub(super) fn run_stop(root: &Path) -> Result<String> {
    let paths = PlanningPaths::new(root);
    let Some(pid) = read_pid(&paths)? else {
        return Ok("Cron scheduler is not running.".to_string());
    };

    if !pid_is_running(pid) {
        cleanup_stale_pid(&paths)?;
        return Ok(format!(
            "Cron scheduler was not running; removed stale pid {}.",
            pid
        ));
    }

    #[cfg(not(unix))]
    {
        bail!("`meta cron stop` is only supported on Unix-like hosts today");
    }

    #[cfg(unix)]
    {
        let status = ProcessCommand::new("kill")
            .arg(pid.to_string())
            .status()
            .context("failed to invoke `kill` for the cron scheduler")?;

        if !status.success() {
            bail!("`kill {pid}` exited unsuccessfully");
        }

        for _ in 0..40 {
            if !pid_is_running(pid) {
                break;
            }

            thread::sleep(Duration::from_millis(100));
        }

        if pid_is_running(pid) {
            bail!("cron scheduler pid {pid} did not stop within 4 seconds");
        }

        if paths.cron_scheduler_pid_path().exists() {
            fs::remove_file(paths.cron_scheduler_pid_path()).with_context(|| {
                format!(
                    "failed to remove `{}`",
                    paths.cron_scheduler_pid_path().display()
                )
            })?;
        }

        let mut state = load_scheduler_state(&paths)?;
        state.pid = None;
        state.updated_at = Some(Utc::now());
        persist_scheduler_state(&paths, &state)?;

        Ok(format!("Stopped cron scheduler pid {}.", pid))
    }
}

pub(super) fn run_status(root: &Path) -> Result<String> {
    let paths = PlanningPaths::new(root);
    let state = load_scheduler_state(&paths)?;
    let running_pid = state.pid.filter(|pid| pid_is_running(*pid));
    let discovered = discover_jobs(root)?;

    let mut lines = vec![format!(
        "Cron scheduler: {}",
        running_pid
            .map(|pid| format!("running (pid {pid})"))
            .unwrap_or_else(|| "stopped".to_string())
    )];

    if let Some(updated_at) = state.updated_at {
        lines.push(format!(
            "Last scheduler update: {}",
            format_timestamp(updated_at)
        ));
    }

    if let Some(poll_interval_seconds) = state.poll_interval_seconds {
        lines.push(format!("Poll interval: {}s", poll_interval_seconds));
    }

    if let Some(log_path) = state.log_path.as_deref() {
        lines.push(format!("Scheduler log: {log_path}"));
    }

    if discovered.is_empty() {
        lines.push(format!(
            "No cron jobs found under {}.",
            display_path(&paths.cron_dir, root)
        ));
        return Ok(lines.join("\n"));
    }

    lines.push("Jobs:".to_string());

    for discovered_job in discovered {
        match discovered_job.result {
            Ok(job) => {
                let runtime = state.jobs.get(&job.name);
                lines.push(format!(
                    "- {} [{}]",
                    job.name,
                    if job.front_matter.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                ));
                lines.push(format!("  file: {}", job.relative_path));
                lines.push(format!("  schedule: {}", job.front_matter.schedule));
                if let Some(agent) = job.front_matter.agent.as_deref() {
                    lines.push(format!("  agent: {agent}"));
                }
                if effective_prompt(&job).is_some() {
                    lines.push("  prompt: configured".to_string());
                }

                if job.front_matter.enabled {
                    let next_run = runtime
                        .and_then(|entry| entry.next_run_at)
                        .map(|timestamp| timestamp.with_timezone(&Local))
                        .or_else(|| super::next_run_after_now(&job.front_matter.schedule).ok());
                    if let Some(next_run) = next_run {
                        lines.push(format!("  next run: {}", format_local_timestamp(next_run)));
                    }
                } else {
                    lines.push("  next run: disabled".to_string());
                }

                if let Some(runtime) = runtime {
                    if let Some(last_started_at) = runtime.last_started_at {
                        lines.push(format!(
                            "  last started: {}",
                            format_timestamp(last_started_at)
                        ));
                    }
                    if let Some(last_finished_at) = runtime.last_finished_at {
                        lines.push(format!(
                            "  last finished: {}",
                            format_timestamp(last_finished_at)
                        ));
                    }
                    if let Some(exit_code) = runtime.last_exit_code {
                        lines.push(format!("  last exit code: {exit_code}"));
                    }
                    if let Some(error) = runtime.last_error.as_deref() {
                        lines.push(format!("  last error: {error}"));
                    }
                    if let Some(log_path) = runtime.log_path.as_deref() {
                        lines.push(format!("  log: {log_path}"));
                    }
                } else {
                    lines.push("  last run: never".to_string());
                }
            }
            Err(error) => {
                lines.push(format!("- {} [invalid]", discovered_job.name));
                lines.push(format!("  file: {}", discovered_job.relative_path));
                lines.push(format!("  error: {error:#}"));
            }
        }
    }

    Ok(lines.join("\n"))
}

pub(super) fn run_job_now(root: &Path, args: &CronRunArgs) -> Result<String> {
    super::ensure_cron_layout(root)?;
    let paths = PlanningPaths::new(root);
    ensure_runtime_layout(&paths)?;
    let job = load_job(root, &paths.cron_job_path(&args.name))?;
    let mut state = load_scheduler_state(&paths)?;
    let outcome = execute_job(root, &paths, &job, &mut state)?;
    persist_scheduler_state(&paths, &state)?;

    if outcome.succeeded() {
        Ok(format!(
            "Ran cron job `{}` successfully. Log: {}",
            args.name, outcome.log_path
        ))
    } else if outcome.timed_out {
        bail!(
            "cron job `{}` timed out after {}s; inspect `{}`",
            args.name,
            job.front_matter.timeout_seconds,
            outcome.log_path
        );
    } else if let Some(error) = outcome.error {
        bail!("cron job `{}` failed: {}", args.name, error);
    } else {
        bail!(
            "cron job `{}` exited with {:?}; inspect `{}`",
            args.name,
            outcome.exit_code,
            outcome.log_path
        );
    }
}

pub(super) fn run_daemon(root: &Path, args: &CronDaemonArgs) -> Result<Option<String>> {
    super::ensure_cron_layout(root)?;
    run_scheduler_loop(root, args.poll_interval_seconds, None)?;
    Ok(None)
}

fn run_scheduler_loop(
    root: &Path,
    poll_interval_seconds: u64,
    pid_override: Option<u32>,
) -> Result<()> {
    let paths = PlanningPaths::new(root);
    ensure_runtime_layout(&paths)?;

    let mut state = load_scheduler_state(&paths)?;
    state.pid = Some(pid_override.unwrap_or_else(std::process::id));
    state.poll_interval_seconds = Some(poll_interval_seconds.max(1));
    state.started_at = Some(Utc::now());
    state.updated_at = Some(Utc::now());
    state.log_path = Some(display_path(&paths.cron_scheduler_log_path(), root));
    write_pid(&paths, state.pid.expect("daemon pid must be present"))?;
    persist_scheduler_state(&paths, &state)?;

    let mut schedule_cache: HashMap<String, ScheduledJob> = HashMap::new();
    let poll_interval = Duration::from_secs(poll_interval_seconds.max(1));

    loop {
        let now = Local::now();
        let discovered = discover_jobs(root)?;
        let mut known_jobs = BTreeMap::new();

        for discovered_job in discovered {
            let job_name = discovered_job.name.clone();
            match discovered_job.result {
                Ok(job) => {
                    let schedule = parse_schedule(&job.front_matter.schedule)?;
                    let cache_entry =
                        schedule_cache
                            .entry(job.name.clone())
                            .or_insert_with(|| ScheduledJob {
                                schedule_source: job.front_matter.schedule.clone(),
                                next_due_at: super::next_after(&schedule, now)
                                    .expect("cron schedules should always have a next run"),
                            });

                    if cache_entry.schedule_source != job.front_matter.schedule {
                        cache_entry.schedule_source = job.front_matter.schedule.clone();
                        cache_entry.next_due_at = super::next_after(&schedule, now)
                            .expect("cron schedules should always have a next run");
                    }

                    let runtime = known_jobs
                        .entry(job.name.clone())
                        .or_insert_with(|| state.jobs.get(&job.name).cloned().unwrap_or_default());
                    runtime.path = job.relative_path.clone();
                    runtime.enabled = job.front_matter.enabled;
                    runtime.schedule = job.front_matter.schedule.clone();
                    runtime.log_path =
                        Some(display_path(&paths.cron_job_log_path(&job.name), root));
                    runtime.last_parse_error = None;

                    if job.front_matter.enabled {
                        runtime.next_run_at = Some(cache_entry.next_due_at.with_timezone(&Utc));
                        if cache_entry.next_due_at <= now {
                            let outcome = match execute_job(root, &paths, &job, &mut state) {
                                Ok(outcome) => outcome,
                                Err(error) => {
                                    runtime.last_finished_at = Some(Utc::now());
                                    runtime.last_exit_code = None;
                                    runtime.last_error = Some(error.to_string());
                                    append_scheduler_log(
                                        &paths.cron_scheduler_log_path(),
                                        &format!(
                                            "cron job `{}` hit a scheduler error: {}",
                                            job.name, error
                                        ),
                                    )?;
                                    JobExecutionOutcome {
                                        exit_code: None,
                                        error: Some(error.to_string()),
                                        timed_out: false,
                                        log_path: display_path(
                                            &paths.cron_job_log_path(&job.name),
                                            root,
                                        ),
                                    }
                                }
                            };
                            if let Some(refreshed_runtime) = state.jobs.get(&job.name).cloned() {
                                *runtime = refreshed_runtime;
                            }
                            cache_entry.next_due_at = super::next_after(&schedule, Local::now())
                                .expect("cron schedules should always have a next run");
                            runtime.next_run_at = Some(cache_entry.next_due_at.with_timezone(&Utc));

                            if !outcome.succeeded() {
                                append_scheduler_log(
                                    &paths.cron_scheduler_log_path(),
                                    &format!(
                                        "cron job `{}` failed; inspect `{}`",
                                        job.name, outcome.log_path
                                    ),
                                )?;
                            }
                        }
                    } else {
                        runtime.next_run_at = None;
                    }
                }
                Err(error) => {
                    schedule_cache.remove(&job_name);
                    let runtime = known_jobs.entry(job_name.clone()).or_default();
                    runtime.path = discovered_job.relative_path.clone();
                    runtime.enabled = false;
                    runtime.last_parse_error = Some(error.to_string());
                    runtime.next_run_at = None;
                }
            }
        }

        schedule_cache.retain(|name, _| known_jobs.contains_key(name));
        state.jobs = known_jobs;
        state.updated_at = Some(Utc::now());
        persist_scheduler_state(&paths, &state)?;
        thread::sleep(poll_interval);
    }
}

fn execute_job(
    root: &Path,
    paths: &PlanningPaths,
    job: &CronJob,
    state: &mut SchedulerState,
) -> Result<JobExecutionOutcome> {
    let log_path = paths.cron_job_log_path(&job.name);
    let display_log_path = display_path(&log_path, root);
    let working_directory = resolve_working_directory(root, &job.front_matter.working_directory)?;

    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open `{}`", log_path.display()))?;
    writeln!(
        log,
        "\n[{}] start `{}`\ncommand: {}\nagent: {}\nworking directory: {}\n",
        Local::now().to_rfc3339(),
        job.name,
        job.front_matter.command.as_deref().unwrap_or("<disabled>"),
        job.front_matter.agent.as_deref().unwrap_or("disabled"),
        working_directory.display()
    )
    .with_context(|| format!("failed to write `{}`", log_path.display()))?;

    let started_at = Utc::now();
    let runtime = state.jobs.entry(job.name.clone()).or_default();
    runtime.path = job.relative_path.clone();
    runtime.enabled = job.front_matter.enabled;
    runtime.schedule = job.front_matter.schedule.clone();
    runtime.log_path = Some(display_log_path.clone());
    runtime.last_started_at = Some(started_at);
    runtime.last_error = None;
    runtime.last_parse_error = None;

    let command_outcome = execute_command_phase(job, &working_directory, &log_path, &mut log)?;
    let command_error = render_command_error(job, &command_outcome);
    let agent_error = execute_agent_phase(
        root,
        job,
        &working_directory,
        &display_log_path,
        &log_path,
        &mut log,
        &command_outcome,
    )
    .err()
    .map(|error| error.to_string());
    let final_error = combine_phase_errors(command_error, agent_error);

    let finished_at = Utc::now();
    runtime.last_finished_at = Some(finished_at);
    runtime.last_exit_code = command_outcome.exit_code;
    runtime.last_error = final_error.clone();
    if final_error.is_none() {
        runtime.last_succeeded_at = Some(finished_at);
    }

    writeln!(log, "[{}] job complete\n", Local::now().to_rfc3339())
        .with_context(|| format!("failed to write `{}`", log_path.display()))?;

    Ok(JobExecutionOutcome {
        exit_code: command_outcome.exit_code,
        error: final_error,
        timed_out: command_outcome.timed_out,
        log_path: display_log_path,
    })
}

fn execute_command_phase(
    job: &CronJob,
    working_directory: &Path,
    log_path: &Path,
    log: &mut std::fs::File,
) -> Result<CommandPhaseOutcome> {
    let Some(command) = job.front_matter.command.as_deref() else {
        writeln!(
            log,
            "[{}] command phase skipped\n",
            Local::now().to_rfc3339()
        )
        .with_context(|| format!("failed to write `{}`", log_path.display()))?;
        return Ok(CommandPhaseOutcome {
            executed: false,
            exit_code: None,
            error: None,
            timed_out: false,
        });
    };

    let stdout = log
        .try_clone()
        .with_context(|| format!("failed to clone `{}`", log_path.display()))?;
    let stderr = log
        .try_clone()
        .with_context(|| format!("failed to clone `{}`", log_path.display()))?;

    writeln!(
        log,
        "[{}] command phase start\nshell: {}\n",
        Local::now().to_rfc3339(),
        job.front_matter.shell
    )
    .with_context(|| format!("failed to write `{}`", log_path.display()))?;

    let mut child = match ProcessCommand::new(&job.front_matter.shell)
        .arg("-lc")
        .arg(command)
        .current_dir(working_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            writeln!(
                log,
                "[{}] command phase failed to start: {}\n",
                Local::now().to_rfc3339(),
                error
            )
            .with_context(|| format!("failed to write `{}`", log_path.display()))?;
            return Ok(CommandPhaseOutcome {
                executed: true,
                exit_code: None,
                error: Some(error.to_string()),
                timed_out: false,
            });
        }
    };

    let started = Instant::now();
    let timeout = Duration::from_secs(job.front_matter.timeout_seconds.max(1));
    let mut exit_code = None;
    let mut timed_out = false;
    loop {
        if let Some(status) = child.try_wait()? {
            exit_code = status.code();
            break;
        }

        if started.elapsed() >= timeout {
            timed_out = true;
            child.kill().ok();
            child.wait().ok();
            break;
        }

        thread::sleep(Duration::from_millis(100));
    }

    let error = if timed_out {
        Some(format!(
            "timed out after {}s",
            job.front_matter.timeout_seconds
        ))
    } else if exit_code == Some(0) {
        None
    } else {
        Some(format!("process exited with {:?}", exit_code))
    };

    writeln!(
        log,
        "[{}] command phase finish with {:?}{}\n",
        Local::now().to_rfc3339(),
        exit_code,
        if timed_out { " (timed out)" } else { "" }
    )
    .with_context(|| format!("failed to write `{}`", log_path.display()))?;

    Ok(CommandPhaseOutcome {
        executed: true,
        exit_code,
        error,
        timed_out,
    })
}

fn execute_agent_phase(
    root: &Path,
    job: &CronJob,
    working_directory: &Path,
    display_log_path: &str,
    log_path: &Path,
    log: &mut std::fs::File,
    command_outcome: &CommandPhaseOutcome,
) -> Result<()> {
    let Some(prompt) = effective_prompt(job) else {
        return Ok(());
    };
    let agent_prompt = render_agent_prompt(
        job,
        working_directory,
        display_log_path,
        &prompt,
        command_outcome,
    );
    let run_args = RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_RUNTIME_CRON_PROMPT.to_string()),
        agent: job.front_matter.agent.as_deref().map(normalize_agent_name),
        prompt: agent_prompt,
        instructions: None,
        model: None,
        reasoning: None,
        transport: None,
    };
    let config = AppConfig::load()?;
    let planning_meta = PlanningMeta::load(root)?;
    let invocation = resolve_agent_invocation_for_planning(&config, &planning_meta, &run_args)?;
    let command_args = command_args_for_invocation(&invocation, Some(working_directory))?;
    let attempted_command = validate_invocation_command_surface(&invocation, &command_args)?;
    let stdout = log
        .try_clone()
        .with_context(|| format!("failed to clone `{}`", log_path.display()))?;
    let stderr = log
        .try_clone()
        .with_context(|| format!("failed to clone `{}`", log_path.display()))?;

    writeln!(
        log,
        "[{}] agent phase start\nagent: {}",
        Local::now().to_rfc3339(),
        invocation.agent
    )
    .with_context(|| format!("failed to write `{}`", log_path.display()))?;
    writeln!(
        log,
        "command: {} {}",
        invocation.command,
        command_args.join(" ")
    )
    .with_context(|| format!("failed to write `{}`", log_path.display()))?;
    for line in render_invocation_diagnostics(&invocation) {
        writeln!(log, "{line}")
            .with_context(|| format!("failed to write `{}`", log_path.display()))?;
    }
    writeln!(log).with_context(|| format!("failed to write `{}`", log_path.display()))?;

    let mut command = ProcessCommand::new(&invocation.command);
    command.current_dir(working_directory);
    command.args(&command_args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::from(stdout));
    command.stderr(Stdio::from(stderr));
    apply_invocation_environment(
        &mut command,
        &invocation,
        &run_args.prompt,
        run_args.instructions.as_deref(),
    );
    command.env("METASTACK_CRON_ROOT", root);
    command.env("METASTACK_CRON_JOB_NAME", &job.name);
    command.env("METASTACK_CRON_JOB_PATH", &job.relative_path);
    command.env("METASTACK_CRON_JOB_SCHEDULE", &job.front_matter.schedule);
    command.env(
        "METASTACK_CRON_JOB_COMMAND",
        job.front_matter.command.as_deref().unwrap_or(""),
    );
    command.env("METASTACK_CRON_JOB_LOG_PATH", display_log_path);
    command.env(
        "METASTACK_CRON_JOB_WORKING_DIRECTORY",
        working_directory.display().to_string(),
    );
    command.env(
        "METASTACK_CRON_JOB_COMMAND_EXIT_CODE",
        command_outcome
            .exit_code
            .map(|value| value.to_string())
            .unwrap_or_default(),
    );
    command.env(
        "METASTACK_CRON_JOB_COMMAND_TIMED_OUT",
        if command_outcome.timed_out { "1" } else { "0" },
    );
    command.env(
        "METASTACK_CRON_JOB_COMMAND_ERROR",
        command_outcome.error.as_deref().unwrap_or(""),
    );

    match invocation.transport {
        crate::config::PromptTransport::Arg => {
            command.stdin(Stdio::null());
        }
        crate::config::PromptTransport::Stdin => {
            command.stdin(Stdio::piped());
        }
    }

    let mut child = command.spawn().with_context(|| {
        format!(
            "failed to launch agent `{}` with command `{attempted_command}`",
            invocation.agent
        )
    })?;

    if invocation.transport == crate::config::PromptTransport::Stdin {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("failed to open stdin for cron agent `{}`", invocation.agent))?;
        stdin
            .write_all(invocation.payload.as_bytes())
            .with_context(|| {
                format!(
                    "failed to write prompt payload to agent `{}`",
                    invocation.agent
                )
            })?;
    }

    let status = child
        .wait()
        .with_context(|| format!("failed to wait for cron agent `{}`", invocation.agent))?;
    if !status.success() {
        let code = status
            .code()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "terminated by signal".to_string());
        bail!(
            "agent `{}` exited unsuccessfully ({code}) while running `{attempted_command}`",
            invocation.agent,
        );
    }

    writeln!(log, "[{}] agent phase finish\n", Local::now().to_rfc3339())
        .with_context(|| format!("failed to write `{}`", log_path.display()))?;

    Ok(())
}

pub(super) fn default_shell() -> String {
    "/bin/sh".to_string()
}

pub(super) fn default_working_directory() -> String {
    ".".to_string()
}

pub(super) fn default_timeout_seconds() -> u64 {
    900
}

pub(super) fn default_enabled() -> bool {
    true
}

fn effective_prompt(job: &CronJob) -> Option<String> {
    normalize_prompt(job.prompt_markdown.as_deref())
        .or_else(|| normalize_prompt(job.front_matter.prompt.as_deref()))
}

fn ensure_runtime_layout(paths: &PlanningPaths) -> Result<()> {
    ensure_dir(&paths.cron_runtime_dir)?;
    ensure_dir(&paths.cron_runtime_jobs_dir)?;
    ensure_dir(&paths.cron_runtime_logs_dir)?;
    Ok(())
}

fn resolve_working_directory(root: &Path, working_directory: &str) -> Result<PathBuf> {
    let candidate = root.join(working_directory);
    let canonical = candidate.canonicalize().with_context(|| {
        format!(
            "failed to resolve working directory `{}` from `{}`",
            working_directory,
            root.display()
        )
    })?;

    if !canonical.starts_with(root) {
        bail!(
            "working directory `{}` must stay inside the repository root",
            working_directory
        );
    }

    Ok(canonical)
}

fn load_scheduler_state(paths: &PlanningPaths) -> Result<SchedulerState> {
    let state_path = paths.cron_scheduler_state_path();
    match fs::read_to_string(&state_path) {
        Ok(contents) => serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse `{}`", state_path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SchedulerState::default()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to read `{}`", state_path.display()))
        }
    }
}

fn persist_scheduler_state(paths: &PlanningPaths, state: &SchedulerState) -> Result<()> {
    ensure_runtime_layout(paths)?;
    fs::write(
        paths.cron_scheduler_state_path(),
        serde_json::to_string_pretty(state).context("failed to serialize scheduler state")?,
    )
    .with_context(|| {
        format!(
            "failed to write `{}`",
            paths.cron_scheduler_state_path().display()
        )
    })?;

    for (job_name, job_state) in &state.jobs {
        fs::write(
            paths.cron_job_state_path(job_name),
            serde_json::to_string_pretty(job_state).context("failed to serialize job state")?,
        )
        .with_context(|| {
            format!(
                "failed to write `{}`",
                paths.cron_job_state_path(job_name).display()
            )
        })?;
    }

    Ok(())
}

fn write_pid(paths: &PlanningPaths, pid: u32) -> Result<()> {
    fs::write(paths.cron_scheduler_pid_path(), format!("{pid}\n")).with_context(|| {
        format!(
            "failed to write `{}`",
            paths.cron_scheduler_pid_path().display()
        )
    })
}

fn read_pid(paths: &PlanningPaths) -> Result<Option<u32>> {
    let pid_path = paths.cron_scheduler_pid_path();
    match fs::read_to_string(&pid_path) {
        Ok(contents) => {
            let pid = contents
                .trim()
                .parse::<u32>()
                .with_context(|| format!("failed to parse pid from `{}`", pid_path.display()))?;
            Ok(Some(pid))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("failed to read `{}`", pid_path.display()))
        }
    }
}

fn cleanup_stale_pid(paths: &PlanningPaths) -> Result<()> {
    if paths.cron_scheduler_pid_path().exists() {
        fs::remove_file(paths.cron_scheduler_pid_path()).with_context(|| {
            format!(
                "failed to remove `{}`",
                paths.cron_scheduler_pid_path().display()
            )
        })?;
    }

    let mut state = load_scheduler_state(paths)?;
    state.pid = None;
    state.updated_at = Some(Utc::now());
    persist_scheduler_state(paths, &state)?;
    Ok(())
}

fn append_scheduler_log(path: &Path, message: &str) -> Result<()> {
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open `{}`", path.display()))?;
    writeln!(log, "[{}] {}", Local::now().to_rfc3339(), message)
        .with_context(|| format!("failed to write `{}`", path.display()))
}

fn format_timestamp(timestamp: DateTime<Utc>) -> String {
    format_local_timestamp(timestamp.with_timezone(&Local))
}

fn format_local_timestamp(timestamp: DateTime<Local>) -> String {
    timestamp.format("%Y-%m-%d %H:%M:%S %Z").to_string()
}

#[cfg(unix)]
fn pid_is_running(pid: u32) -> bool {
    ProcessCommand::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn pid_is_running(_pid: u32) -> bool {
    false
}

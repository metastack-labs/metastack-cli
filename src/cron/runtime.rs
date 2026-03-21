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
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agents::{
    apply_invocation_environment, apply_noninteractive_agent_environment,
    command_args_for_invocation, render_invocation_diagnostics,
    resolve_agent_invocation_for_planning, validate_invocation_command_surface,
};
use crate::cli::{
    CronApprovalsArgs, CronApproveArgs, CronDaemonArgs, CronRejectArgs, CronResumeArgs,
    CronRunArgs, CronStartArgs, RunAgentArgs,
};
use crate::config::{
    AGENT_ROUTE_RUNTIME_CRON_PROMPT, AppConfig, PlanningMeta, normalize_agent_name,
};
use crate::fs::{PlanningPaths, display_path, ensure_dir};
use crate::output::render_json_success;

use super::{
    CommandPhaseOutcome, CronDefinitionSource, CronJob, CronRetryPolicy, CronStepDefinition,
    CronStepKind, CronStepWhen, ScheduledJob, SchedulerState, discover_jobs, load_job,
    normalize_prompt, parse_schedule, render_agent_prompt, render_command_error,
};

const RUN_STATE_VERSION: u8 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RunStatus {
    Running,
    WaitingForApproval,
    Succeeded,
    Failed,
    Rejected,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StepStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
    WaitingForApproval,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunAttempt {
    number: u32,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingApproval {
    step_id: String,
    message: String,
    requested_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunStepState {
    id: String,
    kind: CronStepKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    status: StepStatus,
    attempt_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRun {
    version: u8,
    run_id: String,
    job_name: String,
    definition_path: String,
    source: CronDefinitionSource,
    trigger: String,
    status: RunStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_at: Option<DateTime<Utc>>,
    retry: CronRetryPolicy,
    log_path: String,
    steps: Vec<RunStepState>,
    attempts: Vec<RunAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_approval: Option<PendingApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

#[derive(Debug, Clone)]
struct RunExecutionOutcome {
    run_id: String,
    status: RunStatus,
    log_path: String,
    error: Option<String>,
}

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
    let latest_runs = load_latest_runs_by_job(&paths)?;
    let pending_approvals = load_pending_approvals(&paths)?;

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
    lines.push(format!("Pending approvals: {}", pending_approvals.len()));

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
                lines.push(format!("  source: {}", job.source.label));
                lines.push(format!("  schedule: {}", job.front_matter.schedule));
                lines.push(format!("  steps: {}", job.steps.len()));
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
                if let Some(latest_run) = latest_runs.get(&job.name) {
                    lines.push(format!(
                        "  latest run: {} [{}]",
                        latest_run.run_id,
                        run_status_label(&latest_run.status)
                    ));
                } else {
                    lines.push("  latest run: none".to_string());
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

    let discovered = discover_jobs(root)?;
    let job = discovered
        .into_iter()
        .find_map(|item| match item.result {
            Ok(job) if job.name == args.name => Some(Ok(job)),
            Err(error) if item.name == args.name => Some(Err(error)),
            _ => None,
        })
        .ok_or_else(|| anyhow!("cron workflow `{}` was not found", args.name))??;
    let mut state = load_scheduler_state(&paths)?;
    let outcome = execute_new_run(root, &paths, &job, &mut state, "manual")?;
    persist_scheduler_state(&paths, &state)?;
    render_run_outcome(&job.name, outcome)
}

pub(super) fn run_resume(root: &Path, args: &CronResumeArgs) -> Result<String> {
    let paths = PlanningPaths::new(root);
    ensure_runtime_layout(&paths)?;
    let mut state = load_scheduler_state(&paths)?;
    let mut run = load_run(&paths, &args.run_id)?;
    prepare_run_for_resume(&mut run)?;
    let job = load_job(root, Path::new(&run.definition_path), run.source.clone())?;
    let outcome = execute_existing_run(root, &paths, &job, &mut run, &mut state)?;
    persist_scheduler_state(&paths, &state)?;
    render_run_outcome(&job.name, outcome)
}

pub(super) fn run_approvals(root: &Path, args: &CronApprovalsArgs) -> Result<String> {
    let paths = PlanningPaths::new(root);
    ensure_runtime_layout(&paths)?;
    let approvals = load_pending_approvals(&paths)?;

    #[derive(Serialize)]
    struct ApprovalEntry<'a> {
        run_id: &'a str,
        job_name: &'a str,
        step_id: &'a str,
        message: &'a str,
        requested_at: DateTime<Utc>,
        log_path: &'a str,
    }

    let entries = approvals
        .iter()
        .map(|run| {
            let approval = run.pending_approval.as_ref().expect("pending approval");
            ApprovalEntry {
                run_id: &run.run_id,
                job_name: &run.job_name,
                step_id: &approval.step_id,
                message: &approval.message,
                requested_at: approval.requested_at,
                log_path: &run.log_path,
            }
        })
        .collect::<Vec<_>>();

    if args.json {
        return render_json_success("runtime.cron.approvals", &entries);
    }
    if entries.is_empty() {
        return Ok("No pending cron workflow approvals.".to_string());
    }
    let mut lines = Vec::new();
    for entry in entries {
        lines.push(format!("- {} ({})", entry.run_id, entry.job_name));
        lines.push(format!("  step: {}", entry.step_id));
        lines.push(format!(
            "  requested: {}",
            format_timestamp(entry.requested_at)
        ));
        lines.push(format!("  message: {}", entry.message));
        lines.push(format!("  log: {}", entry.log_path));
    }
    Ok(lines.join("\n"))
}

pub(super) fn run_approve(root: &Path, args: &CronApproveArgs) -> Result<String> {
    let paths = PlanningPaths::new(root);
    ensure_runtime_layout(&paths)?;
    let mut state = load_scheduler_state(&paths)?;
    let mut run = load_run(&paths, &args.run_id)?;
    let approval = run
        .pending_approval
        .clone()
        .ok_or_else(|| anyhow!("run `{}` is not waiting for approval", args.run_id))?;
    if run.status != RunStatus::WaitingForApproval {
        bail!("run `{}` is not waiting for approval", args.run_id);
    }
    let job = load_job(root, Path::new(&run.definition_path), run.source.clone())?;
    let step = run
        .steps
        .iter_mut()
        .find(|step| step.id == approval.step_id)
        .ok_or_else(|| {
            anyhow!(
                "run `{}` is missing approval step `{}`",
                run.run_id,
                approval.step_id
            )
        })?;
    step.status = StepStatus::Succeeded;
    step.finished_at = Some(Utc::now());
    step.output = Some(json!({
        "status": "approved",
        "note": args.note,
    }));
    run.pending_approval = None;
    run.status = RunStatus::Interrupted;
    persist_run(&paths, &run)?;
    let outcome = execute_existing_run(root, &paths, &job, &mut run, &mut state)?;
    persist_scheduler_state(&paths, &state)?;
    render_run_outcome(&job.name, outcome)
}

pub(super) fn run_reject(root: &Path, args: &CronRejectArgs) -> Result<String> {
    let paths = PlanningPaths::new(root);
    ensure_runtime_layout(&paths)?;
    let mut run = load_run(&paths, &args.run_id)?;
    let approval = run
        .pending_approval
        .clone()
        .ok_or_else(|| anyhow!("run `{}` is not waiting for approval", args.run_id))?;
    if run.status != RunStatus::WaitingForApproval {
        bail!("run `{}` is not waiting for approval", args.run_id);
    }
    let step = run
        .steps
        .iter_mut()
        .find(|step| step.id == approval.step_id)
        .ok_or_else(|| {
            anyhow!(
                "run `{}` is missing approval step `{}`",
                run.run_id,
                approval.step_id
            )
        })?;
    step.status = StepStatus::Rejected;
    step.finished_at = Some(Utc::now());
    step.error = args
        .reason
        .clone()
        .or_else(|| Some("approval rejected".to_string()));
    step.output = Some(json!({
        "status": "rejected",
        "reason": args.reason,
    }));
    run.pending_approval = None;
    run.status = RunStatus::Rejected;
    run.last_error = args
        .reason
        .clone()
        .or_else(|| Some("approval rejected".to_string()));
    run.finished_at = Some(Utc::now());
    run.updated_at = Utc::now();
    persist_run(&paths, &run)?;
    Ok(format!(
        "Rejected cron workflow run `{}`. Log: {}",
        run.run_id, run.log_path
    ))
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
    reconcile_incomplete_runs(root, &paths, &mut state)?;

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
                    runtime.last_parse_error = None;
                    if job.front_matter.enabled {
                        runtime.next_run_at = Some(cache_entry.next_due_at.with_timezone(&Utc));
                        if cache_entry.next_due_at <= now {
                            let outcome =
                                execute_new_run(root, &paths, &job, &mut state, "scheduled");
                            if let Err(error) = &outcome {
                                append_scheduler_log(
                                    &paths.cron_scheduler_log_path(),
                                    &format!(
                                        "cron workflow `{}` hit a scheduler error: {}",
                                        job.name, error
                                    ),
                                )?;
                            }
                            let _ = outcome?;
                            if let Some(refreshed_runtime) = state.jobs.get(&job.name).cloned() {
                                *runtime = refreshed_runtime;
                            }
                            cache_entry.next_due_at = super::next_after(&schedule, Local::now())
                                .expect("cron schedules should always have a next run");
                            runtime.next_run_at = Some(cache_entry.next_due_at.with_timezone(&Utc));
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

fn execute_new_run(
    root: &Path,
    paths: &PlanningPaths,
    job: &CronJob,
    state: &mut SchedulerState,
    trigger: &str,
) -> Result<RunExecutionOutcome> {
    let mut run = new_run(root, paths, job, trigger)?;
    persist_run(paths, &run)?;
    execute_existing_run(root, paths, job, &mut run, state)
}

fn execute_existing_run(
    root: &Path,
    paths: &PlanningPaths,
    job: &CronJob,
    run: &mut PersistedRun,
    state: &mut SchedulerState,
) -> Result<RunExecutionOutcome> {
    let log_path = root.join(&run.log_path);
    let absolute_log_path = if log_path.is_absolute() {
        log_path
    } else {
        root.join(&run.log_path)
    };
    ensure_runtime_layout(paths)?;
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&absolute_log_path)
        .with_context(|| format!("failed to open `{}`", absolute_log_path.display()))?;

    if run.started_at.is_none() {
        run.started_at = Some(Utc::now());
    }
    run.status = RunStatus::Running;
    run.updated_at = Utc::now();
    persist_run(paths, run)?;

    let next_attempt_number = run.attempts.len() as u32 + 1;
    let final_attempt_number = job.front_matter.retry.max_attempts.max(next_attempt_number);
    for attempt_number in next_attempt_number..=final_attempt_number {
        let attempt_index = run.attempts.len();
        run.attempts.push(RunAttempt {
            number: attempt_number,
            started_at: Utc::now(),
            finished_at: None,
            error: None,
        });
        persist_run(paths, run)?;

        let attempt_result = execute_attempt(root, paths, job, run, state, &mut log);
        run.attempts[attempt_index].finished_at = Some(Utc::now());

        match attempt_result {
            Ok(AttemptProgress::Completed) => {
                run.status = RunStatus::Succeeded;
                run.finished_at = Some(Utc::now());
                run.last_error = None;
                update_job_runtime_state(state, job, run, None);
                persist_run(paths, run)?;
                return Ok(RunExecutionOutcome {
                    run_id: run.run_id.clone(),
                    status: run.status.clone(),
                    log_path: run.log_path.clone(),
                    error: None,
                });
            }
            Ok(AttemptProgress::WaitingForApproval) => {
                run.attempts[attempt_index].error = None;
                update_job_runtime_state(state, job, run, None);
                persist_run(paths, run)?;
                return Ok(RunExecutionOutcome {
                    run_id: run.run_id.clone(),
                    status: run.status.clone(),
                    log_path: run.log_path.clone(),
                    error: None,
                });
            }
            Err(error) => {
                let error_text = error.to_string();
                run.attempts[attempt_index].error = Some(error_text.clone());
                run.last_error = Some(error_text.clone());
                reset_running_steps(run);
                if attempt_number < job.front_matter.retry.max_attempts {
                    run.status = RunStatus::Interrupted;
                    run.updated_at = Utc::now();
                    persist_run(paths, run)?;
                    if job.front_matter.retry.backoff_seconds > 0 {
                        thread::sleep(Duration::from_secs(job.front_matter.retry.backoff_seconds));
                    }
                    continue;
                }

                run.status = RunStatus::Failed;
                run.finished_at = Some(Utc::now());
                update_job_runtime_state(state, job, run, Some(error_text.clone()));
                persist_run(paths, run)?;
                return Ok(RunExecutionOutcome {
                    run_id: run.run_id.clone(),
                    status: run.status.clone(),
                    log_path: run.log_path.clone(),
                    error: Some(error_text),
                });
            }
        }
    }

    bail!("run `{}` exhausted retries unexpectedly", run.run_id)
}

enum AttemptProgress {
    Completed,
    WaitingForApproval,
}

fn execute_attempt(
    root: &Path,
    paths: &PlanningPaths,
    job: &CronJob,
    run: &mut PersistedRun,
    _state: &mut SchedulerState,
    log: &mut std::fs::File,
) -> Result<AttemptProgress> {
    let mut outputs = collect_step_outputs(run);
    for step in &job.steps {
        let step_index = run
            .steps
            .iter()
            .position(|candidate| candidate.id == step.id)
            .ok_or_else(|| anyhow!("run `{}` is missing step `{}`", run.run_id, step.id))?;
        if matches!(
            run.steps[step_index].status,
            StepStatus::Succeeded | StepStatus::Skipped | StepStatus::Rejected
        ) {
            if let Some(output) = &run.steps[step_index].output {
                outputs.insert(step.id.clone(), output.clone());
            }
            continue;
        }

        if !step_should_run(step.when.as_ref(), &outputs)? {
            run.steps[step_index].status = StepStatus::Skipped;
            run.steps[step_index].finished_at = Some(Utc::now());
            run.steps[step_index].output = Some(json!({ "status": "skipped" }));
            persist_run(paths, run)?;
            outputs.insert(step.id.clone(), json!({ "status": "skipped" }));
            continue;
        }

        run.steps[step_index].status = StepStatus::Running;
        run.steps[step_index].attempt_count += 1;
        run.steps[step_index].started_at = Some(Utc::now());
        run.steps[step_index].error = None;
        persist_run(paths, run)?;

        let output = match step.kind {
            CronStepKind::Shell => execute_shell_step(job, step, root, &run.log_path, log)?,
            CronStepKind::Agent => execute_agent_step(
                job,
                step,
                root,
                &run.log_path,
                log,
                &outputs,
                previous_command_context(&job.steps, step, &outputs),
            )?,
            CronStepKind::Cli => execute_cli_step(step, root, &run.log_path, log)?,
            CronStepKind::Approval => {
                let message = normalize_prompt(step.approval_message.as_deref())
                    .ok_or_else(|| anyhow!("approval step `{}` is missing a message", step.id))?;
                run.steps[step_index].status = StepStatus::WaitingForApproval;
                run.steps[step_index].finished_at = None;
                run.steps[step_index].output = Some(json!({ "status": "waiting_for_approval" }));
                run.pending_approval = Some(PendingApproval {
                    step_id: step.id.clone(),
                    message,
                    requested_at: Utc::now(),
                    note: None,
                });
                run.status = RunStatus::WaitingForApproval;
                run.updated_at = Utc::now();
                persist_run(paths, run)?;
                return Ok(AttemptProgress::WaitingForApproval);
            }
        };

        run.steps[step_index].status = StepStatus::Succeeded;
        run.steps[step_index].finished_at = Some(Utc::now());
        run.steps[step_index].output = Some(output.clone());
        outputs.insert(step.id.clone(), output);
        persist_run(paths, run)?;
    }

    Ok(AttemptProgress::Completed)
}

fn execute_shell_step(
    job: &CronJob,
    step: &CronStepDefinition,
    root: &Path,
    display_log_path: &str,
    log: &mut std::fs::File,
) -> Result<Value> {
    let working_directory = resolve_working_directory(root, &step.working_directory)?;
    let step_job = CronJob {
        name: job.name.clone(),
        relative_path: job.relative_path.clone(),
        absolute_path: job.absolute_path.clone(),
        source: job.source.clone(),
        front_matter: super::CronJobFrontMatter {
            schedule: job.front_matter.schedule.clone(),
            mode: super::CronJobMode::Workflow,
            command: step.command.clone(),
            agent: None,
            prompt: None,
            shell: step.shell.clone(),
            working_directory: step.working_directory.clone(),
            timeout_seconds: step.timeout_seconds,
            enabled: true,
            steps: Vec::new(),
            retry: CronRetryPolicy::default(),
        },
        prompt_markdown: None,
        steps: vec![step.clone()],
    };
    let log_path = root.join(display_log_path);
    let outcome = execute_command_phase(&step_job, &working_directory, &log_path, log)?;
    let command_error = render_command_error(&step_job, &outcome);
    if let Some(error) = command_error {
        bail!(error);
    }
    Ok(json!({
        "status": "succeeded",
        "exit_code": outcome.exit_code,
        "error": outcome.error,
        "timed_out": outcome.timed_out,
        "working_directory": working_directory.display().to_string(),
    }))
}

fn execute_agent_step(
    job: &CronJob,
    step: &CronStepDefinition,
    root: &Path,
    display_log_path: &str,
    log: &mut std::fs::File,
    outputs: &BTreeMap<String, Value>,
    command_outcome: CommandPhaseOutcome,
) -> Result<Value> {
    let prompt = normalize_prompt(step.prompt.as_deref())
        .ok_or_else(|| anyhow!("agent step `{}` is missing a prompt", step.id))?;
    let working_directory = resolve_working_directory(root, &step.working_directory)?;
    let rendered_prompt = if outputs.is_empty() {
        prompt
    } else {
        format!(
            "{prompt}\n\n## Prior Step Outputs\n{}",
            serde_json::to_string_pretty(outputs)?
        )
    };
    let step_job = CronJob {
        name: job.name.clone(),
        relative_path: job.relative_path.clone(),
        absolute_path: job.absolute_path.clone(),
        source: job.source.clone(),
        front_matter: super::CronJobFrontMatter {
            schedule: job.front_matter.schedule.clone(),
            mode: super::CronJobMode::Workflow,
            command: None,
            agent: step.agent.clone(),
            prompt: Some(rendered_prompt.clone()),
            shell: step.shell.clone(),
            working_directory: step.working_directory.clone(),
            timeout_seconds: step.timeout_seconds,
            enabled: true,
            steps: Vec::new(),
            retry: CronRetryPolicy::default(),
        },
        prompt_markdown: Some(rendered_prompt.clone()),
        steps: vec![step.clone()],
    };
    execute_agent_phase(
        root,
        &step_job,
        &working_directory,
        display_log_path,
        &root.join(display_log_path),
        log,
        &command_outcome,
    )?;
    Ok(json!({
        "status": "succeeded",
        "agent": step.agent,
        "working_directory": working_directory.display().to_string(),
    }))
}

fn previous_command_context(
    steps: &[CronStepDefinition],
    current_step: &CronStepDefinition,
    outputs: &BTreeMap<String, Value>,
) -> CommandPhaseOutcome {
    let mut context = CommandPhaseOutcome {
        executed: false,
        exit_code: None,
        error: None,
        timed_out: false,
    };
    for step in steps {
        if step.id == current_step.id {
            break;
        }
        if step.kind != CronStepKind::Shell {
            continue;
        }
        let Some(output) = outputs.get(&step.id) else {
            continue;
        };
        context = CommandPhaseOutcome {
            executed: true,
            exit_code: output
                .get("exit_code")
                .and_then(|value| value.as_i64())
                .map(|value| value as i32),
            error: output
                .get("error")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            timed_out: output
                .get("timed_out")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        };
    }
    context
}

fn execute_cli_step(
    step: &CronStepDefinition,
    root: &Path,
    display_log_path: &str,
    log: &mut std::fs::File,
) -> Result<Value> {
    let current_exe =
        env::current_exe().context("failed to resolve the current `meta` executable")?;
    let working_directory = resolve_working_directory(root, &step.working_directory)?;
    let stdout = log
        .try_clone()
        .with_context(|| format!("failed to clone `{display_log_path}`"))?;
    let stderr = log
        .try_clone()
        .with_context(|| format!("failed to clone `{display_log_path}`"))?;
    let command = step
        .command
        .as_deref()
        .ok_or_else(|| anyhow!("cli step `{}` is missing a command", step.id))?;

    writeln!(
        log,
        "[{}] cli step `{}` start\ncommand: meta {} {}\n",
        Local::now().to_rfc3339(),
        step.id,
        command,
        step.args.clone().unwrap_or_default().join(" ")
    )?;

    let status = ProcessCommand::new(current_exe)
        .current_dir(&working_directory)
        .arg(command)
        .args(step.args.clone().unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()
        .with_context(|| format!("failed to execute cli step `{}`", step.id))?;
    if !status.success() {
        bail!(
            "cli step `{}` exited unsuccessfully ({:?})",
            step.id,
            status.code()
        );
    }
    Ok(json!({
        "status": "succeeded",
        "exit_code": status.code(),
        "working_directory": working_directory.display().to_string(),
    }))
}

fn new_run(
    root: &Path,
    paths: &PlanningPaths,
    job: &CronJob,
    trigger: &str,
) -> Result<PersistedRun> {
    let created_at = Utc::now();
    let run_id = format!("{}-{}", job.name, created_at.format("%Y%m%d%H%M%S%.3f"));
    let log_path = display_path(
        &paths.cron_runtime_logs_dir.join(format!("{run_id}.log")),
        root,
    );
    let steps = job
        .steps
        .iter()
        .map(|step| RunStepState {
            id: step.id.clone(),
            kind: step.kind.clone(),
            name: step.name.clone(),
            status: StepStatus::Pending,
            attempt_count: 0,
            started_at: None,
            finished_at: None,
            output: None,
            error: None,
        })
        .collect();

    Ok(PersistedRun {
        version: RUN_STATE_VERSION,
        run_id,
        job_name: job.name.clone(),
        definition_path: job.absolute_path.display().to_string(),
        source: job.source.clone(),
        trigger: trigger.to_string(),
        status: RunStatus::Running,
        created_at,
        updated_at: created_at,
        started_at: None,
        finished_at: None,
        retry: job.front_matter.retry.clone(),
        log_path,
        steps,
        attempts: Vec::new(),
        pending_approval: None,
        last_error: None,
    })
}

fn prepare_run_for_resume(run: &mut PersistedRun) -> Result<()> {
    match run.status {
        RunStatus::Interrupted | RunStatus::Failed | RunStatus::Running => {}
        RunStatus::WaitingForApproval => {
            bail!(
                "run `{}` is waiting for approval; use `meta cron approve` or `meta cron reject`",
                run.run_id
            )
        }
        RunStatus::Succeeded | RunStatus::Rejected => {
            bail!("run `{}` is already terminal", run.run_id)
        }
    }
    reset_running_steps(run);
    run.status = RunStatus::Interrupted;
    run.updated_at = Utc::now();
    Ok(())
}

fn reset_running_steps(run: &mut PersistedRun) {
    for step in &mut run.steps {
        if step.status == StepStatus::Running {
            step.status = StepStatus::Pending;
            step.error = Some("interrupted before completion".to_string());
        }
    }
}

fn step_should_run(
    condition: Option<&CronStepWhen>,
    outputs: &BTreeMap<String, Value>,
) -> Result<bool> {
    let Some(condition) = condition else {
        return Ok(true);
    };
    let source = outputs
        .get(&condition.step)
        .ok_or_else(|| anyhow!("`when.step` references unknown step `{}`", condition.step))?;
    let candidate = if let Some(path) = condition.path.as_deref() {
        extract_output_path(source, path)
            .cloned()
            .unwrap_or(Value::Null)
    } else {
        source.clone()
    };
    if let Some(expected) = &condition.equals {
        return Ok(candidate == *expected);
    }
    if let Some(expected) = &condition.not_equals {
        return Ok(candidate != *expected);
    }
    if let Some(exists) = condition.exists {
        return Ok((candidate != Value::Null) == exists);
    }
    Ok(true)
}

fn extract_output_path<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn collect_step_outputs(run: &PersistedRun) -> BTreeMap<String, Value> {
    run.steps
        .iter()
        .filter_map(|step| step.output.clone().map(|output| (step.id.clone(), output)))
        .collect()
}

fn update_job_runtime_state(
    state: &mut SchedulerState,
    job: &CronJob,
    run: &PersistedRun,
    error: Option<String>,
) {
    let runtime = state.jobs.entry(job.name.clone()).or_default();
    runtime.path = job.relative_path.clone();
    runtime.enabled = job.front_matter.enabled;
    runtime.schedule = job.front_matter.schedule.clone();
    runtime.log_path = Some(run.log_path.clone());
    runtime.last_started_at = run.started_at;
    runtime.last_finished_at = run.finished_at.or(Some(Utc::now()));
    runtime.last_exit_code = None;
    runtime.last_error = error;
    if run.status == RunStatus::Succeeded {
        runtime.last_succeeded_at = run.finished_at;
    }
}

fn persist_run(paths: &PlanningPaths, run: &PersistedRun) -> Result<()> {
    ensure_runtime_layout(paths)?;
    fs::write(
        paths.cron_run_state_path(&run.run_id),
        serde_json::to_string_pretty(run).context("failed to serialize cron run state")?,
    )
    .with_context(|| {
        format!(
            "failed to write `{}`",
            paths.cron_run_state_path(&run.run_id).display()
        )
    })
}

fn load_run(paths: &PlanningPaths, run_id: &str) -> Result<PersistedRun> {
    let path = paths.cron_run_state_path(run_id);
    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("failed to parse `{}`", path.display()))
}

fn load_all_runs(paths: &PlanningPaths) -> Result<Vec<PersistedRun>> {
    ensure_runtime_layout(paths)?;
    if !paths.cron_runtime_runs_dir.exists() {
        return Ok(Vec::new());
    }
    let mut runs = Vec::new();
    for entry in fs::read_dir(&paths.cron_runtime_runs_dir)
        .with_context(|| format!("failed to read `{}`", paths.cron_runtime_runs_dir.display()))?
    {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read `{}`", path.display()))?;
        let run: PersistedRun = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse `{}`", path.display()))?;
        runs.push(run);
    }
    runs.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    Ok(runs)
}

fn load_latest_runs_by_job(paths: &PlanningPaths) -> Result<BTreeMap<String, PersistedRun>> {
    let mut latest = BTreeMap::new();
    for run in load_all_runs(paths)? {
        latest.insert(run.job_name.clone(), run);
    }
    Ok(latest)
}

fn load_pending_approvals(paths: &PlanningPaths) -> Result<Vec<PersistedRun>> {
    Ok(load_all_runs(paths)?
        .into_iter()
        .filter(|run| run.status == RunStatus::WaitingForApproval && run.pending_approval.is_some())
        .collect())
}

fn reconcile_incomplete_runs(
    root: &Path,
    paths: &PlanningPaths,
    state: &mut SchedulerState,
) -> Result<()> {
    for mut run in load_all_runs(paths)? {
        if !matches!(run.status, RunStatus::Running | RunStatus::Interrupted) {
            continue;
        }
        let job = load_job(root, Path::new(&run.definition_path), run.source.clone())?;
        reset_running_steps(&mut run);
        run.status = RunStatus::Interrupted;
        persist_run(paths, &run)?;
        let _ = execute_existing_run(root, paths, &job, &mut run, state)?;
    }
    Ok(())
}

fn render_run_outcome(job_name: &str, outcome: RunExecutionOutcome) -> Result<String> {
    match outcome.status {
        RunStatus::Succeeded => Ok(format!(
            "Ran cron workflow `{job_name}` successfully as `{}`. Log: {}",
            outcome.run_id, outcome.log_path
        )),
        RunStatus::WaitingForApproval => Ok(format!(
            "Cron workflow `{job_name}` is waiting for approval in run `{}`. Log: {}",
            outcome.run_id, outcome.log_path
        )),
        RunStatus::Rejected => Ok(format!(
            "Cron workflow `{job_name}` was rejected in run `{}`. Log: {}",
            outcome.run_id, outcome.log_path
        )),
        RunStatus::Failed | RunStatus::Interrupted | RunStatus::Running => {
            if let Some(error) = outcome.error {
                bail!(
                    "cron workflow `{job_name}` failed in run `{}`: {}",
                    outcome.run_id,
                    error
                );
            }
            bail!(
                "cron workflow `{job_name}` did not complete in run `{}`. Log: {}",
                outcome.run_id,
                outcome.log_path
            );
        }
    }
}

fn run_status_label(status: &RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::WaitingForApproval => "waiting_for_approval",
        RunStatus::Succeeded => "succeeded",
        RunStatus::Failed => "failed",
        RunStatus::Rejected => "rejected",
        RunStatus::Interrupted => "interrupted",
    }
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
    let Some(prompt) = super::normalize_prompt(job.prompt_markdown.as_deref()) else {
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
        attachments: Vec::new(),
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
    )?;
    writeln!(
        log,
        "command: {} {}",
        invocation.command,
        command_args.join(" ")
    )?;
    for line in render_invocation_diagnostics(&invocation) {
        writeln!(log, "{line}")?;
    }
    writeln!(log)?;

    let mut command = ProcessCommand::new(&invocation.command);
    command.current_dir(working_directory);
    command.args(&command_args);
    command.stdin(Stdio::null());
    command.stdout(Stdio::from(stdout));
    command.stderr(Stdio::from(stderr));
    apply_noninteractive_agent_environment(&mut command);
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

    writeln!(log, "[{}] agent phase finish\n", Local::now().to_rfc3339())?;
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

fn ensure_runtime_layout(paths: &PlanningPaths) -> Result<()> {
    ensure_dir(&paths.cron_runtime_dir)?;
    ensure_dir(&paths.cron_runtime_jobs_dir)?;
    ensure_dir(&paths.cron_runtime_logs_dir)?;
    ensure_dir(&paths.cron_runtime_runs_dir)?;
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

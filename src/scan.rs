use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use toml::Value;
use walkdir::{DirEntry, WalkDir};

use crate::agents::{
    AgentExecutionOptions, apply_invocation_environment, command_args_for_invocation,
    render_invocation_diagnostics, resolve_agent_invocation_for_planning,
    validate_invocation_command_surface,
};
use crate::cli::{RunAgentArgs, ScanArgs};
use crate::config::{
    AGENT_ROUTE_CONTEXT_SCAN, AppConfig, PlanningMeta, detect_supported_agents, resolve_agent_route,
};
use crate::context::load_workflow_contract;
use crate::fs::{
    FileWriteStatus, PlanningPaths, canonicalize_existing_dir, display_path, write_text_file,
};
use crate::repo_target::RepoTarget;
use crate::scaffold::ensure_planning_layout;
use crate::scan_dashboard::{ScanDashboard, ScanDashboardData, ScanDashboardRow, ScanItemState};
use crate::scan_prompts::{build_scan_agent_prompt, scan_document_file_names};

const EXCLUDED_DIRS: &[&str] = &[".git", ".metastack", "node_modules", "target"];
const MAX_KEY_FILES: usize = 12;
const SCAN_PROGRESS_POLL_INTERVAL: Duration = Duration::from_millis(80);
const STEP_COLLECT_FACTS: usize = 0;
const STEP_WRITE_FACT_BASE: usize = 1;
const STEP_REFRESH_DOCS: usize = 2;
const STEP_VERIFY_OUTPUTS: usize = 3;

#[derive(Debug, Clone)]
pub struct ScanReport {
    root: PathBuf,
    agent: String,
    log_path: String,
    written_files: Vec<String>,
    removed_files: Vec<String>,
}

#[derive(Debug, Clone)]
struct ScanProgressEntry {
    label: String,
    detail: String,
    state: ScanItemState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    length: u64,
    modified_at: Option<SystemTime>,
}

#[derive(Debug, Clone)]
struct TrackedScanFile {
    path: PathBuf,
    display_path: String,
    detail: String,
    baseline: Option<FileFingerprint>,
    state: ScanItemState,
    agent_output: bool,
}

#[derive(Debug, Clone)]
struct ScanProgress {
    repo_name: String,
    agent: String,
    log_path: String,
    steps: Vec<ScanProgressEntry>,
    files: Vec<TrackedScanFile>,
}

#[derive(Debug, Clone, Default)]
struct RustManifestSummary {
    package_name: Option<String>,
    version: Option<String>,
    edition: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CodebaseContext {
    repo_name: String,
    top_level_entries: Vec<(String, &'static str)>,
    file_count: usize,
    directory_count: usize,
    languages: Vec<(String, usize)>,
    manifests: Vec<String>,
    readme_summary: Option<String>,
    rust_manifest: Option<RustManifestSummary>,
    tree_lines: Vec<String>,
    entrypoints: Vec<String>,
    key_source_files: Vec<String>,
    test_files: Vec<String>,
    doc_files: Vec<String>,
}

pub fn run_scan(args: &ScanArgs) -> Result<ScanReport> {
    run_scan_for_route(args, AGENT_ROUTE_CONTEXT_SCAN)
}

pub(crate) fn run_scan_for_route(args: &ScanArgs, route_key: &str) -> Result<ScanReport> {
    let root = canonicalize_existing_dir(&args.root)?;
    ensure_planning_layout(&root, false)?;
    let paths = PlanningPaths::new(&root);
    let context = CodebaseContext::collect(&root)?;
    let agent = resolve_scan_agent_name(&root, route_key)?;
    let log_path = display_path(&paths.scan_log_path(), &root);
    let mut progress = ScanProgress::new(
        &context.repo_name,
        &agent,
        log_path.clone(),
        tracked_scan_files(&paths, &root),
    );
    progress.set_step(
        STEP_COLLECT_FACTS,
        ScanItemState::Complete,
        format!(
            "Collected {files} file(s) across {directories} directorie(s)",
            files = context.file_count,
            directories = context.directory_count
        ),
    );
    let mut written_files = Vec::new();
    let mut removed_files = Vec::new();

    for legacy_path in paths.legacy_scan_paths() {
        if !has_exact_file_name(&legacy_path)? {
            continue;
        }

        match fs::remove_file(&legacy_path) {
            Ok(()) => removed_files.push(display_path(&legacy_path, &root)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to remove `{}`", legacy_path.display()));
            }
        }
    }

    let scan_path = paths.scan_path();
    let scan_status = write_text_file(&scan_path, &context.render_scan_manual(), true)?;
    let scan_display_path = display_path(&scan_path, &root);
    written_files.push(scan_display_path.clone());
    progress.mark_file_complete(&scan_display_path);
    progress.set_step(
        STEP_WRITE_FACT_BASE,
        ScanItemState::Complete,
        match scan_status {
            FileWriteStatus::Created => format!("Created `{scan_display_path}`"),
            FileWriteStatus::Updated => format!("Updated `{scan_display_path}`"),
            FileWriteStatus::Unchanged => format!("Reused `{scan_display_path}`"),
        },
    );

    let repo_target = RepoTarget::from_root(&root);
    let workflow_contract = load_workflow_contract(&root)?;
    let prompt = build_scan_agent_prompt(
        &repo_target,
        &workflow_contract,
        &context.render_prompt_summary(),
    );
    let run_args = RunAgentArgs {
        root: Some(root.clone()),
        route_key: Some(route_key.to_string()),
        agent: Some(agent.clone()),
        prompt,
        instructions: None,
        model: None,
        reasoning: None,
        transport: None,
    };
    let options = AgentExecutionOptions {
        working_dir: Some(root.clone()),
        extra_env: vec![
            (
                "METASTACK_SCAN_ROOT".to_string(),
                root.display().to_string(),
            ),
            (
                "METASTACK_SCAN_CONTEXT_DIR".to_string(),
                display_path(&paths.codebase_dir, &root),
            ),
            (
                "METASTACK_SCAN_FACT_BASE".to_string(),
                display_path(&scan_path, &root),
            ),
            (
                "METASTACK_SCAN_DOCUMENTS".to_string(),
                scan_document_file_names().join(","),
            ),
        ],
    };

    progress.set_step(
        STEP_REFRESH_DOCS,
        ScanItemState::Running,
        format!(
            "Refreshing `{}` with agent `{agent}`",
            display_path(&paths.codebase_dir, &root)
        ),
    );
    progress.set_step(
        STEP_VERIFY_OUTPUTS,
        ScanItemState::Pending,
        "Waiting for the scan agent to finish".to_string(),
    );
    progress.refresh_agent_files(false);

    let mut dashboard = ScanDashboard::start()?;
    dashboard.draw(&progress.dashboard_data())?;
    run_scan_agent_with_dashboard(
        &run_args,
        options,
        &paths,
        &root,
        &mut progress,
        &mut dashboard,
    )
    .with_context(|| format!("scan agent `{agent}` failed while refreshing codebase docs"))?;
    progress.set_step(
        STEP_REFRESH_DOCS,
        ScanItemState::Complete,
        format!("Agent `{agent}` completed the codebase refresh"),
    );
    progress.set_step(
        STEP_VERIFY_OUTPUTS,
        ScanItemState::Running,
        "Checking that every required planning document is present".to_string(),
    );

    let mut generated_files =
        collect_required_outputs(&paths, &root, &agent).with_context(|| {
            format!(
                "full agent output was saved to `{}`",
                display_path(&paths.scan_log_path(), &root)
            )
        })?;
    progress.refresh_agent_files(true);
    progress.set_step(
        STEP_VERIFY_OUTPUTS,
        ScanItemState::Complete,
        format!(
            "Verified {} agent-authored codebase file(s)",
            generated_files.len()
        ),
    );
    dashboard.draw(&progress.dashboard_data())?;
    written_files.append(&mut generated_files);
    written_files.sort();
    written_files.dedup();
    removed_files.sort();

    Ok(ScanReport {
        root,
        agent,
        log_path,
        written_files,
        removed_files,
    })
}

impl ScanReport {
    pub fn render(&self) -> String {
        let mut lines = vec![format!(
            "Codebase scan completed in {} with agent `{}`.",
            display_path(&self.root.join(".metastack/codebase"), &self.root),
            self.agent,
        )];

        lines.push(String::new());
        lines.push("Steps:".to_string());
        lines.push("  [done] Collect repository facts".to_string());
        lines.push("  [done] Write `.metastack/codebase/SCAN.md`".to_string());
        lines.push(format!(
            "  [done] Refresh reusable codebase docs with agent `{}`",
            self.agent
        ));
        lines.push("  [done] Verify required scan outputs".to_string());

        lines.push(String::new());
        lines.push("Files:".to_string());
        for path in &self.written_files {
            lines.push(format!("  [done] {path}"));
        }

        if !self.removed_files.is_empty() {
            lines.push(String::new());
            lines.push("Removed stale files:".to_string());
            for path in &self.removed_files {
                lines.push(format!("  [done] {path}"));
            }
        }

        lines.push(String::new());
        lines.push(format!("Agent log kept off-screen: {}", self.log_path));

        lines.join("\n")
    }
}

impl ScanProgress {
    fn new(repo_name: &str, agent: &str, log_path: String, files: Vec<TrackedScanFile>) -> Self {
        Self {
            repo_name: repo_name.to_string(),
            agent: agent.to_string(),
            log_path,
            steps: vec![
                ScanProgressEntry {
                    label: "Collect repository facts".to_string(),
                    detail: "Walking the repository and building the fact base".to_string(),
                    state: ScanItemState::Running,
                },
                ScanProgressEntry {
                    label: "Write `.metastack/codebase/SCAN.md`".to_string(),
                    detail: "Preparing the deterministic scan snapshot".to_string(),
                    state: ScanItemState::Pending,
                },
                ScanProgressEntry {
                    label: format!("Refresh codebase docs with `{agent}`"),
                    detail: "Waiting for the fact base to finish".to_string(),
                    state: ScanItemState::Pending,
                },
                ScanProgressEntry {
                    label: "Verify required codebase docs".to_string(),
                    detail: "Waiting for the scan agent".to_string(),
                    state: ScanItemState::Pending,
                },
            ],
            files,
        }
    }

    fn set_step(&mut self, index: usize, state: ScanItemState, detail: String) {
        if let Some(step) = self.steps.get_mut(index) {
            step.state = state;
            step.detail = detail;
        }
    }

    fn mark_file_complete(&mut self, display_path: &str) {
        if let Some(file) = self
            .files
            .iter_mut()
            .find(|file| file.display_path == display_path)
        {
            file.state = ScanItemState::Complete;
        }
    }

    fn refresh_agent_files(&mut self, finished: bool) {
        for file in self.files.iter_mut().filter(|file| file.agent_output) {
            file.state = match fingerprint(&file.path) {
                Some(_) if finished => ScanItemState::Complete,
                Some(current) if Some(current) != file.baseline => ScanItemState::Running,
                Some(_) => ScanItemState::Pending,
                None if finished => ScanItemState::Failed,
                None => ScanItemState::Pending,
            };
        }
    }

    fn fail_refresh_step(&mut self, detail: String) {
        self.set_step(STEP_REFRESH_DOCS, ScanItemState::Failed, detail);
        self.set_step(
            STEP_VERIFY_OUTPUTS,
            ScanItemState::Failed,
            "Skipped because the scan agent did not finish successfully".to_string(),
        );
        self.refresh_agent_files(false);
    }

    fn dashboard_data(&self) -> ScanDashboardData {
        ScanDashboardData {
            title: format!("Codebase scan for {}", self.repo_name),
            status_line: format!(
                "Using agent `{}` to refresh reusable planning context",
                self.agent
            ),
            steps: self
                .steps
                .iter()
                .map(|step| ScanDashboardRow {
                    label: step.label.clone(),
                    detail: step.detail.clone(),
                    state: step.state,
                })
                .collect(),
            files: self
                .files
                .iter()
                .map(|file| ScanDashboardRow {
                    label: file.display_path.clone(),
                    detail: file.detail.clone(),
                    state: file.state,
                })
                .collect(),
            log_path: self.log_path.clone(),
        }
    }
}

fn tracked_scan_files(paths: &PlanningPaths, root: &Path) -> Vec<TrackedScanFile> {
    let mut files = Vec::new();

    files.push(TrackedScanFile {
        path: paths.scan_path(),
        display_path: display_path(&paths.scan_path(), root),
        detail: "deterministic fact base".to_string(),
        baseline: None,
        state: ScanItemState::Pending,
        agent_output: false,
    });

    for path in [
        paths.architecture_path(),
        paths.concerns_path(),
        paths.conventions_path(),
        paths.integrations_path(),
        paths.stack_path(),
        paths.structure_path(),
        paths.testing_path(),
    ] {
        files.push(TrackedScanFile {
            display_path: display_path(&path, root),
            detail: "agent-authored context".to_string(),
            baseline: fingerprint(&path),
            path,
            state: ScanItemState::Pending,
            agent_output: true,
        });
    }

    files
}

fn fingerprint(path: &Path) -> Option<FileFingerprint> {
    let metadata = fs::metadata(path).ok()?;
    Some(FileFingerprint {
        length: metadata.len(),
        modified_at: metadata.modified().ok(),
    })
}

fn run_scan_agent_with_dashboard(
    args: &RunAgentArgs,
    options: AgentExecutionOptions,
    paths: &PlanningPaths,
    root: &Path,
    progress: &mut ScanProgress,
    dashboard: &mut ScanDashboard,
) -> Result<()> {
    let config = AppConfig::load()?;
    let planning_meta = PlanningMeta::load(root)?;
    let invocation = resolve_agent_invocation_for_planning(&config, &planning_meta, args)?;
    let command_args = command_args_for_invocation(&invocation, options.working_dir.as_deref())?;
    let attempted_command = validate_invocation_command_surface(&invocation, &command_args)?;
    let log_path = paths.scan_log_path();
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }

    let mut log = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .with_context(|| format!("failed to open `{}`", log_path.display()))?;
    writeln!(log, "# meta scan agent log")?;
    writeln!(log, "agent: {}", invocation.agent)?;
    writeln!(
        log,
        "working directory: {}",
        options.working_dir.as_deref().unwrap_or(root).display()
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

    let mut command = Command::new(&invocation.command);
    command.args(&command_args);
    if let Some(working_dir) = &options.working_dir {
        command.current_dir(working_dir);
    }
    command.stdout(Stdio::from(log.try_clone()?));
    command.stderr(Stdio::from(log.try_clone()?));
    apply_invocation_environment(
        &mut command,
        &invocation,
        &args.prompt,
        args.instructions.as_deref(),
    );
    for (key, value) in &options.extra_env {
        command.env(key, value);
    }

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
            .ok_or_else(|| anyhow!("failed to open stdin for agent `{}`", invocation.agent))?;
        stdin
            .write_all(invocation.payload.as_bytes())
            .with_context(|| {
                format!(
                    "failed to write prompt payload to agent `{}`",
                    invocation.agent
                )
            })?;
    }

    loop {
        progress.refresh_agent_files(false);
        dashboard.draw(&progress.dashboard_data())?;

        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("failed to wait for agent `{}`", invocation.agent))?
        {
            if !status.success() {
                let code = status
                    .code()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "terminated by signal".to_string());
                let log_display = display_path(&log_path, root);
                progress.fail_refresh_step(format!(
                    "Agent exited unsuccessfully ({code}); inspect `{log_display}` if needed"
                ));
                dashboard.draw(&progress.dashboard_data())?;
                bail!(
                    "agent `{}` exited unsuccessfully ({code}); full agent output was saved to `{}`",
                    invocation.agent,
                    log_display
                );
            }
            break;
        }

        thread::sleep(SCAN_PROGRESS_POLL_INTERVAL);
    }

    Ok(())
}

impl CodebaseContext {
    pub(crate) fn collect(root: &Path) -> Result<Self> {
        let repo_name = root
            .file_name()
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let top_level_entries = collect_top_level_entries(root)?;
        let readme_summary = read_readme_summary(root)?;
        let rust_manifest = read_rust_manifest(root)?;
        let manifests = collect_manifests(root)?;
        let (file_count, directory_count, language_counts) = collect_stats(root)?;
        let mut languages: Vec<_> = language_counts.into_iter().collect();
        languages.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

        Ok(Self {
            repo_name,
            top_level_entries,
            file_count,
            directory_count,
            languages,
            manifests,
            readme_summary,
            rust_manifest,
            tree_lines: build_tree(root, 3)?,
            entrypoints: collect_files(root, MAX_KEY_FILES, |relative, path| {
                relative == "src/main.rs"
                    || relative == "src/lib.rs"
                    || relative.starts_with("src/bin/")
                    || is_script_entrypoint(relative, path)
            })?,
            key_source_files: collect_files(root, MAX_KEY_FILES, |relative, path| {
                relative.starts_with("src/")
                    && path
                        .extension()
                        .and_then(|value| value.to_str())
                        .is_some_and(|extension| {
                            matches!(extension, "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "go")
                        })
            })?,
            test_files: collect_files(root, MAX_KEY_FILES, |relative, path| {
                relative.starts_with("tests/")
                    || relative.contains("/tests/")
                    || path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .is_some_and(|name| {
                            let lowercase = name.to_ascii_lowercase();
                            lowercase.contains("test") || lowercase.contains("spec")
                        })
            })?,
            doc_files: collect_files(root, MAX_KEY_FILES, |relative, path| {
                relative == "README.md"
                    || relative.starts_with("docs/")
                    || path
                        .extension()
                        .and_then(|value| value.to_str())
                        .is_some_and(|extension| extension == "md")
                        && (relative.starts_with("docs/") || relative.ends_with(".md"))
            })?,
        })
    }

    pub(crate) fn render_scan_manual(&self) -> String {
        let mut lines = vec![
            "# Scan".to_string(),
            String::new(),
            "_Manual directory sweep used as the fact base for the scan agent._".to_string(),
            String::new(),
            format!("Repository: `{}`", self.repo_name),
            format!("Files scanned: `{}`", self.file_count),
            format!("Directories scanned: `{}`", self.directory_count),
            String::new(),
            "## Summary".to_string(),
            String::new(),
        ];

        if let Some(manifest) = &self.rust_manifest
            && let Some(package_name) = &manifest.package_name
        {
            lines.push(format!("Root Rust package: `{package_name}`"));
            if let Some(version) = &manifest.version {
                lines.push(format!("Root Rust version: `{version}`"));
            }
            if let Some(edition) = &manifest.edition {
                lines.push(format!("Root Rust edition: `{edition}`"));
            }
            lines.push(String::new());
        }

        if let Some(summary) = &self.readme_summary {
            lines.push(summary.clone());
        } else {
            lines.push(
                "No top-level README summary was detected; the scan relies on filesystem facts only."
                    .to_string(),
            );
        }

        lines.push(String::new());
        lines.push("## Top-Level Layout".to_string());
        lines.push(String::new());
        for (entry, kind) in &self.top_level_entries {
            lines.push(format!("- `{entry}` ({kind})"));
        }

        lines.push(String::new());
        lines.push("## Detected Manifests".to_string());
        lines.push(String::new());
        if self.manifests.is_empty() {
            lines.push("- No known manifest files were detected.".to_string());
        } else {
            for manifest in &self.manifests {
                lines.push(format!("- `{manifest}`"));
            }
        }

        lines.push(String::new());
        lines.push("## Language Footprint".to_string());
        lines.push(String::new());
        if self.languages.is_empty() {
            lines.push("- No source files were detected.".to_string());
        } else {
            for (language, count) in &self.languages {
                lines.push(format!("- `{language}`: {count} file(s)"));
            }
        }

        lines.push(String::new());
        lines.push("## Candidate Entry Points".to_string());
        lines.push(String::new());
        push_list(
            &mut lines,
            &self.entrypoints,
            "No conventional entry points were detected.",
        );

        lines.push(String::new());
        lines.push("## Key Source Files".to_string());
        lines.push(String::new());
        push_list(
            &mut lines,
            &self.key_source_files,
            "No representative source files were detected from the scan window.",
        );

        lines.push(String::new());
        lines.push("## Exclusions".to_string());
        lines.push(String::new());
        for entry in EXCLUDED_DIRS {
            lines.push(format!("- `{entry}`"));
        }

        lines.join("\n")
    }

    pub(crate) fn render_prompt_summary(&self) -> String {
        let mut lines = vec![
            format!("- Repository: `{}`", self.repo_name),
            format!("- Files scanned: `{}`", self.file_count),
            format!("- Directories scanned: `{}`", self.directory_count),
        ];

        if let Some(summary) = &self.readme_summary {
            lines.push(format!("- README summary: {}", summary.trim()));
        }

        lines.push(String::new());
        lines.push("Top-level entries:".to_string());
        push_list(
            &mut lines,
            &self
                .top_level_entries
                .iter()
                .map(|(entry, kind)| format!("{entry} ({kind})"))
                .collect::<Vec<_>>(),
            "No top-level entries detected.",
        );

        lines.push(String::new());
        lines.push("Detected manifests:".to_string());
        push_list(
            &mut lines,
            &self.manifests,
            "No known manifest files were detected.",
        );

        lines.push(String::new());
        lines.push("Candidate entry points:".to_string());
        push_list(
            &mut lines,
            &self.entrypoints,
            "No conventional entry points were detected.",
        );

        lines.push(String::new());
        lines.push("Representative source files:".to_string());
        push_list(
            &mut lines,
            &self.key_source_files,
            "No representative source files were detected.",
        );

        lines.push(String::new());
        lines.push("Representative test files:".to_string());
        push_list(
            &mut lines,
            &self.test_files,
            "No representative test files were detected.",
        );

        lines.push(String::new());
        lines.push("Representative docs:".to_string());
        push_list(
            &mut lines,
            &self.doc_files,
            "No documentation files were detected beyond generated planning context.",
        );

        lines.push(String::new());
        lines.push("Shallow tree snapshot:".to_string());
        push_list(
            &mut lines,
            &self.tree_lines,
            "No files or directories were detected in the scan window.",
        );

        lines.join("\n")
    }
}

fn resolve_scan_agent_name(root: &Path, route_key: &str) -> Result<String> {
    let config = AppConfig::load()?;
    let planning_meta = PlanningMeta::load(root)?;
    if let Ok(resolved) = resolve_agent_route(
        &config,
        &planning_meta,
        route_key,
        crate::config::AgentConfigOverrides::default(),
    ) {
        return Ok(resolved.provider);
    }

    detect_supported_agents()
        .into_iter()
        .next()
        .ok_or_else(|| {
            anyhow!(
                "`meta scan` requires a local agent. Run `meta config` or `meta setup` to configure one, or install a supported agent such as `codex` or `claude`."
            )
        })
}

fn collect_required_outputs(
    paths: &PlanningPaths,
    root: &Path,
    agent: &str,
) -> Result<Vec<String>> {
    let required_paths = [
        paths.architecture_path(),
        paths.concerns_path(),
        paths.conventions_path(),
        paths.integrations_path(),
        paths.stack_path(),
        paths.structure_path(),
        paths.testing_path(),
    ];
    let mut missing = Vec::new();
    let mut written = Vec::new();

    for path in required_paths {
        if path.is_file() {
            written.push(display_path(&path, root));
        } else {
            missing.push(display_path(&path, root));
        }
    }

    if !missing.is_empty() {
        bail!(
            "scan agent `{agent}` did not create required codebase files: {}",
            missing.join(", ")
        );
    }

    written.sort();
    Ok(written)
}

fn push_list(lines: &mut Vec<String>, entries: &[String], empty_line: &str) {
    if entries.is_empty() {
        lines.push(format!("- {empty_line}"));
        return;
    }

    for entry in entries {
        lines.push(format!("- `{entry}`"));
    }
}

fn collect_top_level_entries(root: &Path) -> Result<Vec<(String, &'static str)>> {
    let mut entries = Vec::new();

    for entry in
        fs::read_dir(root).with_context(|| format!("failed to read `{}`", root.display()))?
    {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        if EXCLUDED_DIRS.contains(&file_name.as_str()) {
            continue;
        }

        let kind = if entry.file_type()?.is_dir() {
            "directory"
        } else {
            "file"
        };
        entries.push((file_name, kind));
    }

    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

fn read_readme_summary(root: &Path) -> Result<Option<String>> {
    let path = root.join("README.md");
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    let summary = contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned);

    Ok(summary)
}

fn read_rust_manifest(root: &Path) -> Result<Option<RustManifestSummary>> {
    let path = root.join("Cargo.toml");
    if !path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&path)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    let value: Value = toml::from_str(&contents)
        .with_context(|| format!("failed to parse `{}`", path.display()))?;
    let package = value.get("package").and_then(Value::as_table);

    Ok(Some(RustManifestSummary {
        package_name: package
            .and_then(|table| table.get("name"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        version: package
            .and_then(|table| table.get("version"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        edition: package
            .and_then(|table| table.get("edition"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }))
}

fn collect_manifests(root: &Path) -> Result<Vec<String>> {
    let known = BTreeSet::from([
        "Cargo.toml",
        "package.json",
        "pnpm-lock.yaml",
        "package-lock.json",
        "yarn.lock",
        "go.mod",
        "pyproject.toml",
        "requirements.txt",
        "Gemfile",
        "mix.exs",
    ]);
    let mut manifests = BTreeSet::new();

    for entry in walk_entries(root) {
        let entry = entry?;
        if entry.file_type().is_file() {
            let name = entry.file_name().to_string_lossy().to_string();
            if known.contains(name.as_str()) {
                manifests.insert(display_path(entry.path(), root));
            }
        }
    }

    Ok(manifests.into_iter().collect())
}

fn collect_stats(root: &Path) -> Result<(usize, usize, BTreeMap<String, usize>)> {
    let mut files = 0;
    let mut directories = 0;
    let mut languages = BTreeMap::new();

    for entry in walk_entries(root) {
        let entry = entry?;
        if entry.depth() == 0 {
            continue;
        }

        if entry.file_type().is_dir() {
            directories += 1;
            continue;
        }

        files += 1;
        if let Some(language) = detect_language(entry.path()) {
            *languages.entry(language.to_string()).or_insert(0) += 1;
        }
    }

    Ok((files, directories, languages))
}

fn build_tree(root: &Path, max_depth: usize) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for entry in walk_entries(root) {
        let entry = entry?;
        if entry.depth() == 0 || entry.depth() > max_depth {
            continue;
        }

        let indent = "  ".repeat(entry.depth().saturating_sub(1));
        let suffix = if entry.file_type().is_dir() { "/" } else { "" };
        lines.push(format!(
            "{indent}{}{suffix}",
            display_path(entry.path(), root)
        ));
    }

    Ok(lines)
}

fn collect_files<F>(root: &Path, limit: usize, mut predicate: F) -> Result<Vec<String>>
where
    F: FnMut(&str, &Path) -> bool,
{
    let mut matches = BTreeSet::new();

    for entry in walk_entries(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let relative = display_path(entry.path(), root);
        if predicate(&relative, entry.path()) {
            matches.insert(relative);
        }

        if matches.len() >= limit {
            break;
        }
    }

    Ok(matches.into_iter().collect())
}

fn is_script_entrypoint(relative: &str, path: &Path) -> bool {
    matches!(
        path.extension().and_then(|value| value.to_str()),
        Some("sh" | "py")
    ) && (relative.starts_with("bin/") || relative.starts_with("scripts/"))
}

fn walk_entries(root: &Path) -> impl Iterator<Item = Result<DirEntry, walkdir::Error>> {
    WalkDir::new(root)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| !should_skip(entry))
}

fn should_skip(entry: &DirEntry) -> bool {
    if entry.depth() == 0 {
        return false;
    }

    let name = entry.file_name().to_string_lossy();
    entry.file_type().is_dir() && EXCLUDED_DIRS.contains(&name.as_ref())
}

fn detect_language(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|value| value.to_str()) {
        Some("rs") => Some("Rust"),
        Some("ts") | Some("tsx") => Some("TypeScript"),
        Some("js") | Some("jsx") => Some("JavaScript"),
        Some("py") => Some("Python"),
        Some("go") => Some("Go"),
        Some("java") => Some("Java"),
        Some("rb") => Some("Ruby"),
        Some("md") => Some("Markdown"),
        Some("toml") => Some("TOML"),
        Some("yaml") | Some("yml") => Some("YAML"),
        Some("json") => Some("JSON"),
        Some("sh") => Some("Shell"),
        _ => None,
    }
}

fn has_exact_file_name(path: &Path) -> Result<bool> {
    let Some(parent) = path.parent() else {
        return Ok(false);
    };
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return Ok(false);
    };

    let read_dir = match fs::read_dir(parent) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read `{}`", parent.display()));
        }
    };

    for entry in read_dir {
        let entry = entry?;
        if entry.file_name().to_string_lossy() == file_name {
            return Ok(true);
        }
    }

    Ok(false)
}

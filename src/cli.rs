use std::ffi::OsString;
use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

use crate::tui::prompt_images::PromptImageAttachment;

const ROOT_HELP_EXAMPLES: &str = "\
Example flows:
  engineer:
    meta runtime setup --root . --team MET --project \"MetaStack CLI\"
    meta context scan --root .
    meta backlog plan --root . --request \"Break the next release into tickets\"

  team lead:
    meta linear issues list --team MET --state \"In Progress\"
    meta dashboard team --team MET --project \"MetaStack CLI\"

  ops operator:
    meta agents listen --team MET --project \"MetaStack CLI\"
    meta dashboard ops --root .";

const BACKLOG_HELP_EXAMPLES: &str = "\
Examples:
  meta backlog plan --root . --request \"Split the onboarding work into tickets\"
  meta backlog plan --root . ENG-10144
  meta backlog plan --root . ENG-10144 --velocity
  meta backlog improve --root . --mode basic
  meta backlog improve --root . ENG-10144 --mode advanced --apply
  meta backlog tech MET-35
  meta backlog split MET-35
  meta backlog sync status
  meta backlog sync link MET-35 --entry manual-notes --pull
  meta backlog sync pull --all
  meta backlog sync push MET-35 --update-description";

const BACKLOG_IMPROVE_HELP: &str = "\
Use `meta backlog improve` for a repo-scoped backlog sweep across existing issues in one state.
Choose `--mode basic` for conservative metadata hygiene and `--mode advanced` for deeper
rewrites or parent-child structure proposals.

Use `meta linear issues refine` when you already know which issue needs a critique/rewrite and
the primary goal is improving that issue's description rather than scanning the backlog.";

const ISSUES_REFINE_HELP: &str = "\
Use `meta linear issues refine` when you already know which issue needs a critique/rewrite and
want a focused description-quality pass with auditable refinement artifacts.

Use `meta backlog improve` when you want a repo-scoped backlog sweep for missing labels,
acceptance criteria, priority/estimate, and parent-child structure opportunities.";

const AGENTS_HELP_EXAMPLES: &str = "\
Examples:
  meta agents listen --team MET --project \"MetaStack CLI\"
  meta agents workflows list --root .
  meta agents workflows run ticket-implementation --root . --dry-run";

const LISTEN_HELP_EXAMPLES: &str = "\
Terminal-only examples:
  meta agents listen --check --root .
  meta agents listen --team MET --once
  meta listen sessions list

Concurrent project-scoped examples from one checkout:
  meta agents listen --team MET --project \"MetaStack CLI\"
  meta agents listen --team MET --project \"MetaStack API\"
  meta listen sessions inspect --root . --project \"MetaStack API\"
  meta listen sessions clear --root . --project \"MetaStack API\"
  meta listen sessions resume --root . --project \"MetaStack API\" --once

Default-project example:
  meta runtime setup --root . --team MET --project \"MetaStack CLI\"
  meta agents listen --team MET";

const CONTEXT_HELP_EXAMPLES: &str = "\
Examples:
  meta context show --root .
  meta context scan --root .
  meta context doctor --root .";

const RUNTIME_HELP_EXAMPLES: &str = "\
Examples:
  meta runtime config --json
  meta runtime setup --root . --team MET --project \"MetaStack CLI\"
  meta runtime cron status --root .";

const RUNTIME_CONFIG_HELP: &str = "\
Resolution precedence for built-in provider/model/reasoning:
  1. explicit CLI overrides such as --agent/--provider, --model, and --reasoning
  2. command route override
  3. family route override
  4. repo defaults from `meta runtime setup`
  5. install defaults from `meta runtime config`

Built-in provider catalog:
  codex: gpt-5.4, gpt-5.3-codex, gpt-5.2-codex, gpt-5.1-codex-max, gpt-5.1-codex,
         gpt-5.1-codex-mini, gpt-5-codex, gpt-5-codex-mini
         reasoning: low, medium, high
  claude: sonnet, opus, haiku, sonnet[1m], opusplan
          reasoning: low, medium, high, max

Confirm the effective selection before launch:
  meta agents workflows run ticket-implementation --root . --dry-run";

const RUNTIME_SETUP_HELP: &str = "\
Repo defaults written by `meta runtime setup` participate in the built-in resolution order:
  explicit CLI override -> command route -> family route -> repo default -> install default

Built-in provider/model/reasoning combinations are validated before they are saved.
Use `meta agents workflows run ... --dry-run` or `meta context scan --root .` to confirm the
resolved provider, model, reasoning, route key, and config source before or during execution.

Listen prerequisites:
  codex: `~/.codex/config.toml` must set `approval_policy = \"never\"`
         and `sandbox_mode = \"danger-full-access\"`, and Linear MCP should be removed or
         disabled with `-c mcp_servers.linear.enabled=false`
  claude: `claude` must be on PATH and `ANTHROPIC_API_KEY` should be unset
  verify: `meta agents listen --check --root .`";

const DASHBOARD_HELP_EXAMPLES: &str = "\
Examples:
  meta dashboard linear --team MET --project \"MetaStack CLI\"
  meta dashboard agents --team MET --project \"MetaStack CLI\" --render-once
  meta dashboard team --team MET
  meta dashboard ops --root .";

const MERGE_HELP_EXAMPLES: &str = "\
Examples:
  meta merge --json
  meta merge
  meta merge --render-once --events space,down,space,enter
  meta merge --no-interactive --pull-request 101 --pull-request 102 --validate \"make quality\"";

const WORKSPACE_HELP_EXAMPLES: &str = "\
Examples:
  meta workspace list --root .
  meta workspace clean ENG-10175 --root .
  meta workspace clean --target-only --root .
  meta workspace prune --dry-run --root .";

const UPGRADE_HELP_EXAMPLES: &str = "\
Default latest-stable path:
  meta upgrade --check
  meta upgrade --dry-run
  meta upgrade

Advanced version-management path:
  meta upgrade --version 0.2.0 --dry-run
  meta upgrade --version 0.3.0-rc.1 --prerelease
  meta upgrade --version 0.1.0 --allow-downgrade";

#[derive(Debug, Parser)]
#[command(
    name = "meta",
    bin_name = "meta",
    version,
    about = "CLI scaffolding for backlog management, Linear workflows, agent-backed automation, and codebase scanning.",
    after_help = ROOT_HELP_EXAMPLES
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Plan, create technical backlog children, and sync backlog work inside the repository scope.
    Backlog(BacklogArgs),
    /// Run unattended agents and reusable workflow playbooks.
    Agents(AgentsArgs),
    /// List, create, edit, and refine Linear work.
    Linear(LinearArgs),
    /// Inspect, map, scan, and refresh repository context.
    Context(ContextArgs),
    /// Configure install-scoped and repo-scoped runtime settings.
    Runtime(RuntimeArgs),
    /// Open dashboard views for Linear work, agent sessions, team review, or sync ops.
    Dashboard(DashboardArgs),
    /// Inspect open pull requests, batch them in a one-shot dashboard, and publish one aggregate PR.
    Merge(MergeArgs),
    /// List and clean sibling listener workspace clones under the fixed `<repo>-workspace/` root.
    Workspace(WorkspaceArgs),
    /// Securely self-update a GitHub Release install of `meta` on macOS/Linux.
    Upgrade(UpgradeArgs),
    /// Compatibility alias for `meta backlog plan`.
    #[command(hide = true)]
    Plan(PlanArgs),
    /// Compatibility alias for `meta backlog tech`.
    #[command(hide = true)]
    Technical(TechnicalArgs),
    /// Compatibility alias for `meta agents listen`.
    #[command(hide = true)]
    Listen(ListenArgs),
    /// Compatibility alias for `meta linear issues`.
    #[command(hide = true)]
    #[command(visible_alias = "tickets")]
    Issues(IssuesArgs),
    /// Compatibility alias for `meta linear projects`.
    #[command(hide = true)]
    Projects(ProjectsArgs),
    /// Compatibility alias for `meta runtime cron`.
    #[command(hide = true)]
    Cron(CronArgs),
    /// Compatibility alias for `meta context scan`.
    #[command(hide = true)]
    Scan(ScanArgs),
    /// Compatibility alias for `meta agents workflows`.
    #[command(hide = true)]
    Workflows(WorkflowsArgs),
    /// Compatibility alias for `meta runtime config`.
    #[command(hide = true)]
    Config(ConfigArgs),
    /// Compatibility alias for `meta runtime setup`.
    #[command(hide = true)]
    Setup(SetupArgs),
    /// Compatibility alias for `meta backlog sync`.
    #[command(hide = true)]
    Sync(SyncArgs),
    /// Hidden worker used by `meta listen` to supervise repeated agent turns.
    #[command(hide = true)]
    ListenWorker(ListenWorkerArgs),
    /// Create the local .metastack workspace and reusable templates.
    #[command(hide = true)]
    Scaffold(ScaffoldArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ScaffoldArgs {
    /// Repository root where the `.metastack/` workspace should be created.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub root: PathBuf,
    /// Replace any scaffold-managed files that already exist.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ScanArgs {
    /// Repository root to scan.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub root: PathBuf,
    /// Emit the scan result as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Args)]
pub struct RepositoryRootArgs {
    /// Repository root containing the `.metastack/` workspace.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub root: PathBuf,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = BACKLOG_HELP_EXAMPLES)]
pub struct BacklogArgs {
    #[command(subcommand)]
    pub command: BacklogCommands,
}

#[derive(Debug, Clone, Subcommand)]
pub enum BacklogCommands {
    /// Plan a backlog request into one or more Linear backlog issues.
    Plan(PlanArgs),
    /// Review repo-scoped backlog issues for hygiene gaps and optionally apply improvements.
    Improve(BacklogImproveArgs),
    /// Create a backlog sub-issue and local planning files from a parent issue.
    #[command(name = "tech", visible_alias = "split", visible_alias = "derive")]
    Tech(TechnicalArgs),
    /// Launch the sync dashboard or run direct pull/push backlog operations.
    Sync(SyncArgs),
}

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum BacklogImproveModeArg {
    Basic,
    Advanced,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = BACKLOG_IMPROVE_HELP)]
pub struct BacklogImproveArgs {
    #[command(flatten)]
    pub client: LinearClientArgs,
    /// Optional explicit issue identifiers to improve. When omitted, scans repo-scoped backlog issues.
    #[arg(value_name = "IDENTIFIER")]
    pub issues: Vec<String>,
    /// Improvement depth. `basic` focuses on safe metadata hygiene; `advanced` can rewrite more deeply and propose structure changes.
    #[arg(long, value_enum, default_value_t = BacklogImproveModeArg::Basic)]
    pub mode: BacklogImproveModeArg,
    /// Backlog state to scan when no explicit issues are provided.
    #[arg(long, default_value = "Backlog")]
    pub state: String,
    /// Maximum number of repo-scoped issues to scan when no explicit issues are provided.
    #[arg(long, default_value_t = 25)]
    pub limit: usize,
    /// Apply the proposed updates after persisting the local artifact trail.
    #[arg(long)]
    pub apply: bool,
    /// Override the configured default agent/provider for backlog improvement.
    #[arg(long)]
    pub agent: Option<String>,
    /// Override the configured default model for backlog improvement.
    #[arg(long)]
    pub model: Option<String>,
    /// Override the resolved built-in reasoning option for backlog improvement.
    #[arg(long)]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = AGENTS_HELP_EXAMPLES)]
pub struct AgentsArgs {
    #[command(subcommand)]
    pub command: AgentsCommands,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = WORKSPACE_HELP_EXAMPLES)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub command: WorkspaceCommands,
}

#[derive(Debug, Clone, Subcommand)]
pub enum WorkspaceCommands {
    /// List ticket workspace clones with local git safety signals plus Linear and optional PR status.
    List(WorkspaceListArgs),
    /// Remove one workspace clone or delete `target/` directories across all clones.
    Clean(WorkspaceCleanArgs),
    /// Remove completed workspace clones and print reclaimed-space summaries.
    Prune(WorkspacePruneArgs),
}

#[derive(Debug, Clone, Args)]
pub struct WorkspaceListArgs {
    #[command(flatten)]
    pub client: LinearClientArgs,
}

#[derive(Debug, Clone, Args)]
pub struct WorkspaceCleanArgs {
    #[command(flatten)]
    pub root: RepositoryRootArgs,
    /// Ticket identifier to clean, for example ENG-10175.
    #[arg(value_name = "TICKET", required_unless_present = "target_only")]
    pub ticket: Option<String>,
    /// Remove `target/` directories instead of deleting workspace clones.
    #[arg(long)]
    pub target_only: bool,
    /// Skip confirmation before deleting a workspace clone.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Args)]
pub struct WorkspacePruneArgs {
    #[command(flatten)]
    pub client: LinearClientArgs,
    /// Preview which clones would be removed without deleting anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip confirmation before removing workspace clones.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = UPGRADE_HELP_EXAMPLES)]
pub struct UpgradeArgs {
    /// Check the latest stable release and report whether this install is up to date.
    #[arg(long, conflicts_with = "dry_run")]
    pub check: bool,
    /// Resolve the selected release and print the replacement plan without modifying the install.
    #[arg(long)]
    pub dry_run: bool,
    /// Install a specific release version instead of the latest stable release.
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
    /// Allow prerelease versions for explicit version requests or latest-release resolution.
    #[arg(long)]
    pub prerelease: bool,
    /// Allow replacing the current install with an older version.
    #[arg(long)]
    pub allow_downgrade: bool,
    /// Testing override for the GitHub Releases API root.
    #[arg(long, hide = true, default_value = "https://api.github.com")]
    pub github_api_url: String,
    /// Testing override for the GitHub repository used for release discovery.
    #[arg(long, hide = true, default_value = "metastack-systems/metastack-cli")]
    pub repository: String,
    /// Testing override for the executable path that should be replaced.
    #[arg(long, hide = true, value_name = "PATH")]
    pub executable_path: Option<PathBuf>,
    /// Testing override for the detected operating system slug.
    #[arg(long, hide = true, value_name = "OS")]
    pub os: Option<String>,
    /// Testing override for the detected architecture slug.
    #[arg(long, hide = true, value_name = "ARCH")]
    pub arch: Option<String>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum AgentsCommands {
    /// Listen to Linear for new backlog requests and run agents.
    Listen(ListenArgs),
    /// List, explain, and run reusable workflow playbooks.
    Workflows(WorkflowsArgs),
}

#[derive(Debug, Clone, Args)]
pub struct WorkflowsArgs {
    #[command(subcommand)]
    pub command: WorkflowCommands,
}

#[derive(Debug, Clone, Subcommand)]
pub enum WorkflowCommands {
    /// List the built-in and repo-local workflow playbooks.
    List(WorkflowListArgs),
    /// Explain one workflow including parameters, provider, and validation steps.
    Explain(WorkflowExplainArgs),
    /// Render and run a workflow with the selected local agent provider.
    Run(WorkflowRunArgs),
}

#[derive(Debug, Clone, Args)]
pub struct WorkflowListArgs {
    #[command(flatten)]
    pub root: RepositoryRootArgs,
}

#[derive(Debug, Clone, Args)]
pub struct WorkflowExplainArgs {
    #[command(flatten)]
    pub root: RepositoryRootArgs,
    /// Workflow name, for example `backlog-planning`.
    #[arg(value_name = "NAME")]
    pub name: String,
}

#[derive(Debug, Clone, Args)]
pub struct WorkflowRunArgs {
    #[command(flatten)]
    pub root: RepositoryRootArgs,
    /// Workflow name, for example `backlog-planning`.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Parameter assignments using `key=value`.
    #[arg(long = "param", value_name = "KEY=VALUE")]
    pub params: Vec<String>,
    /// Override the workflow's default local agent/provider.
    #[arg(long)]
    pub provider: Option<String>,
    /// Override the configured default model for this workflow run.
    #[arg(long)]
    pub model: Option<String>,
    /// Override the resolved built-in reasoning option for this workflow run.
    #[arg(long)]
    pub reasoning: Option<String>,
    /// Render the resolved instructions and prompt without launching the provider.
    #[arg(long)]
    pub dry_run: bool,
    /// Linear API token. Falls back to LINEAR_API_KEY.
    #[arg(long, hide_env_values = true)]
    pub api_key: Option<String>,
    /// Override the Linear GraphQL endpoint.
    #[arg(long)]
    pub api_url: Option<String>,
    /// Override the named Linear profile used for issue lookups.
    #[arg(long)]
    pub profile: Option<String>,
    /// Default Linear team key used for workflow-triggered issue lookups.
    #[arg(long)]
    pub team: Option<String>,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = CONTEXT_HELP_EXAMPLES)]
pub struct ContextArgs {
    #[command(subcommand)]
    pub command: ContextCommands,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ContextCommands {
    /// Show the effective instructions, project rules, and context sources used for agent runs.
    Show(ContextShowArgs),
    /// Scan the codebase and generate reusable codebase context.
    Scan(ScanArgs),
    /// Refresh the reusable planning/codebase context for the repository.
    Reload(ContextReloadArgs),
    /// Print a repo-map style summary of the current repository.
    Map(ContextMapArgs),
    /// Diagnose missing or stale context inputs and suggest remediation.
    Doctor(ContextDoctorArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ContextShowArgs {
    #[command(flatten)]
    pub root: RepositoryRootArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ContextReloadArgs {
    #[command(flatten)]
    pub root: RepositoryRootArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ContextMapArgs {
    #[command(flatten)]
    pub root: RepositoryRootArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ContextDoctorArgs {
    #[command(flatten)]
    pub root: RepositoryRootArgs,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = MERGE_HELP_EXAMPLES)]
pub struct MergeArgs {
    /// Repository root containing the `.metastack` workspace.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub root: PathBuf,
    /// Emit the discovered repository and open pull request metadata as JSON.
    #[arg(long, conflicts_with_all = ["no_interactive", "render_once"])]
    pub json: bool,
    /// Skip the one-shot dashboard and run the selected pull requests directly.
    #[arg(long, conflicts_with = "render_once")]
    pub no_interactive: bool,
    /// Resume an existing merge run by run id under `.metastack/merge-runs/<RUN_ID>/`.
    #[arg(long, value_name = "RUN_ID", conflicts_with_all = ["json", "render_once", "pull_requests"])]
    pub resume_run: Option<String>,
    /// Repeatable pull request number used with `--no-interactive`.
    #[arg(long = "pull-request", value_name = "NUMBER")]
    pub pull_requests: Vec<u64>,
    /// Override the validation commands run after the local batch merge completes.
    #[arg(long = "validate", value_name = "COMMAND")]
    pub validate: Vec<String>,
    /// Override the configured default agent/provider for merge planning and conflict help.
    #[arg(long)]
    pub agent: Option<String>,
    /// Override the configured default model for merge planning and conflict help.
    #[arg(long)]
    pub model: Option<String>,
    /// Override the resolved built-in reasoning option for merge planning and conflict help.
    #[arg(long)]
    pub reasoning: Option<String>,
    /// Render the merge dashboard once to an in-memory buffer and print the snapshot.
    #[arg(long)]
    pub render_once: bool,
    /// Apply merge-dashboard actions before a render-once snapshot.
    #[arg(long, hide = true, value_enum, value_delimiter = ',')]
    pub events: Vec<MergeDashboardEventArg>,
    /// Snapshot width when --render-once is set.
    #[arg(long, hide = true, default_value_t = 120)]
    pub width: u16,
    /// Snapshot height when --render-once is set.
    #[arg(long, hide = true, default_value_t = 32)]
    pub height: u16,
}

#[derive(Debug, Clone, Args)]
pub struct CronArgs {
    /// Repository root containing the `.metastack/cron/` workspace.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub root: PathBuf,
    #[command(subcommand)]
    pub command: CronCommands,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = RUNTIME_HELP_EXAMPLES)]
pub struct RuntimeArgs {
    #[command(subcommand)]
    pub command: RuntimeCommands,
}

#[derive(Debug, Clone, Subcommand)]
pub enum RuntimeCommands {
    /// Configure install-scoped MetaStack CLI defaults.
    Config(ConfigArgs),
    /// Setup repo-scoped MetaStack defaults and scaffold `.metastack/`.
    Setup(SetupArgs),
    /// Create and supervise repository-local cron jobs.
    Cron(CronArgs),
}

#[derive(Debug, Clone, Subcommand)]
pub enum CronCommands {
    /// Launch the cron-init dashboard or create a Markdown cron job template directly from flags.
    #[command(visible_alias = "new")]
    Init(CronInitArgs),
    /// Start the cron scheduler, detached when supported.
    Start(CronStartArgs),
    /// Stop the detached cron scheduler.
    Stop,
    /// Show scheduler status plus the known cron jobs.
    Status,
    /// Run one cron job immediately.
    Run(CronRunArgs),
    /// Hidden worker used by `meta cron start` for the detached scheduler loop.
    #[command(hide = true)]
    Daemon(CronDaemonArgs),
}

#[derive(Debug, Clone, Args)]
pub struct CronInitArgs {
    /// Cron job name used for `.metastack/cron/<NAME>.md`. Required with `--no-interactive`.
    #[arg(value_name = "NAME")]
    pub name: Option<String>,
    /// Cron expression using the standard 5-field form. Required with `--no-interactive`.
    #[arg(long, value_name = "EXPR")]
    pub schedule: Option<String>,
    /// Shell command executed when the job is due. Required with `--no-interactive`.
    #[arg(long, value_name = "COMMAND")]
    pub command: Option<String>,
    /// Agent name to run after the shell command when a prompt is configured.
    #[arg(long)]
    pub agent: Option<String>,
    /// Prompt to send to the agent on every cron run.
    #[arg(long)]
    pub prompt: Option<String>,
    /// Shell binary used to execute the command.
    #[arg(long, default_value = "/bin/sh")]
    pub shell: String,
    /// Working directory relative to the repository root.
    #[arg(long, default_value = ".")]
    pub working_directory: String,
    /// Hard timeout for a single run, in seconds.
    #[arg(long, default_value_t = 900)]
    pub timeout_seconds: u64,
    /// Create the file as disabled.
    #[arg(long)]
    pub disabled: bool,
    /// Skip the dashboard flow and create directly from CLI flags.
    #[arg(long, conflicts_with = "render_once")]
    pub no_interactive: bool,
    /// Emit the cron-init result as JSON.
    #[arg(long, conflicts_with = "render_once")]
    pub json: bool,
    /// Render the cron init dashboard once to an in-memory buffer and print the snapshot.
    #[arg(long, hide = true)]
    pub render_once: bool,
    /// Apply cron-init dashboard actions before a render-once snapshot.
    #[arg(long, hide = true, value_enum, value_delimiter = ',')]
    pub events: Vec<CronInitEventArg>,
    /// Snapshot width when --render-once is set.
    #[arg(long, hide = true, default_value_t = 120)]
    pub width: u16,
    /// Snapshot height when --render-once is set.
    #[arg(long, hide = true, default_value_t = 36)]
    pub height: u16,
    /// Replace the file if it already exists.
    #[arg(long)]
    pub force: bool,
}

#[derive(Debug, Clone, Args)]
pub struct CronStartArgs {
    /// Keep the scheduler attached to the current terminal instead of detaching.
    #[arg(long)]
    pub foreground: bool,
    /// Seconds between scheduler polls.
    #[arg(long, default_value_t = 30)]
    pub poll_interval_seconds: u64,
}

#[derive(Debug, Clone, Args)]
pub struct CronRunArgs {
    /// Cron job name without the `.md` suffix.
    #[arg(value_name = "NAME")]
    pub name: String,
}

#[derive(Debug, Clone, Args)]
pub struct CronDaemonArgs {
    /// Seconds between scheduler polls.
    #[arg(long, default_value_t = 30, hide = true)]
    pub poll_interval_seconds: u64,
}

#[derive(Debug, Clone, Args)]
pub struct PlanArgs {
    #[command(flatten)]
    pub client: LinearClientArgs,
    /// Existing issue identifier to reshape in place, for example ENG-10144.
    #[arg(value_name = "IDENTIFIER", conflicts_with = "request")]
    pub target: Option<String>,
    /// Override the Linear team used for created backlog issues.
    #[arg(long)]
    pub team: Option<String>,
    /// Override the Linear project name attached to created backlog issues.
    #[arg(long)]
    pub project: Option<String>,
    /// Override the Linear workflow state attached to created backlog issues.
    #[arg(long)]
    pub state: Option<String>,
    /// Override the Linear priority attached to created backlog issues (1-4).
    #[arg(long)]
    pub priority: Option<u8>,
    /// Add one or more extra Linear labels to created backlog issues.
    #[arg(long = "label")]
    pub labels: Vec<String>,
    /// Override the assignee attached to created backlog issues. Supports `viewer`, user IDs, names, and emails.
    #[arg(long)]
    pub assignee: Option<String>,
    /// Prefill the initial planning request. Required when `--no-interactive` is used.
    #[arg(long)]
    pub request: Option<String>,
    /// Provide follow-up answers in the same order the planning agent asks them.
    #[arg(long = "answer")]
    pub answers: Vec<String>,
    /// Override the configured default agent/provider for this planning run.
    #[arg(long)]
    pub agent: Option<String>,
    /// Override the configured default model for this planning run.
    #[arg(long)]
    pub model: Option<String>,
    /// Override the resolved built-in reasoning option for this planning run.
    #[arg(long)]
    pub reasoning: Option<String>,
    /// Use the fast single-pass planning flow.
    #[arg(long)]
    pub fast: bool,
    /// In fast mode, allow the generated plan to contain multiple tickets instead of the single-ticket default.
    #[arg(long)]
    pub multi: bool,
    /// In fast mode, cap the one-round follow-up batch size between 0 and 10.
    #[arg(long)]
    pub questions: Option<usize>,
    /// Skip the reshape diff preview and apply the in-place update immediately.
    #[arg(long)]
    pub velocity: bool,
    /// Skip the ratatui workflow and run directly from flags/stdin context.
    #[arg(long)]
    pub no_interactive: bool,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = RUNTIME_CONFIG_HELP)]
pub struct ConfigArgs {
    /// Repository root to resolve when compatibility with older invocations is needed.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub root: PathBuf,
    /// Update the stored Linear API key.
    #[arg(long)]
    pub api_key: Option<String>,
    /// Update the default Linear team.
    #[arg(long)]
    pub team: Option<String>,
    /// Update the global default Linear profile.
    #[arg(long)]
    pub default_profile: Option<String>,
    /// Update the default agent.
    #[arg(long)]
    pub default_agent: Option<String>,
    /// Update the default model.
    #[arg(long)]
    pub default_model: Option<String>,
    /// Update the global default built-in reasoning option.
    #[arg(long)]
    pub default_reasoning: Option<String>,
    /// Update how many times `meta merge` will ask the agent to repair failed validation by default.
    #[arg(long)]
    pub merge_validation_repair_attempts: Option<String>,
    /// Update how many transient validation reruns `meta merge` will allow before escalating.
    #[arg(long)]
    pub merge_validation_transient_retry_attempts: Option<String>,
    /// Update how many times `meta merge` retries push and PR publication after transient remote failures.
    #[arg(long)]
    pub merge_publication_retry_attempts: Option<String>,
    /// Update the default assignee used by backlog ticket creation.
    #[arg(long)]
    pub default_assignee: Option<String>,
    /// Update the default state used by backlog ticket creation.
    #[arg(long)]
    pub default_state: Option<String>,
    /// Update the default priority used by backlog ticket creation.
    #[arg(long)]
    pub default_priority: Option<String>,
    /// Update the additive default labels used by backlog ticket creation. Pass `none` to clear them.
    #[arg(long = "default-label")]
    pub default_labels: Vec<String>,
    /// Update the zero-prompt default project selector used by backlog ticket creation.
    #[arg(long)]
    pub velocity_project: Option<String>,
    /// Update the zero-prompt default state used by backlog ticket creation.
    #[arg(long)]
    pub velocity_state: Option<String>,
    /// Update zero-prompt auto-assignment for backlog ticket creation. Supported values: `viewer`.
    #[arg(long)]
    pub velocity_auto_assign: Option<String>,
    /// Update the install-scoped default Linear project ID.
    #[arg(long)]
    pub project_id: Option<String>,
    /// Update the install-scoped default listen label.
    #[arg(long)]
    pub listen_label: Option<String>,
    /// Update the install-scoped listen assignee scope.
    #[arg(long, value_enum)]
    pub assignment_scope: Option<ListenAssignmentScopeArg>,
    /// Update the install-scoped listen refresh policy.
    #[arg(long, value_enum)]
    pub refresh_policy: Option<ListenRefreshPolicyArg>,
    /// Update the install-scoped listen poll interval (seconds).
    #[arg(long)]
    pub poll_interval: Option<String>,
    /// Update the install-scoped plan follow-up question limit.
    #[arg(long)]
    pub plan_follow_up_limit: Option<String>,
    /// Update the install-scoped default mode for `meta backlog plan`. Supported values: `normal`, `fast`, or `none`.
    #[arg(long)]
    pub plan_default_mode: Option<String>,
    /// Update whether install-scoped fast planning defaults to a single ticket. Supported values: `true`, `false`, or `none`.
    #[arg(long)]
    pub plan_fast_single_ticket: Option<String>,
    /// Update the install-scoped fast planning follow-up batch size.
    #[arg(long)]
    pub plan_fast_questions: Option<String>,
    /// Enable or disable install-scoped vim navigation aliases for supported TUI flows.
    #[arg(long, value_enum)]
    pub vim_mode: Option<VimModeArg>,
    /// Update the install-scoped default plan label.
    #[arg(long)]
    pub plan_label: Option<String>,
    /// Update the install-scoped default technical label.
    #[arg(long)]
    pub technical_label: Option<String>,
    /// Set or update an advanced agent route override for a family key like `backlog` or a command key like `backlog.plan`.
    #[arg(long)]
    pub route: Option<String>,
    /// Remove an advanced agent route override for a family or command key.
    #[arg(long)]
    pub clear_route: Option<String>,
    /// Update the agent/provider override for `--route`.
    #[arg(long)]
    pub route_agent: Option<String>,
    /// Update the model override for `--route`.
    #[arg(long)]
    pub route_model: Option<String>,
    /// Update the built-in reasoning override for `--route`.
    #[arg(long)]
    pub route_reasoning: Option<String>,
    /// Launch the dedicated advanced agent-routing dashboard instead of the primary simple config flow.
    #[arg(long)]
    pub advanced_routing: bool,
    /// Launch the shared first-run onboarding wizard instead of the manual config dashboard.
    #[arg(long)]
    pub replay_onboarding: bool,
    /// Emit the install-scoped config view as JSON instead of launching the dashboard.
    #[arg(long, conflicts_with = "render_once")]
    pub json: bool,
    /// Render the config dashboard once to an in-memory buffer and print the snapshot.
    #[arg(long, hide = true)]
    pub render_once: bool,
    /// Apply config-dashboard actions before a render-once snapshot.
    #[arg(long, hide = true, value_enum, value_delimiter = ',')]
    pub events: Vec<ConfigEventArg>,
    /// Snapshot width when --render-once is set.
    #[arg(long, hide = true, default_value_t = 120)]
    pub width: u16,
    /// Snapshot height when --render-once is set.
    #[arg(long, hide = true, default_value_t = 32)]
    pub height: u16,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = RUNTIME_SETUP_HELP)]
pub struct SetupArgs {
    /// Repository root containing `.metastack/meta.json`.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub root: PathBuf,
    /// Update the project-specific Linear API key stored in install-scoped CLI config.
    #[arg(long, hide_env_values = true)]
    pub api_key: Option<String>,
    /// Update the repo-scoped named Linear profile binding.
    #[arg(long)]
    pub profile: Option<String>,
    /// Update the repo-scoped default Linear team.
    #[arg(long)]
    pub team: Option<String>,
    /// Update the repo-scoped default Linear project selector. Names are resolved to canonical IDs before saving.
    #[arg(long)]
    pub project: Option<String>,
    /// Compatibility override for directly setting the repo-scoped Linear project ID.
    #[arg(long, hide = true)]
    pub project_id: Option<String>,
    /// Update the repo-scoped default agent/provider.
    #[arg(long)]
    pub provider: Option<String>,
    /// Update the repo-scoped default model.
    #[arg(long)]
    pub model: Option<String>,
    /// Update the repo-scoped default built-in reasoning option.
    #[arg(long)]
    pub reasoning: Option<String>,
    /// Update the labels required for `meta listen` pickup. Provide a comma-separated list.
    #[arg(long)]
    pub listen_label: Option<String>,
    /// Update the assignee filter used by `meta listen`.
    #[arg(long, value_enum)]
    pub assignment_scope: Option<ListenAssignmentScopeArg>,
    /// Update how `meta listen` refreshes existing ticket workspaces.
    #[arg(long, value_enum)]
    pub refresh_policy: Option<ListenRefreshPolicyArg>,
    /// Update the optional instructions file injected into launched agents.
    #[arg(long)]
    pub instructions_path: Option<String>,
    /// Update the default Linear refresh cadence used by `meta listen`.
    #[arg(long)]
    pub listen_poll_interval: Option<String>,
    /// Update the interactive `meta plan` follow-up question limit.
    #[arg(long)]
    pub interactive_plan_follow_up_question_limit: Option<String>,
    /// Update the repo-scoped default mode for `meta backlog plan`. Supported values: `normal`, `fast`, or `none`.
    #[arg(long)]
    pub plan_default_mode: Option<String>,
    /// Update whether repo-scoped fast planning defaults to a single ticket. Supported values: `true`, `false`, or `none`.
    #[arg(long)]
    pub plan_fast_single_ticket: Option<String>,
    /// Update the repo-scoped fast planning follow-up batch size.
    #[arg(long)]
    pub plan_fast_questions: Option<String>,
    /// Update the default label applied to issues created by `meta plan`.
    #[arg(long)]
    pub plan_label: Option<String>,
    /// Update the default label applied to issues created by `meta backlog tech`.
    #[arg(long)]
    pub technical_label: Option<String>,
    /// Update the default assignee used by backlog ticket creation.
    #[arg(long)]
    pub default_assignee: Option<String>,
    /// Update the default state used by backlog ticket creation.
    #[arg(long)]
    pub default_state: Option<String>,
    /// Update the default priority used by backlog ticket creation.
    #[arg(long)]
    pub default_priority: Option<String>,
    /// Update the additive default labels used by backlog ticket creation. Pass `none` to clear them.
    #[arg(long = "default-label")]
    pub default_labels: Vec<String>,
    /// Update the zero-prompt default project selector used by backlog ticket creation.
    #[arg(long)]
    pub velocity_project: Option<String>,
    /// Update the zero-prompt default state used by backlog ticket creation.
    #[arg(long)]
    pub velocity_state: Option<String>,
    /// Update zero-prompt auto-assignment for backlog ticket creation. Supported values: `viewer`.
    #[arg(long)]
    pub velocity_auto_assign: Option<String>,
    /// Emit the repo-scoped setup view as JSON instead of launching the dashboard.
    #[arg(long, conflicts_with = "render_once")]
    pub json: bool,
    /// Render the setup dashboard once to an in-memory buffer and print the snapshot.
    #[arg(long, hide = true)]
    pub render_once: bool,
    /// Apply setup-dashboard actions before a render-once snapshot.
    #[arg(long, hide = true, value_enum, value_delimiter = ',')]
    pub events: Vec<ConfigEventArg>,
    /// Snapshot width when --render-once is set.
    #[arg(long, hide = true, default_value_t = 120)]
    pub width: u16,
    /// Snapshot height when --render-once is set.
    #[arg(long, hide = true, default_value_t = 32)]
    pub height: u16,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = LISTEN_HELP_EXAMPLES)]
pub struct ListenArgs {
    #[command(subcommand)]
    pub command: Option<ListenCommands>,
    #[command(flatten)]
    pub run: ListenRunArgs,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ListenCommands {
    /// List, inspect, clear, and resume stored MetaListen project sessions.
    Sessions(ListenSessionsArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ListenSessionsArgs {
    #[command(subcommand)]
    pub command: ListenSessionCommands,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ListenSessionCommands {
    /// List install-scoped MetaListen project sessions.
    List(ListenSessionListArgs),
    /// Inspect one stored MetaListen project session.
    Inspect(ListenSessionInspectArgs),
    /// Clear selected stored MetaListen project sessions.
    Clear(ListenSessionClearArgs),
    /// Resume listening for a stored MetaListen project session.
    Resume(Box<ListenSessionResumeArgs>),
}

#[derive(Debug, Clone, Args, Default)]
pub struct ListenSessionTargetArgs {
    /// Resolve the stored project session from this repository root.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub root: PathBuf,
    /// Resolve the stored project session for this effective Linear project selector.
    #[arg(long)]
    pub project: Option<String>,
    /// Resolve the stored project session from an install-scoped project key.
    #[arg(long, value_name = "KEY")]
    pub project_key: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct ListenSessionListArgs {}

#[derive(Debug, Clone, Args)]
pub struct ListenSessionInspectArgs {
    #[command(flatten)]
    pub target: ListenSessionTargetArgs,
}

#[derive(Debug, Clone, Args)]
#[command(group(
    ArgGroup::new("selector")
        .required(true)
        .multiple(false)
        .args(["issue_identifier", "blocked", "completed", "stale", "all"])
))]
pub struct ListenSessionClearArgs {
    #[command(flatten)]
    pub target: ListenSessionTargetArgs,
    /// Clear the stored session for this issue identifier, for example ENG-1234.
    #[arg(value_name = "IDENTIFIER")]
    pub issue_identifier: Option<String>,
    /// Clear only blocked stored sessions.
    #[arg(long)]
    pub blocked: bool,
    /// Clear only completed stored sessions.
    #[arg(long)]
    pub completed: bool,
    /// Clear only stored sessions whose worker pid is no longer running.
    #[arg(long)]
    pub stale: bool,
    /// Clear every stored session record for the selected project.
    #[arg(long)]
    pub all: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ListenSessionResumeArgs {
    /// Resolve the stored project session from an install-scoped project key.
    #[arg(long, value_name = "KEY")]
    pub project_key: Option<String>,
    #[command(flatten)]
    pub run: ListenRunArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ListenRunArgs {
    /// Linear API token. Falls back to LINEAR_API_KEY.
    #[arg(long, hide_env_values = true)]
    pub api_key: Option<String>,
    /// Override the Linear GraphQL endpoint.
    #[arg(long)]
    pub api_url: Option<String>,
    /// Override the named Linear profile used by `meta listen`.
    #[arg(long)]
    pub profile: Option<String>,
    /// Default Linear team key.
    #[arg(long)]
    pub team: Option<String>,
    /// Filter watched work to a single Linear project.
    #[arg(long)]
    pub project: Option<String>,
    /// Repository root containing the `.metastack` workspace.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub root: PathBuf,
    /// Maximum number of Todo issues loaded from Linear per poll.
    #[arg(long, default_value_t = 25)]
    pub limit: usize,
    /// Maximum number of new issues to pick up per poll cycle.
    #[arg(long, default_value_t = 1)]
    pub max_pickups: usize,
    /// Poll interval in seconds for the live daemon loop.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub poll_interval: Option<u64>,
    /// Watch Todo issues for all assignees during this run without changing repo setup.
    #[arg(long)]
    pub all_assignees: bool,
    /// Run listen prerequisite checks and exit without polling Linear or starting the daemon.
    #[arg(long, conflicts_with_all = ["once", "render_once", "demo"])]
    pub check: bool,
    /// Execute a single live poll cycle and print a textual summary.
    #[arg(long, conflicts_with = "render_once")]
    pub once: bool,
    /// Emit the single poll-cycle result as JSON. Requires `--once`.
    #[arg(long, conflicts_with = "render_once")]
    pub json: bool,
    /// Execute a single cycle and print a deterministic ratatui snapshot.
    #[arg(long)]
    pub render_once: bool,
    /// Use built-in sample data instead of calling Linear.
    #[arg(long)]
    pub demo: bool,
    /// Snapshot width when --render-once is set.
    #[arg(long, default_value_t = 120)]
    pub width: u16,
    /// Snapshot height when --render-once is set.
    #[arg(long, default_value_t = 32)]
    pub height: u16,
    /// Override the configured default agent/provider for launched listen workers.
    #[arg(long)]
    pub agent: Option<String>,
    /// Override the configured default model for launched listen workers.
    #[arg(long)]
    pub model: Option<String>,
    /// Override the resolved built-in reasoning option for launched listen workers.
    #[arg(long)]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct TechnicalArgs {
    #[command(flatten)]
    pub client: LinearClientArgs,
    /// Optional parent issue identifier, for example MET-35.
    #[arg(value_name = "IDENTIFIER")]
    pub issue: Option<String>,
    /// Override the Linear workflow state attached to created backlog issues.
    #[arg(long)]
    pub state: Option<String>,
    /// Override the Linear priority attached to created backlog issues (1-4).
    #[arg(long)]
    pub priority: Option<u8>,
    /// Add one or more extra Linear labels to created backlog issues.
    #[arg(long = "label")]
    pub labels: Vec<String>,
    /// Override the assignee attached to created backlog issues. Supports `viewer`, user IDs, names, and emails.
    #[arg(long)]
    pub assignee: Option<String>,
    /// Skip the ratatui workflow and require explicit input instead of prompting.
    #[arg(long)]
    pub no_interactive: bool,
    /// Override the configured default agent/provider for backlog generation.
    #[arg(long)]
    pub agent: Option<String>,
    /// Override the configured default model for backlog generation.
    #[arg(long)]
    pub model: Option<String>,
    /// Override the resolved built-in reasoning option for backlog generation.
    #[arg(long)]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct SyncArgs {
    #[command(flatten)]
    pub client: LinearClientArgs,
    #[command(subcommand)]
    pub command: Option<SyncCommands>,
    /// Skip interactive sync pickers and require explicit command arguments.
    #[arg(long, conflicts_with = "render_once")]
    pub no_interactive: bool,
    /// Emit direct sync-command results as JSON.
    #[arg(long, conflicts_with = "render_once")]
    pub json: bool,
    /// Filter to a specific project name (overrides the repo default).
    #[arg(long)]
    pub project: Option<String>,
    /// Render the sync dashboard once to an in-memory buffer and print the snapshot.
    #[arg(long, hide = true)]
    pub render_once: bool,
    /// Apply sync-dashboard actions before a render-once snapshot.
    #[arg(long, hide = true, value_enum, value_delimiter = ',')]
    pub events: Vec<SyncDashboardEventArg>,
    /// Snapshot width when --render-once is set.
    #[arg(long, hide = true, default_value_t = 120)]
    pub width: u16,
    /// Snapshot height when --render-once is set.
    #[arg(long, hide = true, default_value_t = 32)]
    pub height: u16,
}

#[derive(Debug, Clone, Subcommand)]
pub enum SyncCommands {
    /// Link an existing backlog entry to a Linear issue.
    Link(SyncLinkArgs),
    /// Show sync state for backlog entries under `.metastack/backlog/`.
    Status(SyncStatusArgs),
    /// Pull a Linear issue into `.metastack/backlog/<ISSUE_ID>/`.
    Pull(SyncPullArgs),
    /// Push CLI-managed backlog files back to Linear. `index.md` stays local unless `--update-description` is passed.
    Push(SyncPushArgs),
}

#[derive(Debug, Clone, Args)]
pub struct SyncLinkArgs {
    /// Existing issue identifier, for example MET-35. Prompts in a TTY when omitted.
    #[arg(value_name = "IDENTIFIER")]
    pub issue: Option<String>,
    /// Existing backlog entry slug under `.metastack/backlog/`. Prompts in a TTY when omitted.
    #[arg(long, value_name = "SLUG")]
    pub entry: Option<String>,
    /// Immediately pull the linked issue into the selected backlog entry.
    #[arg(long)]
    pub pull: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SyncStatusArgs {
    /// Fetch current Linear issue state before resolving statuses.
    #[arg(long)]
    pub fetch: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SyncPullArgs {
    /// Existing issue identifier, for example MET-35.
    #[arg(value_name = "IDENTIFIER", required_unless_present = "all")]
    pub issue: Option<String>,
    /// Pull every linked backlog entry.
    #[arg(long, conflicts_with = "issue")]
    pub all: bool,
}

#[derive(Debug, Clone, Args)]
pub struct SyncPushArgs {
    /// Existing issue identifier, for example MET-35.
    #[arg(value_name = "IDENTIFIER", required_unless_present = "all")]
    pub issue: Option<String>,
    /// Push every linked backlog entry.
    #[arg(long, conflicts_with = "issue")]
    pub all: bool,
    /// Also update the Linear issue description from `.metastack/backlog/<ISSUE>/index.md`.
    #[arg(long)]
    pub update_description: bool,
}

#[derive(Debug, Clone, Args)]
pub struct ListenWorkerArgs {
    /// Repository root whose listen state should be updated.
    #[arg(long, value_name = "PATH")]
    pub source_root: PathBuf,
    /// Effective Linear project selector for the listener session store.
    #[arg(long)]
    pub project: Option<String>,
    /// Workspace checkout where the agent should run.
    #[arg(long, value_name = "PATH")]
    pub workspace: PathBuf,
    /// Ticket identifier for the worker run.
    #[arg(long)]
    pub issue: String,
    /// Linear workpad comment id to keep reconciling.
    #[arg(long)]
    pub workpad_comment_id: String,
    /// Optional technical backlog child issue identifier created for the parent issue.
    #[arg(long)]
    pub backlog_issue: Option<String>,
    /// Maximum number of agent turns to allow before the worker stops.
    #[arg(long, default_value_t = 20)]
    pub max_turns: u32,
    /// Linear API token. Falls back to LINEAR_API_KEY.
    #[arg(long, hide_env_values = true)]
    pub api_key: Option<String>,
    /// Override the Linear GraphQL endpoint.
    #[arg(long)]
    pub api_url: Option<String>,
    /// Override the named Linear profile used for worker reconciliation.
    #[arg(long)]
    pub profile: Option<String>,
    /// Default Linear team key.
    #[arg(long)]
    pub team: Option<String>,
    /// Override the configured default agent/provider for this worker.
    #[arg(long)]
    pub agent: Option<String>,
    /// Override the configured default model for this worker.
    #[arg(long)]
    pub model: Option<String>,
    /// Override the resolved built-in reasoning option for this worker.
    #[arg(long)]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunAgentArgs {
    pub root: Option<PathBuf>,
    pub route_key: Option<String>,
    /// Agent name to run. Falls back to the configured default agent.
    pub agent: Option<String>,
    /// Prompt to send to the agent.
    pub prompt: String,
    /// Optional extra instructions to send alongside the prompt.
    pub instructions: Option<String>,
    /// Override the configured default model for this launch.
    pub model: Option<String>,
    /// Override the resolved built-in reasoning option for this launch.
    pub reasoning: Option<String>,
    /// Override the configured transport for this launch.
    pub transport: Option<PromptTransportArg>,
    /// Ordered prompt image attachments for built-in provider launches.
    pub attachments: Vec<PromptImageAttachment>,
}

#[derive(Debug, Clone, Args)]
pub struct LinearClientArgs {
    /// Linear API token. Falls back to LINEAR_API_KEY.
    #[arg(long, hide_env_values = true)]
    pub api_key: Option<String>,
    /// Override the Linear GraphQL endpoint.
    #[arg(long)]
    pub api_url: Option<String>,
    /// Override the named Linear profile used for this command.
    #[arg(long)]
    pub profile: Option<String>,
    /// Repository root containing the `.metastack/meta.json` defaults.
    #[arg(long, value_name = "PATH", default_value = ".")]
    pub root: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct ProjectsArgs {
    #[command(flatten)]
    pub client: LinearClientArgs,
    #[command(subcommand)]
    pub command: ProjectsCommands,
}

#[derive(Debug, Clone, Args)]
pub struct IssuesArgs {
    #[command(flatten)]
    pub client: LinearClientArgs,
    #[command(subcommand)]
    pub command: IssueCommands,
}

#[derive(Debug, Clone, Args)]
pub struct LinearArgs {
    #[command(flatten)]
    pub client: LinearClientArgs,
    /// Default Linear team key.
    #[arg(long)]
    pub team: Option<String>,
    #[command(subcommand)]
    pub command: LinearCommands,
}

#[derive(Debug, Clone, Subcommand)]
pub enum LinearCommands {
    /// List and inspect Linear projects.
    #[command(subcommand)]
    Projects(ProjectsCommands),
    /// List, create, and edit Linear issues.
    #[command(visible_alias = "tickets")]
    #[command(subcommand)]
    Issues(IssueCommands),
    /// Launch the ratatui Linear dashboard.
    Dashboard(DashboardCommandArgs),
}

#[derive(Debug, Clone, Subcommand)]
pub enum ProjectsCommands {
    /// List projects in the workspace or a specific team.
    List(ProjectListArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ProjectListArgs {
    /// Limit the number of returned projects.
    #[arg(long, default_value_t = 25)]
    pub limit: usize,
    /// Filter projects to a specific team key.
    #[arg(long)]
    pub team: Option<String>,
    /// Emit raw JSON instead of a table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Subcommand)]
pub enum IssueCommands {
    /// Browse issues in an interactive dashboard or emit raw JSON.
    List(IssueListArgs),
    /// Create a new issue.
    Create(IssueCreateArgs),
    /// Edit an existing issue by identifier.
    Edit(IssueEditArgs),
    /// Critique and rewrite one or more existing issues within the configured repo scope.
    Refine(IssueRefineArgs),
}

#[derive(Debug, Clone, Args)]
pub struct IssueListArgs {
    /// Limit the number of returned issues.
    #[arg(long, default_value_t = 25)]
    pub limit: usize,
    /// Filter issues to a specific team key.
    #[arg(long)]
    pub team: Option<String>,
    /// Filter issues to a project name.
    #[arg(long)]
    pub project: Option<String>,
    /// Filter issues to a state name.
    #[arg(long)]
    pub state: Option<String>,
    /// Emit raw JSON instead of a table.
    #[arg(long, conflicts_with = "render_once")]
    pub json: bool,
    /// Render the issue browser once to an in-memory buffer and print the snapshot.
    #[arg(long, hide = true)]
    pub render_once: bool,
    /// Apply dashboard actions before a render-once snapshot.
    #[arg(long, hide = true, value_enum, value_delimiter = ',')]
    pub events: Vec<DashboardEventArg>,
    /// Snapshot width when --render-once is set.
    #[arg(long, hide = true, default_value_t = 120)]
    pub width: u16,
    /// Snapshot height when --render-once is set.
    #[arg(long, hide = true, default_value_t = 32)]
    pub height: u16,
}

#[derive(Debug, Clone, Args)]
pub struct IssueCreateArgs {
    /// Team key for the new issue.
    #[arg(long)]
    pub team: Option<String>,
    /// Issue title. Prefills the interactive form and is required with --no-interactive.
    #[arg(long)]
    pub title: Option<String>,
    /// Markdown description. Prefills the interactive form.
    #[arg(long)]
    pub description: Option<String>,
    /// Project name to attach.
    #[arg(long)]
    pub project: Option<String>,
    /// Workflow state name. Prefills the interactive form.
    #[arg(long)]
    pub state: Option<String>,
    /// Priority between 0 and 4. Prefills the interactive form.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=4))]
    pub priority: Option<u8>,
    /// Skip the ratatui workflow and create directly from CLI flags.
    #[arg(long, conflicts_with = "render_once")]
    pub no_interactive: bool,
    /// Render the create form once to an in-memory buffer and print the snapshot.
    #[arg(long, hide = true)]
    pub render_once: bool,
    /// Apply create-form actions before a render-once snapshot.
    #[arg(long, hide = true, value_enum, value_delimiter = ',')]
    pub events: Vec<IssueCreateEventArg>,
    /// Snapshot width when --render-once is set.
    #[arg(long, hide = true, default_value_t = 120)]
    pub width: u16,
    /// Snapshot height when --render-once is set.
    #[arg(long, hide = true, default_value_t = 32)]
    pub height: u16,
}

#[derive(Debug, Clone, Args)]
pub struct IssueEditArgs {
    /// Existing issue identifier, for example MET-11.
    #[arg(long, value_name = "IDENTIFIER")]
    pub issue: String,
    /// Update the issue title. Prefills the interactive form.
    #[arg(long)]
    pub title: Option<String>,
    /// Update the issue description. Prefills the interactive form.
    #[arg(long)]
    pub description: Option<String>,
    /// Move the issue to a project by name.
    #[arg(long)]
    pub project: Option<String>,
    /// Move the issue to a workflow state by name. Prefills the interactive form.
    #[arg(long)]
    pub state: Option<String>,
    /// Update the issue priority between 0 and 4. Prefills the interactive form.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=4))]
    pub priority: Option<u8>,
    /// Skip the ratatui workflow and update directly from CLI flags.
    #[arg(long, conflicts_with = "render_once")]
    pub no_interactive: bool,
    /// Render the edit form once to an in-memory buffer and print the snapshot.
    #[arg(long, hide = true)]
    pub render_once: bool,
    /// Apply edit-form actions before a render-once snapshot.
    #[arg(long, hide = true, value_enum, value_delimiter = ',')]
    pub events: Vec<IssueEditEventArg>,
    /// Snapshot width when --render-once is set.
    #[arg(long, hide = true, default_value_t = 120)]
    pub width: u16,
    /// Snapshot height when --render-once is set.
    #[arg(long, hide = true, default_value_t = 32)]
    pub height: u16,
}

#[derive(Debug, Clone, Args)]
#[command(after_help = ISSUES_REFINE_HELP)]
pub struct IssueRefineArgs {
    /// One or more existing issue identifiers, for example MET-35.
    #[arg(value_name = "IDENTIFIER", required = true)]
    pub issues: Vec<String>,
    /// Number of critique/rewrite passes to run for each issue.
    #[arg(long, default_value_t = 1)]
    pub passes: usize,
    /// Update the local backlog packet and push the final rewrite back to Linear.
    #[arg(long)]
    pub apply: bool,
    /// Emit refinement results as JSON.
    #[arg(long)]
    pub json: bool,
    /// Override the configured default agent/provider for refinement.
    #[arg(long)]
    pub agent: Option<String>,
    /// Override the configured default model for refinement.
    #[arg(long)]
    pub model: Option<String>,
    /// Override the resolved built-in reasoning option for refinement.
    #[arg(long)]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Args)]
#[command(
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true,
    after_help = DASHBOARD_HELP_EXAMPLES
)]
pub struct DashboardArgs {
    #[command(subcommand)]
    pub command: Option<DashboardCommands>,
    #[command(flatten)]
    pub legacy: DashboardLinearArgs,
}

#[derive(Debug, Clone, Subcommand)]
pub enum DashboardCommands {
    /// Launch the Linear issue dashboard.
    Linear(DashboardLinearArgs),
    /// Launch the agent-session dashboard exposed by `meta agents listen`.
    Agents(DashboardAgentsArgs),
    /// Launch the team-oriented Linear review dashboard.
    Team(DashboardLinearArgs),
    /// Launch the ops-oriented backlog sync dashboard.
    Ops(DashboardOpsArgs),
}

#[derive(Debug, Clone, Args)]
pub struct DashboardLinearArgs {
    #[command(flatten)]
    pub client: LinearClientArgs,
    #[command(flatten)]
    pub dashboard: DashboardCommandArgs,
}

#[derive(Debug, Clone, Args)]
pub struct DashboardAgentsArgs {
    #[command(flatten)]
    pub listen: ListenRunArgs,
}

#[derive(Debug, Clone, Args)]
pub struct DashboardOpsArgs {
    #[command(flatten)]
    pub sync: SyncArgs,
}

#[derive(Debug, Clone, Args)]
pub struct DashboardCommandArgs {
    /// Filter the dashboard to a team key.
    #[arg(long)]
    pub team: Option<String>,
    /// Filter the dashboard to a project name.
    #[arg(long)]
    pub project: Option<String>,
    /// Maximum number of issues to load.
    #[arg(long, default_value_t = 25)]
    pub limit: usize,
    /// Use built-in sample data instead of calling Linear.
    #[arg(long)]
    pub demo: bool,
    /// Render once to an in-memory buffer and print the snapshot.
    #[arg(long)]
    pub render_once: bool,
    /// Apply dashboard actions before a render-once snapshot.
    #[arg(long, value_enum, value_delimiter = ',')]
    pub events: Vec<DashboardEventArg>,
    /// Snapshot width when --render-once is set.
    #[arg(long, default_value_t = 120)]
    pub width: u16,
    /// Snapshot height when --render-once is set.
    #[arg(long, default_value_t = 32)]
    pub height: u16,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum DashboardEventArg {
    Up,
    Down,
    Tab,
    Enter,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum SyncDashboardEventArg {
    Up,
    Down,
    Enter,
    Back,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum MergeDashboardEventArg {
    Up,
    Down,
    Tab,
    PageUp,
    PageDown,
    Home,
    End,
    Space,
    Enter,
    Back,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CronInitEventArg {
    Up,
    Down,
    Left,
    Right,
    Tab,
    BackTab,
    Save,
    Esc,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum IssueCreateEventArg {
    Up,
    Down,
    Left,
    Right,
    Tab,
    BackTab,
    Enter,
    Esc,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum IssueEditEventArg {
    Up,
    Down,
    Left,
    Right,
    Tab,
    BackTab,
    Enter,
    Esc,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PromptTransportArg {
    Arg,
    Stdin,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ListenAssignmentScopeArg {
    Any,
    ViewerOnly,
    ViewerOrUnassigned,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ListenRefreshPolicyArg {
    ReuseAndRefresh,
    RecreateFromOriginMain,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum VimModeArg {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ConfigEventArg {
    Up,
    Down,
    Tab,
    BackTab,
    Enter,
    Esc,
}

impl Cli {
    /// Infer the machine-output command from raw argv when clap parsing fails before dispatch.
    pub(crate) fn machine_output_command_from_argv(args: &[OsString]) -> Option<&'static str> {
        let tokens = args
            .iter()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        infer_machine_output_command(&tokens)
    }

    pub(crate) fn machine_output_command(&self) -> Option<&'static str> {
        match &self.command {
            Command::Backlog(args) => match &args.command {
                BacklogCommands::Plan(args) if args.no_interactive => Some("backlog.plan"),
                BacklogCommands::Tech(args) if args.no_interactive => Some("backlog.tech"),
                BacklogCommands::Sync(args) if args.no_interactive || args.json => {
                    Some("backlog.sync")
                }
                _ => None,
            },
            Command::Agents(args) => match &args.command {
                AgentsCommands::Listen(args) if args.run.json => Some("agents.listen"),
                _ => None,
            },
            Command::Linear(args) => match &args.command {
                LinearCommands::Projects(ProjectsCommands::List(args)) if args.json => {
                    Some("linear.projects.list")
                }
                LinearCommands::Issues(command) => match command {
                    IssueCommands::List(args) if args.json => Some("linear.issues.list"),
                    IssueCommands::Create(args) if args.no_interactive => {
                        Some("linear.issues.create")
                    }
                    IssueCommands::Edit(args) if args.no_interactive => Some("linear.issues.edit"),
                    IssueCommands::Refine(args) if args.json => Some("linear.issues.refine"),
                    _ => None,
                },
                _ => None,
            },
            Command::Context(args) => match &args.command {
                ContextCommands::Scan(args) if args.json => Some("context.scan"),
                _ => None,
            },
            Command::Runtime(args) => match &args.command {
                RuntimeCommands::Config(args) if args.json => Some("runtime.config"),
                RuntimeCommands::Setup(args) if args.json => Some("runtime.setup"),
                RuntimeCommands::Cron(args) => match &args.command {
                    CronCommands::Init(args) if args.no_interactive || args.json => {
                        Some("runtime.cron.init")
                    }
                    _ => None,
                },
                _ => None,
            },
            Command::Merge(args) if args.json => Some("merge"),
            Command::Plan(args) if args.no_interactive => Some("backlog.plan"),
            Command::Technical(args) if args.no_interactive => Some("backlog.tech"),
            Command::Listen(args) if args.run.json => Some("agents.listen"),
            Command::Issues(args) => match &args.command {
                IssueCommands::List(args) if args.json => Some("linear.issues.list"),
                IssueCommands::Create(args) if args.no_interactive => Some("linear.issues.create"),
                IssueCommands::Edit(args) if args.no_interactive => Some("linear.issues.edit"),
                IssueCommands::Refine(args) if args.json => Some("linear.issues.refine"),
                _ => None,
            },
            Command::Projects(args) => match &args.command {
                ProjectsCommands::List(args) if args.json => Some("linear.projects.list"),
                _ => None,
            },
            Command::Cron(args) => match &args.command {
                CronCommands::Init(args) if args.no_interactive || args.json => {
                    Some("runtime.cron.init")
                }
                _ => None,
            },
            Command::Scan(args) if args.json => Some("context.scan"),
            Command::Config(args) if args.json => Some("runtime.config"),
            Command::Setup(args) if args.json => Some("runtime.setup"),
            Command::Sync(args) if args.no_interactive || args.json => Some("backlog.sync"),
            _ => None,
        }
    }
}

fn infer_machine_output_command(tokens: &[String]) -> Option<&'static str> {
    let (command, rest) = tokens.split_first()?;
    match command.as_str() {
        "backlog" => infer_backlog_machine_output(rest),
        "agents" => infer_agents_machine_output(rest),
        "linear" => infer_linear_machine_output(rest),
        "context" => {
            if matches_subcommand(rest, "scan") && has_flag(rest, "--json") {
                Some("context.scan")
            } else {
                None
            }
        }
        "runtime" => infer_runtime_machine_output(rest),
        "merge" if has_flag(rest, "--json") => Some("merge"),
        "plan" if has_flag(rest, "--no-interactive") => Some("backlog.plan"),
        "technical" if has_flag(rest, "--no-interactive") => Some("backlog.tech"),
        "listen" if has_flag(rest, "--json") => Some("agents.listen"),
        "issues" => infer_issue_machine_output(rest),
        "projects" => {
            if matches_subcommand(rest, "list") && has_flag(rest, "--json") {
                Some("linear.projects.list")
            } else {
                None
            }
        }
        "cron" => infer_cron_init_machine_output(rest),
        "scan" if has_flag(rest, "--json") => Some("context.scan"),
        "config" if has_flag(rest, "--json") => Some("runtime.config"),
        "setup" if has_flag(rest, "--json") => Some("runtime.setup"),
        "sync" if has_flag(rest, "--json") || has_flag(rest, "--no-interactive") => {
            Some("backlog.sync")
        }
        _ => None,
    }
}

fn infer_backlog_machine_output(tokens: &[String]) -> Option<&'static str> {
    let (command, rest) = tokens.split_first()?;
    match command.as_str() {
        "plan" if has_flag(rest, "--no-interactive") => Some("backlog.plan"),
        "tech" | "split" | "derive" if has_flag(rest, "--no-interactive") => Some("backlog.tech"),
        "sync" if has_flag(rest, "--json") || has_flag(rest, "--no-interactive") => {
            Some("backlog.sync")
        }
        _ => None,
    }
}

fn infer_agents_machine_output(tokens: &[String]) -> Option<&'static str> {
    let (command, rest) = tokens.split_first()?;
    match command.as_str() {
        "listen" if has_flag(rest, "--json") => Some("agents.listen"),
        _ => None,
    }
}

fn infer_linear_machine_output(tokens: &[String]) -> Option<&'static str> {
    let (command, rest) = tokens.split_first()?;
    match command.as_str() {
        "projects" => {
            if matches_subcommand(rest, "list") && has_flag(rest, "--json") {
                Some("linear.projects.list")
            } else {
                None
            }
        }
        "issues" => infer_issue_machine_output(rest),
        _ => None,
    }
}

fn infer_issue_machine_output(tokens: &[String]) -> Option<&'static str> {
    let (command, rest) = tokens.split_first()?;
    match command.as_str() {
        "list" if has_flag(rest, "--json") => Some("linear.issues.list"),
        "create" if has_flag(rest, "--no-interactive") => Some("linear.issues.create"),
        "edit" if has_flag(rest, "--no-interactive") => Some("linear.issues.edit"),
        "refine" if has_flag(rest, "--json") => Some("linear.issues.refine"),
        _ => None,
    }
}

fn infer_runtime_machine_output(tokens: &[String]) -> Option<&'static str> {
    let (command, rest) = tokens.split_first()?;
    match command.as_str() {
        "config" if has_flag(rest, "--json") => Some("runtime.config"),
        "setup" if has_flag(rest, "--json") => Some("runtime.setup"),
        "cron" => infer_cron_init_machine_output(rest),
        _ => None,
    }
}

fn infer_cron_init_machine_output(tokens: &[String]) -> Option<&'static str> {
    if matches_subcommand(tokens, "init")
        && (has_flag(tokens, "--json") || has_flag(tokens, "--no-interactive"))
    {
        Some("runtime.cron.init")
    } else {
        None
    }
}

fn matches_subcommand(tokens: &[String], name: &str) -> bool {
    matches!(tokens.first(), Some(token) if token == name)
}

fn has_flag(tokens: &[String], flag: &str) -> bool {
    tokens.iter().any(|token| token == flag)
}

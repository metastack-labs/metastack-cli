mod agent_provider;
mod agents;
mod backlog;
mod cli;
mod config;
mod config_command;
mod context;
mod cron;
mod cron_dashboard;
mod fs;
mod linear;
mod listen;
mod merge;
mod merge_dashboard;
mod plan;
mod progress;
mod repo_target;
mod scaffold;
mod scan;
mod scan_dashboard;
mod scan_prompts;
mod setup;
mod sync_command;
mod sync_dashboard;
mod technical;
mod text_diff;
mod tui;
mod workflow_contract;
mod workflows;
mod workspace;
mod workspace_dashboard;

use std::ffi::OsString;

use anyhow::{Result, bail};
use clap::Parser;

use crate::cli::{
    AgentsCommands, BacklogCommands, Cli, Command, ConfigEventArg, DashboardCommands,
    DashboardEventArg, IssueCreateEventArg, IssueEditEventArg, LinearCommands,
    ListenAssignmentScopeArg, MergeDashboardEventArg, RuntimeCommands, SyncCommands,
    SyncDashboardEventArg,
};
use crate::config::ListenAssignmentScope;
use crate::config_command::{ConfigAction, ConfigCommandOutput, run_config};
use crate::context::run_context_command;
use crate::cron::run_cron;
use crate::linear::create::IssueCreateAction;
use crate::linear::dashboard::DashboardAction;
use crate::linear::edit::IssueEditAction;
pub(crate) use crate::linear::{LinearCommandContext, load_linear_command_context};
use crate::linear::{run_dashboard_command, run_issues_command, run_projects_command};
use crate::listen::{
    run_listen, run_listen_session_clear, run_listen_session_inspect, run_listen_session_list,
    run_listen_session_resume, run_listen_worker,
};
use crate::merge::run_merge;
use crate::merge_dashboard::MergeDashboardAction;
use crate::plan::run_plan;
use crate::scaffold::run_scaffold;
use crate::scan::run_scan;
use crate::setup::run_setup;
use crate::sync_command::{
    run_sync_dashboard_command, run_sync_link, run_sync_pull, run_sync_push, run_sync_status,
};
use crate::sync_dashboard::{SyncDashboardAction, SyncDashboardOptions};
use crate::technical::run_technical;
use crate::workflows::run_workflows;
use crate::workspace::{run_workspace_clean, run_workspace_list, run_workspace_prune};

pub async fn run() -> Result<()> {
    run_with_args(std::env::args_os()).await
}

pub async fn run_with_args<I, T>(args: I) -> Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let cli = Cli::parse_from(args);
    dispatch(cli).await
}

async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Backlog(args) => match args.command {
            BacklogCommands::Plan(args) => {
                let report = run_plan(&args).await?;
                println!("{}", report.render());
            }
            BacklogCommands::Tech(args) => {
                run_technical(&args).await?;
            }
            BacklogCommands::Sync(args) => match args.command {
                Some(SyncCommands::Link(link_args)) => {
                    run_sync_link(
                        &args.client,
                        args.project.as_deref(),
                        args.no_interactive,
                        &link_args,
                    )
                    .await?;
                }
                Some(SyncCommands::Status(status_args)) => {
                    run_sync_status(&args.client, &status_args).await?;
                }
                Some(SyncCommands::Pull(issue_args)) => {
                    run_sync_pull(&args.client, &issue_args).await?;
                }
                Some(SyncCommands::Push(issue_args)) => {
                    run_sync_push(&args.client, &issue_args).await?;
                }
                None => {
                    if args.no_interactive {
                        bail!(
                            "`meta backlog sync --no-interactive` requires a subcommand such as `status`, `link`, `pull`, or `push`"
                        );
                    }
                    run_sync_dashboard_command(
                        &args.client,
                        args.project.as_deref(),
                        SyncDashboardOptions {
                            render_once: args.render_once,
                            width: args.width,
                            height: args.height,
                            actions: args
                                .events
                                .into_iter()
                                .map(SyncDashboardAction::from)
                                .collect(),
                        },
                    )
                    .await?;
                }
            },
        },
        Command::Agents(args) => match args.command {
            AgentsCommands::Listen(args) => match args.command {
                Some(crate::cli::ListenCommands::Sessions(session_args)) => {
                    match session_args.command {
                        crate::cli::ListenSessionCommands::List(list_args) => {
                            println!("{}", run_listen_session_list(&list_args)?);
                        }
                        crate::cli::ListenSessionCommands::Inspect(inspect_args) => {
                            println!("{}", run_listen_session_inspect(&inspect_args)?);
                        }
                        crate::cli::ListenSessionCommands::Clear(clear_args) => {
                            println!("{}", run_listen_session_clear(&clear_args)?);
                        }
                        crate::cli::ListenSessionCommands::Resume(resume_args) => {
                            run_listen_session_resume(&resume_args).await?;
                        }
                    }
                }
                None => {
                    run_listen(&args.run).await?;
                }
            },
            AgentsCommands::Workflows(args) => {
                println!("{}", run_workflows(&args).await?);
            }
        },
        Command::Linear(args) => {
            let client = args.client;
            let default_team = args.team;
            match args.command {
                LinearCommands::Projects(command) => {
                    run_projects_command(&client, default_team.clone(), command).await?;
                }
                LinearCommands::Issues(command) => {
                    run_issues_command(&client, default_team.clone(), command).await?;
                }
                LinearCommands::Dashboard(command) => {
                    run_dashboard_command(&client, default_team, command).await?;
                }
            }
        }
        Command::Context(args) => {
            println!("{}", run_context_command(&args)?);
        }
        Command::Runtime(args) => match args.command {
            RuntimeCommands::Config(args) => match run_config(&args).await? {
                ConfigCommandOutput::Text(output) | ConfigCommandOutput::Json(output) => {
                    println!("{output}");
                }
            },
            RuntimeCommands::Setup(args) => {
                println!("{}", run_setup(&args).await?);
            }
            RuntimeCommands::Cron(args) => {
                if let Some(output) = run_cron(&args)? {
                    println!("{output}");
                }
            }
        },
        Command::Dashboard(args) => match args.command {
            Some(DashboardCommands::Linear(args)) | Some(DashboardCommands::Team(args)) => {
                run_dashboard_command(&args.client, None, args.dashboard).await?;
            }
            Some(DashboardCommands::Agents(args)) => {
                run_listen(&args.listen).await?;
            }
            Some(DashboardCommands::Ops(args)) => match args.sync.command {
                Some(SyncCommands::Link(link_args)) => {
                    run_sync_link(
                        &args.sync.client,
                        args.sync.project.as_deref(),
                        args.sync.no_interactive,
                        &link_args,
                    )
                    .await?;
                }
                Some(SyncCommands::Status(status_args)) => {
                    run_sync_status(&args.sync.client, &status_args).await?;
                }
                Some(SyncCommands::Pull(issue_args)) => {
                    run_sync_pull(&args.sync.client, &issue_args).await?;
                }
                Some(SyncCommands::Push(issue_args)) => {
                    run_sync_push(&args.sync.client, &issue_args).await?;
                }
                None => {
                    if args.sync.no_interactive {
                        bail!(
                            "`meta dashboard ops --no-interactive` requires a subcommand such as `status`, `link`, `pull`, or `push`"
                        );
                    }
                    run_sync_dashboard_command(
                        &args.sync.client,
                        args.sync.project.as_deref(),
                        SyncDashboardOptions {
                            render_once: args.sync.render_once,
                            width: args.sync.width,
                            height: args.sync.height,
                            actions: args
                                .sync
                                .events
                                .into_iter()
                                .map(SyncDashboardAction::from)
                                .collect(),
                        },
                    )
                    .await?;
                }
            },
            None => {
                print_compatibility_hint("meta dashboard", "meta dashboard linear");
                run_dashboard_command(&args.legacy.client, None, args.legacy.dashboard).await?;
            }
        },
        Command::Merge(args) => {
            run_merge(&args).await?;
        }
        Command::Workspace(args) => match args.command {
            crate::cli::WorkspaceCommands::List(args) => {
                println!("{}", run_workspace_list(&args).await?);
            }
            crate::cli::WorkspaceCommands::Clean(args) => {
                println!("{}", run_workspace_clean(&args)?);
            }
            crate::cli::WorkspaceCommands::Prune(args) => {
                println!("{}", run_workspace_prune(&args).await?);
            }
        },
        Command::Scaffold(args) => {
            let report = run_scaffold(&args)?;
            println!("{}", report.render());
        }
        Command::Cron(args) => {
            print_compatibility_hint("meta cron", "meta runtime cron");
            if let Some(output) = run_cron(&args)? {
                println!("{output}");
            }
        }
        Command::Scan(args) => {
            print_compatibility_hint("meta scan", "meta context scan");
            let report = run_scan(&args)?;
            println!("{}", report.render());
        }
        Command::Workflows(args) => {
            print_compatibility_hint("meta workflows", "meta agents workflows");
            println!("{}", run_workflows(&args).await?);
        }
        Command::Plan(args) => {
            print_compatibility_hint("meta plan", "meta backlog plan");
            let report = run_plan(&args).await?;
            println!("{}", report.render());
        }
        Command::Config(args) => {
            print_compatibility_hint("meta config", "meta runtime config");
            match run_config(&args).await? {
                ConfigCommandOutput::Text(output) | ConfigCommandOutput::Json(output) => {
                    println!("{output}");
                }
            }
        }
        Command::Setup(args) => {
            print_compatibility_hint("meta setup", "meta runtime setup");
            println!("{}", run_setup(&args).await?);
        }
        Command::Listen(args) => match args.command {
            Some(crate::cli::ListenCommands::Sessions(session_args)) => {
                print_compatibility_hint("meta listen", "meta agents listen");
                match session_args.command {
                    crate::cli::ListenSessionCommands::List(list_args) => {
                        println!("{}", run_listen_session_list(&list_args)?);
                    }
                    crate::cli::ListenSessionCommands::Inspect(inspect_args) => {
                        println!("{}", run_listen_session_inspect(&inspect_args)?);
                    }
                    crate::cli::ListenSessionCommands::Clear(clear_args) => {
                        println!("{}", run_listen_session_clear(&clear_args)?);
                    }
                    crate::cli::ListenSessionCommands::Resume(resume_args) => {
                        run_listen_session_resume(&resume_args).await?;
                    }
                }
            }
            None => {
                print_compatibility_hint("meta listen", "meta agents listen");
                run_listen(&args.run).await?;
            }
        },
        Command::Technical(args) => {
            print_compatibility_hint("meta technical", "meta backlog tech");
            run_technical(&args).await?;
        }
        Command::Sync(args) => match args.command {
            Some(SyncCommands::Link(link_args)) => {
                print_compatibility_hint("meta sync", "meta backlog sync");
                run_sync_link(
                    &args.client,
                    args.project.as_deref(),
                    args.no_interactive,
                    &link_args,
                )
                .await?;
            }
            Some(SyncCommands::Status(status_args)) => {
                print_compatibility_hint("meta sync", "meta backlog sync");
                run_sync_status(&args.client, &status_args).await?;
            }
            Some(SyncCommands::Pull(issue_args)) => {
                print_compatibility_hint("meta sync", "meta backlog sync");
                run_sync_pull(&args.client, &issue_args).await?;
            }
            Some(SyncCommands::Push(issue_args)) => {
                print_compatibility_hint("meta sync", "meta backlog sync");
                run_sync_push(&args.client, &issue_args).await?;
            }
            None => {
                print_compatibility_hint("meta sync", "meta backlog sync");
                if args.no_interactive {
                    bail!(
                        "`meta sync --no-interactive` requires a subcommand such as `status`, `link`, `pull`, or `push`"
                    );
                }
                run_sync_dashboard_command(
                    &args.client,
                    args.project.as_deref(),
                    SyncDashboardOptions {
                        render_once: args.render_once,
                        width: args.width,
                        height: args.height,
                        actions: args
                            .events
                            .into_iter()
                            .map(SyncDashboardAction::from)
                            .collect(),
                    },
                )
                .await?;
            }
        },
        Command::ListenWorker(args) => {
            run_listen_worker(&args).await?;
        }
        Command::Projects(args) => {
            print_compatibility_hint("meta projects", "meta linear projects");
            run_projects_command(&args.client, None, args.command).await?;
        }
        Command::Issues(args) => {
            print_compatibility_hint("meta issues", "meta linear issues");
            run_issues_command(&args.client, None, args.command).await?;
        }
    }

    Ok(())
}

fn print_compatibility_hint(legacy_command: &str, preferred_command: &str) {
    eprintln!("hint: `{legacy_command}` is a compatibility alias; prefer `{preferred_command}`.");
}

impl From<DashboardEventArg> for DashboardAction {
    fn from(value: DashboardEventArg) -> Self {
        match value {
            DashboardEventArg::Up => DashboardAction::Up,
            DashboardEventArg::Down => DashboardAction::Down,
            DashboardEventArg::Tab => DashboardAction::Tab,
            DashboardEventArg::Enter => DashboardAction::Enter,
        }
    }
}

impl From<SyncDashboardEventArg> for SyncDashboardAction {
    fn from(value: SyncDashboardEventArg) -> Self {
        match value {
            SyncDashboardEventArg::Up => SyncDashboardAction::Up,
            SyncDashboardEventArg::Down => SyncDashboardAction::Down,
            SyncDashboardEventArg::Enter => SyncDashboardAction::Enter,
            SyncDashboardEventArg::Back => SyncDashboardAction::Back,
        }
    }
}

impl From<MergeDashboardEventArg> for MergeDashboardAction {
    fn from(value: MergeDashboardEventArg) -> Self {
        match value {
            MergeDashboardEventArg::Up => MergeDashboardAction::Up,
            MergeDashboardEventArg::Down => MergeDashboardAction::Down,
            MergeDashboardEventArg::Space => MergeDashboardAction::Toggle,
            MergeDashboardEventArg::Enter => MergeDashboardAction::Enter,
            MergeDashboardEventArg::Back => MergeDashboardAction::Back,
        }
    }
}

impl From<IssueCreateEventArg> for IssueCreateAction {
    fn from(value: IssueCreateEventArg) -> Self {
        match value {
            IssueCreateEventArg::Up => IssueCreateAction::Up,
            IssueCreateEventArg::Down => IssueCreateAction::Down,
            IssueCreateEventArg::Left => IssueCreateAction::Left,
            IssueCreateEventArg::Right => IssueCreateAction::Right,
            IssueCreateEventArg::Tab => IssueCreateAction::Tab,
            IssueCreateEventArg::BackTab => IssueCreateAction::BackTab,
            IssueCreateEventArg::Enter => IssueCreateAction::Enter,
            IssueCreateEventArg::Esc => IssueCreateAction::Esc,
        }
    }
}

impl From<IssueEditEventArg> for IssueEditAction {
    fn from(value: IssueEditEventArg) -> Self {
        match value {
            IssueEditEventArg::Up => IssueEditAction::Up,
            IssueEditEventArg::Down => IssueEditAction::Down,
            IssueEditEventArg::Left => IssueEditAction::Left,
            IssueEditEventArg::Right => IssueEditAction::Right,
            IssueEditEventArg::Tab => IssueEditAction::Tab,
            IssueEditEventArg::BackTab => IssueEditAction::BackTab,
            IssueEditEventArg::Enter => IssueEditAction::Enter,
            IssueEditEventArg::Esc => IssueEditAction::Esc,
        }
    }
}

impl From<ListenAssignmentScopeArg> for ListenAssignmentScope {
    fn from(value: ListenAssignmentScopeArg) -> Self {
        match value {
            ListenAssignmentScopeArg::Any => ListenAssignmentScope::Any,
            ListenAssignmentScopeArg::Viewer => ListenAssignmentScope::Viewer,
        }
    }
}

impl From<ConfigEventArg> for ConfigAction {
    fn from(value: ConfigEventArg) -> Self {
        match value {
            ConfigEventArg::Up => ConfigAction::Up,
            ConfigEventArg::Down => ConfigAction::Down,
            ConfigEventArg::Tab => ConfigAction::Tab,
            ConfigEventArg::BackTab => ConfigAction::BackTab,
            ConfigEventArg::Enter => ConfigAction::Enter,
            ConfigEventArg::Esc => ConfigAction::Esc,
        }
    }
}

use std::io::IsTerminal;

use anyhow::{Result, anyhow};

use crate::cli::{DashboardCommandArgs, IssueCommands, LinearClientArgs, ProjectsCommands};
use crate::config::{LinearConfig, LinearConfigOverrides, PlanningMeta};
use crate::fs::canonicalize_existing_dir;
use crate::linear::create::{
    IssueCreateAction, IssueCreateFormContext, IssueCreateFormExit, IssueCreateFormOptions,
    IssueCreateFormPrefill, run_issue_create_form,
};
use crate::linear::dashboard::{DashboardAction, DashboardOptions, run_dashboard};
use crate::linear::edit::{
    IssueEditAction, IssueEditFormContext, IssueEditFormExit, IssueEditFormOptions,
    IssueEditFormPrefill, run_issue_edit_form,
};
use crate::linear::{
    DashboardData, DashboardFilters, IssueCreateSpec, IssueEditSpec, IssueListFilters,
    IssueSummary, LinearService, ProjectListFilters, ReqwestLinearClient, render_issue_summary,
    render_issues_list_output, render_projects_table, run_issue_refine_command,
};

pub(crate) async fn run_projects_command(
    client_args: &LinearClientArgs,
    cli_default_team: Option<String>,
    command: ProjectsCommands,
) -> Result<()> {
    let LinearCommandContext {
        service,
        default_team,
        ..
    } = load_linear_command_context(client_args, cli_default_team)?;

    match command {
        ProjectsCommands::List(list_args) => {
            let projects = service
                .list_projects(ProjectListFilters {
                    team: list_args.team.or(default_team),
                    limit: list_args.limit,
                })
                .await?;

            if list_args.json {
                println!("{}", serde_json::to_string_pretty(&projects)?);
            } else {
                println!("{}", render_projects_table(&projects));
            }
        }
    }

    Ok(())
}

pub(crate) async fn run_issues_command(
    client_args: &LinearClientArgs,
    cli_default_team: Option<String>,
    command: IssueCommands,
) -> Result<()> {
    let LinearCommandContext {
        service,
        default_team,
        default_project_id,
    } = load_linear_command_context(client_args, cli_default_team.clone())?;

    match command {
        IssueCommands::List(list_args) => {
            let applied_team = list_args.team.clone().or(default_team.clone());
            let applied_project_id = if list_args.project.is_some() {
                None
            } else {
                default_project_id.clone()
            };
            let issues = service
                .list_issues(IssueListFilters {
                    team: applied_team.clone(),
                    project_id: applied_project_id.clone(),
                    project: list_args.project.clone(),
                    state: list_args.state.clone(),
                    limit: list_args.limit,
                })
                .await?;
            if list_args.json {
                println!("{}", serde_json::to_string_pretty(&issues)?);
            } else if list_args.render_once
                || (std::io::stdin().is_terminal() && std::io::stdout().is_terminal())
            {
                let data = service
                    .load_dashboard(DashboardFilters {
                        team: applied_team.clone(),
                        project_id: applied_project_id.clone(),
                        project: list_args.project.clone(),
                        limit: list_args.limit,
                    })
                    .await?;

                let options = DashboardOptions {
                    render_once: list_args.render_once,
                    width: list_args.width,
                    height: list_args.height,
                    actions: list_args
                        .events
                        .into_iter()
                        .map(DashboardAction::from)
                        .collect(),
                    initial_state_filter: list_args.state,
                };

                if let Some(snapshot) = run_dashboard(data, options)? {
                    println!("{snapshot}");
                }
            } else {
                println!(
                    "{}",
                    render_issues_list_output(
                        &issues,
                        applied_team.as_deref(),
                        list_args.project.as_deref(),
                        applied_project_id.as_deref(),
                        list_args.state.as_deref(),
                    )
                );
            }
        }
        IssueCommands::Create(create_args) => {
            let explicit_project = create_args.project.clone();
            let defaulted_project_id = if explicit_project.is_some() {
                None
            } else {
                default_project_id.clone()
            };
            let can_launch_tui = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
            let run_non_interactive =
                create_args.no_interactive || (!create_args.render_once && !can_launch_tui);

            if run_non_interactive {
                let title = create_args.title.ok_or_else(|| {
                    anyhow!(
                        "`--title` is required when `--no-interactive` is used or when issue creation runs without a TTY"
                    )
                })?;
                let issue = service
                    .create_issue(IssueCreateSpec {
                        team: create_args.team.or(default_team.clone()),
                        title,
                        description: create_args.description,
                        project_id: defaulted_project_id.clone(),
                        project: explicit_project.clone(),
                        parent_id: None,
                        state: create_args.state,
                        priority: create_args.priority,
                        labels: Vec::new(),
                    })
                    .await?;

                println!("{}", render_issue_summary("Created issue", &issue));
            } else {
                let selected_team = service
                    .load_issue_create_team(create_args.team.clone().or(default_team.clone()))
                    .await?;
                let context = IssueCreateFormContext {
                    team_key: selected_team.key.clone(),
                    team_name: selected_team.name.clone(),
                    project: explicit_project
                        .clone()
                        .or_else(|| defaulted_project_id.clone()),
                    states: selected_team.states.clone(),
                };
                let exit = run_issue_create_form(
                    context,
                    IssueCreateFormPrefill {
                        title: create_args.title,
                        description: create_args.description,
                        state: create_args.state,
                        priority: create_args.priority,
                    },
                    IssueCreateFormOptions {
                        render_once: create_args.render_once,
                        width: create_args.width,
                        height: create_args.height,
                        actions: create_args
                            .events
                            .into_iter()
                            .map(IssueCreateAction::from)
                            .collect(),
                    },
                )?;

                match exit {
                    IssueCreateFormExit::Snapshot(snapshot) => println!("{snapshot}"),
                    IssueCreateFormExit::Cancelled => {
                        println!("Issue creation canceled.");
                    }
                    IssueCreateFormExit::Submitted(values) => {
                        let issue = service
                            .create_issue(IssueCreateSpec {
                                team: Some(selected_team.key),
                                title: values.title,
                                description: values.description,
                                project_id: defaulted_project_id.clone(),
                                project: explicit_project.clone(),
                                parent_id: None,
                                state: values.state,
                                priority: values.priority,
                                labels: Vec::new(),
                            })
                            .await?;

                        println!("{}", render_issue_summary("Created issue", &issue));
                    }
                }
            }
        }
        IssueCommands::Edit(edit_args) => {
            if edit_args.no_interactive {
                let issue = service
                    .edit_issue(IssueEditSpec {
                        identifier: edit_args.issue,
                        title: edit_args.title,
                        description: edit_args.description,
                        project: edit_args.project,
                        state: edit_args.state,
                        priority: edit_args.priority,
                    })
                    .await?;

                println!("{}", render_issue_summary("Updated issue", &issue));
            } else {
                let edit_context = service.load_issue_edit_context(&edit_args.issue).await?;
                let existing_issue = edit_context.issue;
                let exit = run_issue_edit_form(
                    IssueEditFormContext {
                        issue_identifier: existing_issue.identifier.clone(),
                        team_key: edit_context.team.key.clone(),
                        team_name: edit_context.team.name.clone(),
                        current_project: existing_issue
                            .project
                            .as_ref()
                            .map(|project| project.name.clone()),
                        pending_project: edit_args.project.clone(),
                        states: edit_context.team.states,
                    },
                    IssueEditFormPrefill {
                        title: edit_args
                            .title
                            .clone()
                            .unwrap_or_else(|| existing_issue.title.clone()),
                        description: edit_args
                            .description
                            .clone()
                            .or_else(|| existing_issue.description.clone()),
                        state: edit_args.state.clone().or_else(|| {
                            existing_issue
                                .state
                                .as_ref()
                                .map(|state| state.name.clone())
                        }),
                        priority: edit_args.priority.or(existing_issue.priority),
                    },
                    IssueEditFormOptions {
                        render_once: edit_args.render_once,
                        width: edit_args.width,
                        height: edit_args.height,
                        actions: edit_args
                            .events
                            .into_iter()
                            .map(IssueEditAction::from)
                            .collect(),
                    },
                )?;

                match exit {
                    IssueEditFormExit::Snapshot(snapshot) => println!("{snapshot}"),
                    IssueEditFormExit::Cancelled => {
                        println!("Issue edit canceled.");
                    }
                    IssueEditFormExit::Submitted(values) => {
                        let issue = service
                            .edit_issue(IssueEditSpec {
                                identifier: existing_issue.identifier.clone(),
                                title: changed_title(&existing_issue, &values.title),
                                description: changed_description(
                                    &existing_issue,
                                    values.description.as_deref(),
                                ),
                                project: edit_args.project,
                                state: changed_state(&existing_issue, values.state.as_deref()),
                                priority: changed_priority(&existing_issue, values.priority),
                            })
                            .await?;

                        println!("{}", render_issue_summary("Updated issue", &issue));
                    }
                }
            }
        }
        IssueCommands::Refine(refine_args) => {
            run_issue_refine_command(client_args, cli_default_team, refine_args).await?;
        }
    }

    Ok(())
}

pub(crate) async fn run_dashboard_command(
    client_args: &LinearClientArgs,
    cli_default_team: Option<String>,
    dashboard_args: DashboardCommandArgs,
) -> Result<()> {
    let LinearCommandContext {
        service,
        default_team,
        default_project_id,
    } = load_linear_command_context(client_args, cli_default_team)?;
    let project = dashboard_args.project;

    let data = if dashboard_args.demo {
        DashboardData::demo()
    } else {
        service
            .load_dashboard(DashboardFilters {
                team: dashboard_args.team.or(default_team),
                project_id: if project.is_some() {
                    None
                } else {
                    default_project_id
                },
                project,
                limit: dashboard_args.limit,
            })
            .await?
    };

    let options = DashboardOptions {
        render_once: dashboard_args.render_once,
        width: dashboard_args.width,
        height: dashboard_args.height,
        actions: dashboard_args
            .events
            .into_iter()
            .map(DashboardAction::from)
            .collect(),
        initial_state_filter: None,
    };

    if let Some(snapshot) = run_dashboard(data, options)? {
        println!("{snapshot}");
    }

    Ok(())
}

pub(crate) struct LinearCommandContext {
    pub(crate) service: LinearService<ReqwestLinearClient>,
    pub(crate) default_team: Option<String>,
    pub(crate) default_project_id: Option<String>,
}

pub(crate) fn load_linear_command_context(
    client_args: &LinearClientArgs,
    cli_default_team: Option<String>,
) -> Result<LinearCommandContext> {
    let root = canonicalize_existing_dir(&client_args.root)?;
    let planning_meta = PlanningMeta::load(&root)?;
    let config = LinearConfig::new_with_root(
        Some(&root),
        LinearConfigOverrides {
            api_key: client_args.api_key.clone(),
            api_url: client_args.api_url.clone(),
            default_team: cli_default_team,
            profile: client_args.profile.clone(),
        },
    )?;
    let default_team = config.default_team.clone();
    let default_project_id = planning_meta.linear.project_id.clone();
    let client = ReqwestLinearClient::new(config)?;
    let service = LinearService::new(client, default_team.clone());

    Ok(LinearCommandContext {
        service,
        default_team,
        default_project_id,
    })
}

fn changed_title(issue: &IssueSummary, edited_title: &str) -> Option<String> {
    (issue.title != edited_title).then(|| edited_title.to_string())
}

fn changed_description(issue: &IssueSummary, edited_description: Option<&str>) -> Option<String> {
    match (issue.description.as_deref(), edited_description) {
        (None, None) => None,
        (Some(current), Some(edited)) if current == edited => None,
        (Some(_), None) => Some(String::new()),
        (_, Some(edited)) => Some(edited.to_string()),
    }
}

fn changed_state(issue: &IssueSummary, edited_state: Option<&str>) -> Option<String> {
    match (
        issue.state.as_ref().map(|state| state.name.as_str()),
        edited_state,
    ) {
        (Some(current), Some(edited)) if current.eq_ignore_ascii_case(edited) => None,
        (_, Some(edited)) => Some(edited.to_string()),
        _ => None,
    }
}

fn changed_priority(issue: &IssueSummary, edited_priority: Option<u8>) -> Option<u8> {
    match (issue.priority, edited_priority) {
        (None, None) => None,
        (Some(current), Some(edited)) if current == edited => None,
        (Some(_), None) => Some(0),
        (_, Some(edited)) => Some(edited),
    }
}

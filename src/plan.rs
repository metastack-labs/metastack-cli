use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
#[cfg(test)]
use ratatui::backend::TestBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::macros::format_description;

use crate::agents::{AgentContinuation, run_agent_capture, run_agent_capture_with_continuation};
use crate::backlog::{
    BacklogIssueMetadata, INDEX_FILE_NAME, ManagedFileRecord, RenderedTemplateFile,
    TemplateContext, ensure_no_unresolved_placeholders, render_template_files, save_issue_metadata,
    write_rendered_backlog_item,
};
use crate::backlog_defaults::{
    PlanTicketResolutionInput, TicketOptionOverrides, load_remembered_backlog_selection,
    resolve_plan_ticket_defaults, save_remembered_backlog_selection,
};
use crate::cli::{PlanArgs, RunAgentArgs};
use crate::config::{
    AGENT_ROUTE_BACKLOG_PLAN, AppConfig, LinearConfig, LinearConfigOverrides,
    load_required_planning_meta,
};
use crate::context::load_workflow_contract;
use crate::fs::{PlanningPaths, canonicalize_existing_dir};
use crate::linear::{
    IssueCreateSpec, IssueEditSpec, IssueSummary, LinearService, ReqwestLinearClient,
};
use crate::progress::{LoadingPanelData, SPINNER_FRAMES, render_loading_panel};
use crate::scaffold::ensure_planning_layout;
use crate::text_diff::render_text_diff;
use crate::tui::fields::InputFieldState;
use crate::tui::prompt_images::PromptImageAttachment;
use crate::tui::scroll::{ScrollState, plain_text, scrollable_paragraph, wrapped_rows};

const NON_INTERACTIVE_MAX_FOLLOW_UP_QUESTIONS: usize = 3;
const SKIPPED_FOLLOW_UP_LABEL: &str = "Skipped intentionally.";

#[derive(Debug, Clone)]
pub enum PlanReport {
    Cancelled,
    Created { issues: Vec<IssueSummary> },
    Reshaped { identifier: String, url: String },
}

#[derive(Debug, Deserialize)]
struct FollowUpQuestions {
    #[serde(default)]
    questions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PlannedIssueDraft {
    title: String,
    description: String,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
    #[serde(default)]
    priority: Option<u8>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct PlannedIssueSet {
    summary: String,
    #[serde(default)]
    issues: Vec<PlannedIssueDraft>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ReshapedIssueDraft {
    #[serde(default)]
    summary: String,
    title: String,
    description: String,
    #[serde(default)]
    acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone)]
struct FollowUpResponse {
    question: String,
    answer: String,
    skipped: bool,
    attachments: Vec<PromptImageAttachment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FollowUpAnswerState {
    Pending,
    Answered,
    Skipped,
}

#[derive(Debug, Clone)]
struct QuestionAnswer {
    question: String,
    answer: InputFieldState,
    state: FollowUpAnswerState,
}

#[derive(Debug, Clone)]
struct RequestApp {
    request: InputFieldState,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct QuestionsApp {
    request: String,
    request_attachments: Vec<PromptImageAttachment>,
    questions: Vec<QuestionAnswer>,
    selected: usize,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct ReviewApp {
    request: String,
    request_attachments: Vec<PromptImageAttachment>,
    follow_ups: Vec<FollowUpResponse>,
    plan: PlannedIssueSet,
    selected: usize,
    decisions: Vec<usize>,
    revision: usize,
    focus: ReviewFocus,
    overview_scroll: ScrollState,
    selected_ticket_scroll: ScrollState,
    combination_scroll: ScrollState,
    error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewFocus {
    Tickets,
    SelectedTicket,
    Overview,
    CombinationPlan,
}

#[derive(Debug, Clone)]
struct LoadingApp {
    message: String,
    detail: String,
    spinner_index: usize,
}

#[derive(Debug, Clone)]
enum PlanStage {
    Request(RequestApp),
    Questions(QuestionsApp),
    Review(ReviewApp),
    Loading(LoadingApp),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanStageKind {
    Request,
    Questions,
    Review,
    Loading,
}

struct PlanSessionApp {
    stage: PlanStage,
    agent_overrides: PlanningAgentOverrides,
    continuation: Option<AgentContinuation>,
    pending: Option<PendingPlanJob>,
}

struct PendingPlanJob {
    receiver: Receiver<PlanWorkerReport>,
    previous_stage: PlanStage,
}

struct PlanWorkerReport {
    continuation: Option<AgentContinuation>,
    outcome: Result<PlanWorkerOutcome>,
}

enum PlanWorkerOutcome {
    Questions {
        request: String,
        request_attachments: Vec<PromptImageAttachment>,
        questions: Vec<String>,
    },
    Review(ReviewApp),
}

enum InteractivePlanExit {
    Cancelled,
    Confirmed(PlannedIssueSet),
}

#[derive(Debug, Clone, Default)]
struct PlanningAgentOverrides {
    agent: Option<String>,
    model: Option<String>,
    reasoning: Option<String>,
}

enum PlanMode {
    Create,
    Reshape { identifier: String },
}

pub async fn run_plan(args: &PlanArgs) -> Result<PlanReport> {
    let root = canonicalize_existing_dir(&args.client.root)?;
    let app_config = AppConfig::load()?;
    let planning_meta = load_required_planning_meta(&root, "plan")?;
    ensure_planning_layout(&root, false)?;
    let config = LinearConfig::new_with_root(
        Some(&root),
        LinearConfigOverrides {
            api_key: args.client.api_key.clone(),
            api_url: args.client.api_url.clone(),
            default_team: args.team.clone(),
            profile: args.client.profile.clone(),
        },
    )?;
    let default_team = config.default_team.clone();
    let service = LinearService::new(ReqwestLinearClient::new(config)?, default_team.clone());
    let can_launch_tui = io::stdin().is_terminal() && io::stdout().is_terminal();
    let run_non_interactive = args.no_interactive || !can_launch_tui;
    let remembered_selection = if run_non_interactive {
        load_remembered_backlog_selection(&root)?
    } else {
        Default::default()
    };
    let agent_overrides = PlanningAgentOverrides {
        agent: args.agent.clone(),
        model: args.model.clone(),
        reasoning: args.reasoning.clone(),
    };

    match resolve_plan_mode(args.target.as_deref())? {
        PlanMode::Create => {}
        PlanMode::Reshape { identifier } => {
            return run_reshape_plan(&root, &service, &identifier, args, &agent_overrides).await;
        }
    }

    let plan = if run_non_interactive {
        let request = args.request.clone().ok_or_else(|| {
            anyhow!(
                "`--request` is required when `--no-interactive` is used or when `meta plan` runs without a TTY"
            )
        })?;
        let mut continuation = None;

        let questions = generate_follow_up_questions(
            &root,
            &request,
            Vec::new(),
            NON_INTERACTIVE_MAX_FOLLOW_UP_QUESTIONS,
            &agent_overrides,
            &mut continuation,
        )?;
        let answers = if questions.is_empty() {
            Vec::new()
        } else {
            if args.answers.len() != questions.len() {
                bail!(
                    "planning agent requested {} follow-up question(s); pass exactly {} `--answer` value(s)",
                    questions.len(),
                    questions.len()
                );
            }
            args.answers.clone()
        };
        let follow_ups = questions
            .into_iter()
            .zip(answers)
            .map(|(question, answer)| FollowUpResponse {
                question,
                answer,
                skipped: false,
                attachments: Vec::new(),
            })
            .collect::<Vec<_>>();

        let plan = generate_issue_plan(
            &root,
            &request,
            &follow_ups,
            Vec::new(),
            &agent_overrides,
            &mut continuation,
        )?;
        if plan.issues.is_empty() {
            bail!("planning agent returned no issues to create");
        }
        plan
    } else {
        match run_interactive_plan_session(
            &root,
            args.request.clone(),
            planning_meta.effective_interactive_follow_up_question_limit(&app_config),
            agent_overrides.clone(),
        )? {
            InteractivePlanExit::Cancelled => return Ok(PlanReport::Cancelled),
            InteractivePlanExit::Confirmed(plan) => plan,
        }
    };

    let mut created_issues = Vec::with_capacity(plan.issues.len());
    for draft in &plan.issues {
        let resolved_defaults = resolve_plan_ticket_defaults(
            &app_config,
            &planning_meta,
            &remembered_selection,
            &PlanTicketResolutionInput {
                zero_prompt: run_non_interactive,
                explicit_team: args.team.clone(),
                explicit_project: args.project.clone(),
                overrides: TicketOptionOverrides {
                    state: args.state.clone(),
                    priority: args.priority,
                    labels: args.labels.clone(),
                    assignee: args.assignee.clone(),
                },
                built_in_label: planning_meta.effective_plan_label(&app_config),
                generated_priority: draft.priority,
            },
        );
        let initial_files = render_planned_backlog_files(
            &root,
            draft,
            TemplateContext {
                issue_title: Some(draft.title.clone()),
                ..TemplateContext::default()
            },
        )?;
        let initial_description = rendered_index_contents(&initial_files)?;
        let assignee_id = service
            .resolve_assignee_id(resolved_defaults.assignee.as_deref())
            .await?;
        let issue = service
            .create_issue(IssueCreateSpec {
                team: resolved_defaults.team.clone(),
                title: draft.title.clone(),
                description: Some(initial_description),
                project: resolved_defaults.project.clone(),
                project_id: resolved_defaults.project_id.clone(),
                parent_id: None,
                state: resolved_defaults.state.clone(),
                priority: resolved_defaults.priority,
                assignee_id,
                labels: resolved_defaults.labels.clone(),
            })
            .await?;
        if let Err(error) = save_remembered_backlog_selection(&root, &issue) {
            eprintln!("warning: failed to persist remembered backlog defaults: {error}");
        }
        let rendered_files = render_planned_backlog_files(
            &root,
            draft,
            TemplateContext {
                issue_identifier: Some(issue.identifier.clone()),
                issue_title: Some(issue.title.clone()),
                issue_url: Some(issue.url.clone()),
                ..TemplateContext::default()
            },
        )?;
        let issue_dir = write_rendered_backlog_item(&root, &issue.identifier, &rendered_files)?;
        save_issue_metadata(
            &issue_dir,
            &BacklogIssueMetadata {
                issue_id: issue.id.clone(),
                identifier: issue.identifier.clone(),
                title: issue.title.clone(),
                url: issue.url.clone(),
                team_key: issue.team.key.clone(),
                project_id: issue.project.as_ref().map(|project| project.id.clone()),
                project_name: issue.project.as_ref().map(|project| project.name.clone()),
                parent_id: None,
                parent_identifier: None,
                local_hash: None,
                remote_hash: None,
                last_sync_at: None,
                last_pulled_comment_ids: Vec::new(),
                managed_files: Vec::<ManagedFileRecord>::new(),
            },
        )?;
        created_issues.push(issue);
    }

    Ok(PlanReport::Created {
        issues: created_issues,
    })
}

impl PlanReport {
    pub fn render(&self) -> String {
        match self {
            Self::Cancelled => "Planning canceled.".to_string(),
            Self::Created { issues } => {
                let mut lines = vec![format!("Created {} backlog issue(s):", issues.len())];
                for issue in issues {
                    lines.push(format!("- {}: {}", issue.identifier, issue.url));
                }
                lines.join("\n")
            }
            Self::Reshaped { identifier, url } => {
                format!("Reshaped {identifier} in place: {url}")
            }
        }
    }
}

async fn run_reshape_plan(
    root: &Path,
    service: &LinearService<ReqwestLinearClient>,
    identifier: &str,
    args: &PlanArgs,
    overrides: &PlanningAgentOverrides,
) -> Result<PlanReport> {
    let issue = service.load_issue(identifier).await?;
    let draft = generate_issue_reshape(root, &issue, overrides)?;
    let proposed_description = render_reshaped_index_contents(&issue, &draft);
    let preview = render_reshape_preview(&issue, &draft, &proposed_description);

    if !args.velocity {
        if args.no_interactive {
            bail!(
                "`meta backlog plan {identifier}` requires diff confirmation unless `--velocity` is set; rerun without `--no-interactive` to review the preview or pass `--velocity` to auto-apply"
            );
        }

        if !prompt_reshape_apply(identifier, &preview)? {
            return Ok(PlanReport::Cancelled);
        }
    }

    let updated_issue = service
        .edit_issue(IssueEditSpec {
            identifier: issue.identifier.clone(),
            title: Some(draft.title.clone()),
            description: Some(proposed_description),
            project: None,
            state: None,
            priority: None,
            estimate: None,
            labels: None,
            parent_identifier: None,
        })
        .await?;
    service
        .upsert_workpad_comment(
            &issue,
            render_reshape_workpad_comment(&issue, &updated_issue, &draft, args.velocity),
        )
        .await?;

    Ok(PlanReport::Reshaped {
        identifier: updated_issue.identifier.clone(),
        url: updated_issue.url.clone(),
    })
}

fn resolve_plan_mode(target: Option<&str>) -> Result<PlanMode> {
    let Some(target) = target.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(PlanMode::Create);
    };

    if is_strict_issue_identifier(target) {
        return Ok(PlanMode::Reshape {
            identifier: target.to_string(),
        });
    }

    bail!(
        "`meta backlog plan <IDENTIFIER>` only accepts existing issue identifiers like `ENG-10144`; use `--request` for new backlog planning"
    );
}

fn is_strict_issue_identifier(value: &str) -> bool {
    let Some((team, number)) = value.split_once('-') else {
        return false;
    };
    if team.is_empty() || number.is_empty() {
        return false;
    }

    let team_valid = team
        .chars()
        .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
        && team.chars().any(|character| character.is_ascii_uppercase());
    let number_valid = number.chars().all(|character| character.is_ascii_digit());

    team_valid && number_valid
}

fn generate_issue_reshape(
    root: &Path,
    issue: &IssueSummary,
    overrides: &PlanningAgentOverrides,
) -> Result<ReshapedIssueDraft> {
    let prompt = render_reshape_prompt(root, issue)?;
    let output = run_agent_capture(&RunAgentArgs {
        root: Some(root.to_path_buf()),
        route_key: Some(AGENT_ROUTE_BACKLOG_PLAN.to_string()),
        agent: overrides.agent.clone(),
        prompt,
        instructions: None,
        model: overrides.model.clone(),
        reasoning: overrides.reasoning.clone(),
        transport: None,
        attachments: Vec::new(),
    })?;
    let parsed: ReshapedIssueDraft = parse_agent_json(&output.stdout, "issue reshape")?;
    let draft = ReshapedIssueDraft {
        summary: parsed.summary.trim().to_string(),
        title: parsed.title.trim().to_string(),
        description: parsed.description.trim().to_string(),
        acceptance_criteria: parsed
            .acceptance_criteria
            .into_iter()
            .map(|criterion| criterion.trim().to_string())
            .filter(|criterion| !criterion.is_empty())
            .collect(),
    };

    if draft.title.is_empty() || draft.description.is_empty() {
        bail!("planning agent returned an empty title or description during issue reshape");
    }

    Ok(draft)
}

fn generate_follow_up_questions(
    root: &Path,
    request: &str,
    attachments: Vec<PromptImageAttachment>,
    max_questions: usize,
    overrides: &PlanningAgentOverrides,
    continuation: &mut Option<AgentContinuation>,
) -> Result<Vec<String>> {
    let prompt = render_question_prompt(root, request, max_questions)?;
    let output = run_agent_capture_with_continuation(
        &RunAgentArgs {
            root: Some(root.to_path_buf()),
            route_key: Some(AGENT_ROUTE_BACKLOG_PLAN.to_string()),
            agent: overrides.agent.clone(),
            prompt,
            instructions: None,
            model: overrides.model.clone(),
            reasoning: overrides.reasoning.clone(),
            transport: None,
            attachments,
        },
        continuation,
    )?;
    let parsed: FollowUpQuestions =
        parse_agent_json(&output.stdout, "follow-up question generation")?;

    Ok(parsed
        .questions
        .into_iter()
        .map(|question| question.trim().to_string())
        .filter(|question| !question.is_empty())
        .take(max_questions)
        .collect())
}

fn generate_issue_plan(
    root: &Path,
    request: &str,
    follow_ups: &[FollowUpResponse],
    attachments: Vec<PromptImageAttachment>,
    overrides: &PlanningAgentOverrides,
    continuation: &mut Option<AgentContinuation>,
) -> Result<PlannedIssueSet> {
    let prompt = render_issue_plan_prompt(root, request, follow_ups)?;
    let output = run_agent_capture_with_continuation(
        &RunAgentArgs {
            root: Some(root.to_path_buf()),
            route_key: Some(AGENT_ROUTE_BACKLOG_PLAN.to_string()),
            agent: overrides.agent.clone(),
            prompt,
            instructions: None,
            model: overrides.model.clone(),
            reasoning: overrides.reasoning.clone(),
            transport: None,
            attachments,
        },
        continuation,
    )?;
    let parsed: PlannedIssueSet = parse_agent_json(&output.stdout, "issue planning")?;

    Ok(PlannedIssueSet {
        summary: parsed.summary.trim().to_string(),
        issues: parsed
            .issues
            .into_iter()
            .map(|draft| PlannedIssueDraft {
                title: draft.title.trim().to_string(),
                description: draft.description.trim().to_string(),
                acceptance_criteria: draft
                    .acceptance_criteria
                    .into_iter()
                    .map(|criterion| criterion.trim().to_string())
                    .filter(|criterion| !criterion.is_empty())
                    .collect(),
                priority: draft.priority,
            })
            .filter(|draft| !draft.title.is_empty() && !draft.description.is_empty())
            .collect(),
    })
}

fn render_question_prompt(root: &Path, request: &str, max_questions: usize) -> Result<String> {
    let context = load_context_bundle(root)?;
    let workflow_contract = load_workflow_contract(root)?;
    Ok(format!(
        "You are helping plan backlog work for the active repository.\n\n\
Injected workflow contract:\n{workflow_contract}\n\n\
User request:\n{request}\n\n\
Repository planning context:\n{context}\n\n\
Ask at most {max_questions} concise follow-up questions that would materially change how this request should be split into Linear backlog issues for this repository only. Default scope to the full repository root unless the user explicitly asks for a narrower subproject. If the request is already specific enough, return an empty list.\n\n\
Return JSON only using this exact shape:\n{{\"questions\":[\"Question 1\",\"Question 2\"]}}"
    ))
}

fn render_issue_plan_prompt(
    root: &Path,
    request: &str,
    follow_ups: &[FollowUpResponse],
) -> Result<String> {
    let context = load_context_bundle(root)?;
    let workflow_contract = load_workflow_contract(root)?;
    let follow_up_block = render_follow_up_block(follow_ups);

    Ok(format!(
        "You are helping plan backlog work for the active repository.\n\n\
Injected workflow contract:\n{workflow_contract}\n\n\
User request:\n{request}\n\n\
Follow-up answers:\n{follow_up_block}\n\n\
Repository planning context:\n{context}\n\n\
Break the work into 1 to 5 actionable Linear backlog issues for this repository directory only. Each issue must be independently understandable, ready to create in Backlog, and scoped to the target repository unless the user explicitly requested a narrower subproject.\n\n\
Return JSON only using this exact shape:\n\
{{\n  \"summary\":\"One paragraph summary of the overall plan\",\n  \"issues\":[\n    {{\n      \"title\":\"Issue title\",\n      \"description\":\"Short markdown description\",\n      \"acceptance_criteria\":[\"criterion one\",\"criterion two\"],\n      \"priority\": 2\n    }}\n  ]\n}}",
    ))
}

fn render_reshape_prompt(root: &Path, issue: &IssueSummary) -> Result<String> {
    let context = load_context_bundle(root)?;
    let workflow_contract = load_workflow_contract(root)?;
    let issue_json = serde_json::to_string_pretty(issue)
        .context("failed to serialize existing issue context")?;

    Ok(format!(
        "You are reshaping an existing Linear issue in place for the active repository.\n\n\
Injected workflow contract:\n{workflow_contract}\n\n\
Existing issue context JSON:\n{issue_json}\n\n\
Repository planning context:\n{context}\n\n\
Preserve the issue's intent while improving structure, scope boundaries, and acceptance criteria. Keep this as one issue, do not split it into multiple tickets, and do not invent metadata changes for assignee, labels, project, state, cycle, or priority because those fields are preserved separately by the CLI.\n\n\
Return JSON only using this exact shape:\n\
{{\n  \"summary\":\"One paragraph summary of the reshape\",\n  \"title\":\"Replacement issue title\",\n  \"description\":\"Replacement markdown body without the leading H1 title line\",\n  \"acceptance_criteria\":[\"criterion one\",\"criterion two\"]\n}}",
    ))
}

fn render_issue_merge_prompt(
    root: &Path,
    request: &str,
    follow_ups: &[FollowUpResponse],
    plan: &PlannedIssueSet,
    kept_indices: &[usize],
    merge_groups: &BTreeMap<usize, Vec<usize>>,
) -> Result<String> {
    let context = load_context_bundle(root)?;
    let workflow_contract = load_workflow_contract(root)?;
    let follow_up_block = render_follow_up_block(follow_ups);
    let current_plan = serde_json::to_string_pretty(plan)
        .context("failed to serialize the current ticket draft for revision")?;
    let kept_tickets = kept_indices
        .iter()
        .filter_map(|index| plan.issues.get(*index))
        .cloned()
        .collect::<Vec<_>>();
    let kept_tickets_json = serde_json::to_string_pretty(&kept_tickets)
        .context("failed to serialize standalone tickets for revision")?;
    let merge_plan = merge_groups
        .iter()
        .map(|(group, indices)| {
            let tickets = indices
                .iter()
                .filter_map(|index| plan.issues.get(*index))
                .cloned()
                .collect::<Vec<_>>();
            serde_json::json!({
                "group": group,
                "tickets": tickets,
            })
        })
        .collect::<Vec<_>>();
    let merge_plan_json = serde_json::to_string_pretty(&merge_plan)
        .context("failed to serialize merge groups for revision")?;

    Ok(format!(
        "You are revising a backlog ticket plan for the active repository.\n\n\
Injected workflow contract:\n{workflow_contract}\n\n\
User request:\n{request}\n\n\
Follow-up answers:\n{follow_up_block}\n\n\
Repository planning context:\n{context}\n\n\
Current draft plan JSON:\n{current_plan}\n\n\
Selected standalone tickets to preserve:\n{kept_tickets_json}\n\n\
Merge groups:\n{merge_plan_json}\n\n\
Rebuild the next issue list from only the selected standalone tickets plus the numbered merge groups. Tickets omitted from both lists were intentionally skipped and must not appear in the rebuilt output. Preserve implementation details, acceptance criteria, and sequencing for the selected scope. For each merge group, combine all tickets in that group into exactly one replacement ticket unless a tiny wording edit is needed for coherence. Return 1 to 5 actionable Linear backlog issues for this repository only, defaulting scope to the repository root unless the user explicitly requested a narrower subproject.\n\n\
Return JSON only using this exact shape:\n\
{{\n  \"summary\":\"One paragraph summary of the overall plan\",\n  \"issues\":[\n    {{\n      \"title\":\"Issue title\",\n      \"description\":\"Short markdown description\",\n      \"acceptance_criteria\":[\"criterion one\",\"criterion two\"],\n      \"priority\": 2\n    }}\n  ]\n}}",
    ))
}

fn render_follow_up_block(follow_ups: &[FollowUpResponse]) -> String {
    if follow_ups.is_empty() {
        "No follow-up questions were required.".to_string()
    } else {
        follow_ups
            .iter()
            .enumerate()
            .map(|(index, follow_up)| {
                let answer = if follow_up.skipped {
                    SKIPPED_FOLLOW_UP_LABEL.to_string()
                } else {
                    follow_up.answer.clone()
                };
                format!("{}. Q: {}\n   A: {}", index + 1, follow_up.question, answer)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn collect_prompt_attachments(
    request_attachments: &[PromptImageAttachment],
    follow_ups: &[FollowUpResponse],
) -> Vec<PromptImageAttachment> {
    let mut attachments = request_attachments.to_vec();
    for follow_up in follow_ups {
        attachments.extend(follow_up.attachments.clone());
    }
    attachments
}

fn load_context_bundle(root: &Path) -> Result<String> {
    let paths = PlanningPaths::new(root);
    let sections = [
        ("SCAN.md", paths.scan_path()),
        ("ARCHITECTURE.md", paths.architecture_path()),
        ("CONVENTIONS.md", paths.conventions_path()),
        ("STACK.md", paths.stack_path()),
        ("STRUCTURE.md", paths.structure_path()),
        ("TESTING.md", paths.testing_path()),
    ];
    let mut lines = Vec::new();

    for (title, path) in sections {
        lines.push(format!("## {title}"));
        lines.push(String::new());
        lines.push(read_context(&path)?);
        lines.push(String::new());
    }

    Ok(lines.join("\n"))
}

fn read_context(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(format!(
            "_Missing `{}`. Run `meta scan` to generate it._",
            path.file_name()
                .map(|value| value.to_string_lossy())
                .unwrap_or_default()
        )),
        Err(error) => Err(error).with_context(|| format!("failed to read `{}`", path.display())),
    }
}

fn parse_agent_json<T>(raw: &str, phase: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let trimmed = raw.trim();
    let mut candidates = vec![trimmed.to_string()];

    if let Some(stripped) = strip_code_fence(trimmed) {
        candidates.push(stripped);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}'))
        && start <= end
    {
        candidates.push(trimmed[start..=end].to_string());
    }

    for candidate in candidates {
        if let Ok(parsed) = serde_json::from_str::<T>(&candidate) {
            return Ok(parsed);
        }
    }

    bail!(
        "planning agent returned invalid JSON during {phase}: {}",
        preview_text(trimmed)
    )
}

fn strip_code_fence(raw: &str) -> Option<String> {
    let stripped = raw.strip_prefix("```")?;
    let stripped = stripped
        .strip_prefix("json\n")
        .or_else(|| stripped.strip_prefix("JSON\n"))
        .or_else(|| stripped.strip_prefix('\n'))
        .unwrap_or(stripped);
    let stripped = stripped.strip_suffix("```")?;
    Some(stripped.trim().to_string())
}

fn preview_text(value: &str) -> String {
    const MAX_PREVIEW_LEN: usize = 240;
    if value.len() <= MAX_PREVIEW_LEN {
        value.to_string()
    } else {
        format!("{}...", &value[..MAX_PREVIEW_LEN])
    }
}

fn render_planned_backlog_files(
    root: &Path,
    draft: &PlannedIssueDraft,
    context: TemplateContext,
) -> Result<Vec<RenderedTemplateFile>> {
    let mut rendered_files = render_template_files(root, &context)?;
    let index_file = rendered_files
        .iter_mut()
        .find(|file| file.relative_path == INDEX_FILE_NAME)
        .ok_or_else(|| anyhow!("the backlog template must contain `{INDEX_FILE_NAME}`"))?;
    index_file.contents = render_planned_index_contents(draft);
    ensure_no_unresolved_placeholders(&rendered_files)?;
    Ok(rendered_files)
}

fn rendered_index_contents(rendered_files: &[RenderedTemplateFile]) -> Result<String> {
    rendered_files
        .iter()
        .find(|file| file.relative_path == INDEX_FILE_NAME)
        .map(|file| file.contents.clone())
        .ok_or_else(|| anyhow!("the backlog template must contain `{INDEX_FILE_NAME}`"))
}

fn render_planned_index_contents(draft: &PlannedIssueDraft) -> String {
    let mut lines = vec![
        format!("# {}", draft.title),
        String::new(),
        draft.description.clone(),
    ];

    if !draft.acceptance_criteria.is_empty() {
        lines.push(String::new());
        lines.push("## Acceptance Criteria".to_string());
        lines.push(String::new());
        lines.extend(
            draft
                .acceptance_criteria
                .iter()
                .map(|criterion| format!("- {criterion}")),
        );
    }

    lines.join("\n")
}

fn render_reshaped_index_contents(issue: &IssueSummary, draft: &ReshapedIssueDraft) -> String {
    render_planned_index_contents(&PlannedIssueDraft {
        title: draft.title.clone(),
        description: draft.description.clone(),
        acceptance_criteria: draft.acceptance_criteria.clone(),
        priority: issue.priority,
    })
}

fn render_reshape_preview(
    issue: &IssueSummary,
    draft: &ReshapedIssueDraft,
    proposed_description: &str,
) -> String {
    let title_status = if issue.title == draft.title {
        format!("  {}", issue.title)
    } else {
        format!("- {}\n+ {}", issue.title, draft.title)
    };
    let description_diff = render_text_diff(
        "linear/current-description",
        "linear/proposed-description",
        issue.description.as_deref().unwrap_or_default(),
        proposed_description,
    );

    format!(
        "`meta backlog plan {}` prepared an in-place reshape preview:\n\nTitle:\n{}\n\nDescription diff:\n{}\n\nMetadata preserved on apply: assignee, labels, project, state, priority, and cycle.\nLocal `.metastack/backlog/` files are unchanged in reshape mode.",
        issue.identifier, title_status, description_diff
    )
}

fn prompt_reshape_apply(identifier: &str, preview: &str) -> Result<bool> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    prompt_reshape_apply_with_io(identifier, preview, &mut reader, &mut writer)
}

fn prompt_reshape_apply_with_io(
    identifier: &str,
    preview: &str,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> Result<bool> {
    writeln!(writer, "{preview}")?;
    writeln!(
        writer,
        "Choose [a]pply or [c]ancel for `meta backlog plan {identifier}`:"
    )?;
    writer.flush()?;

    let mut input = String::new();
    loop {
        input.clear();
        reader.read_line(&mut input)?;
        match input.trim().to_ascii_lowercase().as_str() {
            "a" | "apply" => return Ok(true),
            "c" | "cancel" => return Ok(false),
            _ => {
                writeln!(writer, "Enter `a` or `c`:")?;
                writer.flush()?;
            }
        }
    }
}

fn render_reshape_workpad_comment(
    original_issue: &IssueSummary,
    updated_issue: &IssueSummary,
    draft: &ReshapedIssueDraft,
    velocity: bool,
) -> String {
    let mut lines = vec![
        "## Codex Workpad".to_string(),
        String::new(),
        format!("- Reshape applied: {}", reshape_timestamp()),
        format!(
            "- Command: `meta backlog plan {}{}`",
            original_issue.identifier,
            if velocity { " --velocity" } else { "" }
        ),
    ];

    if !draft.summary.is_empty() {
        lines.push(format!("- Summary: {}", draft.summary));
    }
    if original_issue.title != updated_issue.title {
        lines.push(format!(
            "- Title: `{}` -> `{}`",
            original_issue.title, updated_issue.title
        ));
    }
    lines.push(
        "- Metadata preserved: assignee, labels, project, state, priority, and cycle were left unchanged."
            .to_string(),
    );
    lines.push(
        "- Local `.metastack/backlog/` files were not modified by this reshape flow.".to_string(),
    );

    lines.join("\n")
}

fn reshape_timestamp() -> String {
    let format = format_description!("[year]-[month]-[day] [hour]:[minute]:[second] UTC");
    OffsetDateTime::now_utc()
        .format(&format)
        .unwrap_or_else(|_| "unknown time".to_string())
}

fn run_interactive_plan_session(
    root: &Path,
    prefill: Option<String>,
    follow_up_question_limit: usize,
    agent_overrides: PlanningAgentOverrides,
) -> Result<InteractivePlanExit> {
    let mut app = PlanSessionApp {
        stage: PlanStage::Request(RequestApp {
            request: InputFieldState::multiline_with_prompt_attachments(
                prefill.unwrap_or_default(),
            ),
            error: None,
        }),
        agent_overrides,
        continuation: None,
        pending: None,
    };
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
    let _cleanup = TerminalCleanup;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut previous_stage = stage_kind(&app.stage);
    terminal.clear()?;

    loop {
        if let Some(exit) = process_pending_plan_job(&mut app, root)? {
            return Ok(exit);
        }
        advance_loading_spinner(&mut app);
        let current_stage = stage_kind(&app.stage);
        if current_stage != previous_stage {
            terminal.clear()?;
            previous_stage = current_stage;
        }
        terminal.draw(|frame| render_plan_session(frame, &app))?;

        if event::poll(Duration::from_millis(if app.pending.is_some() {
            120
        } else {
            250
        }))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.code == KeyCode::Esc {
                        return Ok(InteractivePlanExit::Cancelled);
                    }

                    if app.pending.is_some() {
                        continue;
                    }

                    let frame_size = terminal.size()?;
                    let action = match &mut app.stage {
                        PlanStage::Request(request_app) => handle_request_step_key_with_viewport(
                            request_app,
                            key,
                            request_input_viewport(frame_size.into()),
                        ),
                        PlanStage::Questions(questions_app) => {
                            handle_questions_step_key_with_viewport(
                                questions_app,
                                key,
                                questions_answer_input_viewport(frame_size.into()),
                            )
                        }
                        PlanStage::Review(review_app) => handle_review_step_key(
                            review_app,
                            key,
                            review_layout(frame_size.into()),
                        ),
                        PlanStage::Loading(_) => SessionAction::None,
                    };

                    match action {
                        SessionAction::None => {}
                        SessionAction::GenerateQuestions {
                            request,
                            request_attachments,
                        } => {
                            start_question_generation(
                                &mut app,
                                root,
                                request,
                                request_attachments,
                                follow_up_question_limit,
                            );
                        }
                        SessionAction::GeneratePlan {
                            request,
                            request_attachments,
                            follow_ups,
                        } => {
                            start_plan_generation(
                                &mut app,
                                root,
                                request,
                                request_attachments,
                                follow_ups,
                                1,
                            );
                        }
                        SessionAction::RegeneratePlan { review } => {
                            start_plan_revision(&mut app, root, review);
                        }
                        SessionAction::Confirm(plan) => {
                            if plan.issues.is_empty() {
                                match &mut app.stage {
                                    PlanStage::Request(request_app) => {
                                        request_app.error = Some(
                                            "planning agent returned no issues to create"
                                                .to_string(),
                                        );
                                    }
                                    PlanStage::Questions(questions_app) => {
                                        questions_app.error = Some(
                                            "planning agent returned no issues to create"
                                                .to_string(),
                                        );
                                    }
                                    PlanStage::Review(review_app) => {
                                        review_app.error = Some(
                                            "planning agent returned no issues to create"
                                                .to_string(),
                                        );
                                    }
                                    PlanStage::Loading(_) => {}
                                }
                            } else {
                                return Ok(InteractivePlanExit::Confirmed(plan));
                            }
                        }
                    }
                }
                Event::Paste(text) => match &mut app.stage {
                    PlanStage::Request(request_app) => {
                        handle_request_step_paste(request_app, &text)
                    }
                    PlanStage::Questions(questions_app) => {
                        handle_questions_step_paste(questions_app, &text);
                    }
                    PlanStage::Review(_) | PlanStage::Loading(_) => {}
                },
                Event::Mouse(mouse) => {
                    if app.pending.is_some() {
                        continue;
                    }
                    let frame_size = terminal.size()?;
                    match &mut app.stage {
                        PlanStage::Request(request_app) => {
                            let viewport = request_input_viewport(frame_size.into());
                            let _ = handle_request_step_mouse(request_app, mouse, viewport);
                        }
                        PlanStage::Questions(questions_app) => {
                            if let Some(question) =
                                questions_app.questions.get_mut(questions_app.selected)
                            {
                                let viewport = questions_answer_input_viewport(frame_size.into());
                                let _ = handle_questions_step_mouse(question, mouse, viewport);
                            }
                        }
                        PlanStage::Review(review_app) => {
                            let _ = handle_review_step_mouse(
                                review_app,
                                mouse,
                                review_layout(frame_size.into()),
                            );
                        }
                        PlanStage::Loading(_) => {}
                    }
                }
                _ => {}
            }
        }
    }
}

fn build_questions_app(
    request: String,
    request_attachments: Vec<PromptImageAttachment>,
    questions: Vec<String>,
) -> QuestionsApp {
    QuestionsApp {
        request,
        request_attachments,
        questions: questions
            .into_iter()
            .map(|question| QuestionAnswer {
                question,
                answer: InputFieldState::multiline_with_prompt_attachments(String::new()),
                state: FollowUpAnswerState::Pending,
            })
            .collect(),
        selected: 0,
        error: None,
    }
}

fn build_review_app(
    request: String,
    request_attachments: Vec<PromptImageAttachment>,
    follow_ups: Vec<FollowUpResponse>,
    plan: PlannedIssueSet,
    revision: usize,
) -> ReviewApp {
    let decision_len = plan.issues.len();
    ReviewApp {
        request,
        request_attachments,
        follow_ups,
        plan,
        selected: 0,
        decisions: vec![0; decision_len],
        revision,
        focus: ReviewFocus::Tickets,
        overview_scroll: ScrollState::default(),
        selected_ticket_scroll: ScrollState::default(),
        combination_scroll: ScrollState::default(),
        error: None,
    }
}

impl ReviewFocus {
    fn next(self) -> Self {
        match self {
            Self::Tickets => Self::SelectedTicket,
            Self::SelectedTicket => Self::Overview,
            Self::Overview => Self::CombinationPlan,
            Self::CombinationPlan => Self::Tickets,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Tickets => Self::CombinationPlan,
            Self::SelectedTicket => Self::Tickets,
            Self::Overview => Self::SelectedTicket,
            Self::CombinationPlan => Self::Overview,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ReviewLayout {
    issue_list: Rect,
    selected_ticket: Rect,
    overview: Rect,
    combination_plan: Rect,
}

impl ReviewApp {
    fn selected_issue(&self) -> &PlannedIssueDraft {
        &self.plan.issues[self.selected]
    }

    fn overview_text(&self) -> Text<'static> {
        let decisions = review_decision_counts(self);
        let answered_follow_ups = self
            .follow_ups
            .iter()
            .filter(|follow_up| !follow_up.skipped)
            .count();
        let skipped_follow_ups = self
            .follow_ups
            .iter()
            .filter(|follow_up| follow_up.skipped)
            .count();
        Text::from(vec![
            Line::from("Original request"),
            Line::from(""),
            Line::from(self.request.clone()),
            Line::from(""),
            Line::from(format!(
                "Follow-ups: {} answered, {} skipped",
                answered_follow_ups, skipped_follow_ups
            )),
            Line::from(format!("Draft batch: {}", self.revision)),
            Line::from(format!(
                "Selected: {}/{}",
                decisions.selected_count,
                self.plan.issues.len()
            )),
            Line::from(format!("Skipped: {}", decisions.skipped_count)),
            Line::from(format!("Keeping as-is: {}", decisions.keep_count)),
            Line::from(format!("Merge groups: {}", decisions.group_count)),
            Line::from(""),
            Line::from("Plan Summary"),
            Line::from(""),
            Line::from(self.plan.summary.clone()),
        ])
    }

    fn selected_ticket_text(&self) -> Text<'static> {
        let selected = self.selected_issue();
        let mut detail_lines = vec![
            Line::from(format!("Title: {}", selected.title)),
            Line::from(format!(
                "Priority: {}",
                selected
                    .priority
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unset".to_string())
            )),
            Line::from(""),
            Line::from(selected.description.clone()),
        ];
        if !selected.acceptance_criteria.is_empty() {
            detail_lines.push(Line::from(""));
            detail_lines.push(Line::from("Acceptance Criteria"));
            detail_lines.push(Line::from(""));
            detail_lines.extend(
                selected
                    .acceptance_criteria
                    .iter()
                    .map(|criterion| Line::from(format!("- {criterion}"))),
            );
        }
        Text::from(detail_lines)
    }

    fn combination_plan_text(&self) -> Text<'static> {
        let decisions = review_decision_counts(self);
        let mut merge_lines = vec![
            Line::from("Space cycles the active ticket through review states."),
            Line::from(""),
            Line::from("[ ] Skip the ticket"),
            Line::from("[x] Keep the ticket as-is"),
            Line::from("[1], [2], ... Merge every ticket sharing that number"),
            Line::from(""),
            Line::from(format!(
                "Active ticket state: {}",
                review_marker(
                    self.decisions
                        .get(self.selected)
                        .copied()
                        .unwrap_or_default()
                )
            )),
            Line::from(""),
        ];
        if decisions.selected_count == 0 {
            merge_lines.push(Line::from(
                "Select at least one ticket to keep or merge. Leave [ ] on tickets you want to skip.",
            ));
        } else if decisions.group_count == 0 {
            merge_lines.push(Line::from(
                "Press Enter to create the checked [x] tickets in Linear. Unchecked [ ] tickets will be skipped.",
            ));
        } else {
            merge_lines.push(Line::from(
                "Press Enter to rebuild the next preview from the checked [x] tickets and these merge groups. Unchecked [ ] tickets will be skipped:",
            ));
            merge_lines.push(Line::from(""));
            merge_lines.extend(render_merge_group_lines(self));
        }
        Text::from(merge_lines)
    }

    fn overview_rows(&self, width: u16) -> usize {
        wrapped_rows(&plain_text(&self.overview_text()), width.max(1))
    }

    fn selected_ticket_rows(&self, width: u16) -> usize {
        wrapped_rows(&plain_text(&self.selected_ticket_text()), width.max(1))
    }

    fn combination_plan_rows(&self, width: u16) -> usize {
        wrapped_rows(&plain_text(&self.combination_plan_text()), width.max(1))
    }

    fn move_selection(&mut self, delta: isize) {
        let next = if delta.is_negative() {
            self.selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.selected
                .saturating_add(delta as usize)
                .min(self.plan.issues.len().saturating_sub(1))
        };
        if next != self.selected {
            self.selected = next;
            self.selected_ticket_scroll.reset();
        }
    }
}

fn review_layout(frame_area: Rect) -> ReviewLayout {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(4)])
        .split(frame_area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(layout[0]);
    let top_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(rows[0]);
    let bottom_row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(rows[1]);

    ReviewLayout {
        issue_list: top_row[0],
        selected_ticket: top_row[1],
        overview: bottom_row[0],
        combination_plan: bottom_row[1],
    }
}

enum SessionAction {
    None,
    GenerateQuestions {
        request: String,
        request_attachments: Vec<PromptImageAttachment>,
    },
    GeneratePlan {
        request: String,
        request_attachments: Vec<PromptImageAttachment>,
        follow_ups: Vec<FollowUpResponse>,
    },
    RegeneratePlan {
        review: ReviewApp,
    },
    Confirm(PlannedIssueSet),
}

#[cfg(test)]
fn handle_request_step_key(
    app: &mut RequestApp,
    key: crossterm::event::KeyEvent,
    input_width: u16,
) -> SessionAction {
    handle_request_step_key_with_viewport(app, key, Rect::new(0, 0, input_width.max(1), 8))
}

fn handle_request_step_key_with_viewport(
    app: &mut RequestApp,
    key: crossterm::event::KeyEvent,
    input_viewport: Rect,
) -> SessionAction {
    match key.code {
        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            match app.request.paste_clipboard_with_prompt_attachments() {
                Ok(_) => app.error = None,
                Err(error) => app.error = Some(error.to_string()),
            }
            SessionAction::None
        }
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let request_value = app.request.display_value();
            let request = request_value.trim();
            if request.is_empty() {
                app.error = Some("Enter a planning request before continuing.".to_string());
                SessionAction::None
            } else {
                app.error = None;
                SessionAction::GenerateQuestions {
                    request: request.to_string(),
                    request_attachments: app.request.prompt_attachments().to_vec(),
                }
            }
        }
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                if app.request.insert_newline() {
                    app.error = None;
                }
                SessionAction::None
            } else {
                let request_value = app.request.display_value();
                let request = request_value.trim();
                if request.is_empty() {
                    app.error = Some("Enter a planning request before continuing.".to_string());
                    SessionAction::None
                } else {
                    app.error = None;
                    SessionAction::GenerateQuestions {
                        request: request.to_string(),
                        request_attachments: app.request.prompt_attachments().to_vec(),
                    }
                }
            }
        }
        _ => {
            if app.request.handle_key_with_viewport(
                key,
                input_viewport.width,
                input_viewport.height,
            ) {
                app.error = None;
            }
            SessionAction::None
        }
    }
}

fn handle_request_step_paste(app: &mut RequestApp, text: &str) {
    match app.request.paste_with_prompt_attachments(text) {
        Ok(_) => app.error = None,
        Err(error) => app.error = Some(error.to_string()),
    }
}

fn handle_request_step_mouse(
    app: &mut RequestApp,
    mouse: MouseEvent,
    input_viewport: Rect,
) -> bool {
    if !matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        return false;
    }

    app.request.handle_mouse_scroll(
        mouse,
        input_viewport,
        input_viewport.width,
        input_viewport.height,
    )
}

#[cfg(test)]
fn handle_questions_step_key(
    app: &mut QuestionsApp,
    key: crossterm::event::KeyEvent,
    input_width: u16,
) -> SessionAction {
    handle_questions_step_key_with_viewport(app, key, Rect::new(0, 0, input_width.max(1), 8))
}

fn handle_questions_step_key_with_viewport(
    app: &mut QuestionsApp,
    key: crossterm::event::KeyEvent,
    input_viewport: Rect,
) -> SessionAction {
    match key.code {
        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(question) = app.questions.get_mut(app.selected) {
                match question.answer.paste_clipboard_with_prompt_attachments() {
                    Ok(_) => {
                        question.state = FollowUpAnswerState::Pending;
                        app.error = None;
                    }
                    Err(error) => app.error = Some(error.to_string()),
                }
            }
            SessionAction::None
        }
        KeyCode::BackTab => {
            if app.selected == 0 {
                app.selected = app.questions.len().saturating_sub(1);
            } else {
                app.selected -= 1;
            }
            app.error = None;
            SessionAction::None
        }
        KeyCode::Tab => {
            app.selected = (app.selected + 1) % app.questions.len();
            app.error = None;
            SessionAction::None
        }
        KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let Some(selected) = app.questions.get_mut(app.selected) else {
                return SessionAction::None;
            };
            selected.state = if selected.answer.display_value().trim().is_empty() {
                FollowUpAnswerState::Skipped
            } else {
                FollowUpAnswerState::Answered
            };

            if app.questions.iter().all(question_is_completed) {
                app.error = None;
                return SessionAction::GeneratePlan {
                    request: app.request.clone(),
                    request_attachments: app.request_attachments.clone(),
                    follow_ups: collect_follow_up_responses(&app.questions),
                };
            }

            if let Some(index) = next_incomplete_question(&app.questions, app.selected) {
                app.selected = index;
            }
            app.error = None;
            SessionAction::None
        }
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                if let Some(question) = app.questions.get_mut(app.selected)
                    && question.answer.insert_newline()
                {
                    question.state = FollowUpAnswerState::Pending;
                    app.error = None;
                }
                SessionAction::None
            } else {
                let Some(selected) = app.questions.get_mut(app.selected) else {
                    return SessionAction::None;
                };
                selected.state = if selected.answer.display_value().trim().is_empty() {
                    FollowUpAnswerState::Skipped
                } else {
                    FollowUpAnswerState::Answered
                };

                if app.questions.iter().all(question_is_completed) {
                    app.error = None;
                    SessionAction::GeneratePlan {
                        request: app.request.clone(),
                        request_attachments: app.request_attachments.clone(),
                        follow_ups: collect_follow_up_responses(&app.questions),
                    }
                } else {
                    if let Some(index) = next_incomplete_question(&app.questions, app.selected) {
                        app.selected = index;
                    }
                    app.error = None;
                    SessionAction::None
                }
            }
        }
        _ => {
            if let Some(question) = app.questions.get_mut(app.selected)
                && question.answer.handle_key_with_viewport(
                    key,
                    input_viewport.width,
                    input_viewport.height,
                )
            {
                question.state = FollowUpAnswerState::Pending;
                app.error = None;
            }
            SessionAction::None
        }
    }
}

fn handle_questions_step_paste(app: &mut QuestionsApp, text: &str) {
    if let Some(question) = app.questions.get_mut(app.selected) {
        match question.answer.paste_with_prompt_attachments(text) {
            Ok(_) => {
                question.state = FollowUpAnswerState::Pending;
                app.error = None;
            }
            Err(error) => app.error = Some(error.to_string()),
        }
    }
}

fn handle_questions_step_mouse(
    question: &mut QuestionAnswer,
    mouse: MouseEvent,
    input_viewport: Rect,
) -> bool {
    if !matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        return false;
    }

    question.answer.handle_mouse_scroll(
        mouse,
        input_viewport,
        input_viewport.width,
        input_viewport.height,
    )
}

fn handle_review_step_key(
    app: &mut ReviewApp,
    key: crossterm::event::KeyEvent,
    layout: ReviewLayout,
) -> SessionAction {
    match key.code {
        KeyCode::BackTab => {
            app.focus = app.focus.previous();
            app.error = None;
            SessionAction::None
        }
        KeyCode::Tab => {
            app.focus = app.focus.next();
            app.error = None;
            SessionAction::None
        }
        KeyCode::Up => {
            if app.focus == ReviewFocus::Tickets {
                app.move_selection(-1);
            } else {
                let _ = handle_review_scroll_key(app, key, layout);
            }
            app.error = None;
            SessionAction::None
        }
        KeyCode::Down => {
            if app.focus == ReviewFocus::Tickets {
                app.move_selection(1);
            } else {
                let _ = handle_review_scroll_key(app, key, layout);
            }
            app.error = None;
            SessionAction::None
        }
        KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End => {
            let _ = handle_review_scroll_key(app, key, layout);
            app.error = None;
            SessionAction::None
        }
        KeyCode::Char(' ') => {
            cycle_review_decision(app);
            app.error = None;
            SessionAction::None
        }
        KeyCode::Char('u') => {
            for decision in &mut app.decisions {
                *decision = 0;
            }
            app.error = None;
            SessionAction::None
        }
        KeyCode::Enter => match review_submission_action(app) {
            Ok(ReviewSubmissionAction::ConfirmAsIs) => {
                SessionAction::Confirm(selected_issue_plan(app))
            }
            Ok(ReviewSubmissionAction::RegeneratePreview) => {
                app.error = None;
                SessionAction::RegeneratePlan {
                    review: app.clone(),
                }
            }
            Err(error) => {
                app.error = Some(error);
                SessionAction::None
            }
        },
        _ => SessionAction::None,
    }
}

fn handle_review_scroll_key(
    app: &mut ReviewApp,
    key: crossterm::event::KeyEvent,
    layout: ReviewLayout,
) -> bool {
    match app.focus {
        ReviewFocus::Tickets => false,
        ReviewFocus::SelectedTicket => app.selected_ticket_scroll.apply_key_in_viewport(
            key,
            layout.selected_ticket,
            app.selected_ticket_rows(layout.selected_ticket.width.saturating_sub(2)),
        ),
        ReviewFocus::Overview => app.overview_scroll.apply_key_in_viewport(
            key,
            layout.overview,
            app.overview_rows(layout.overview.width.saturating_sub(2)),
        ),
        ReviewFocus::CombinationPlan => app.combination_scroll.apply_key_in_viewport(
            key,
            layout.combination_plan,
            app.combination_plan_rows(layout.combination_plan.width.saturating_sub(2)),
        ),
    }
}

fn handle_review_step_mouse(app: &mut ReviewApp, mouse: MouseEvent, layout: ReviewLayout) -> bool {
    if !matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        return false;
    }

    match app.focus {
        ReviewFocus::Tickets => false,
        ReviewFocus::SelectedTicket => app.selected_ticket_scroll.apply_mouse_in_viewport(
            mouse,
            layout.selected_ticket,
            app.selected_ticket_rows(layout.selected_ticket.width.saturating_sub(2)),
        ),
        ReviewFocus::Overview => app.overview_scroll.apply_mouse_in_viewport(
            mouse,
            layout.overview,
            app.overview_rows(layout.overview.width.saturating_sub(2)),
        ),
        ReviewFocus::CombinationPlan => app.combination_scroll.apply_mouse_in_viewport(
            mouse,
            layout.combination_plan,
            app.combination_plan_rows(layout.combination_plan.width.saturating_sub(2)),
        ),
    }
}

fn cycle_review_decision(app: &mut ReviewApp) {
    if app.plan.issues.is_empty() {
        return;
    }

    let max_state = app.plan.issues.len() + 1;
    if let Some(decision) = app.decisions.get_mut(app.selected) {
        *decision = (*decision + 1) % (max_state + 1);
    }
}

enum ReviewSubmissionAction {
    ConfirmAsIs,
    RegeneratePreview,
}

fn review_submission_action(app: &ReviewApp) -> Result<ReviewSubmissionAction, String> {
    if app.decisions.iter().all(|decision| *decision == 0) {
        return Err(
            "Select at least one suggested ticket before continuing. Leave [ ] on any ticket you want to skip, use [x] to keep it, or assign a number to merge it."
                .to_string(),
        );
    }

    let merge_groups = review_merge_groups(app);
    for (group, indices) in &merge_groups {
        if indices.len() < 2 {
            return Err(format!(
                "Merge group {group} only has one ticket. Mark it as [x] or assign another ticket to [{group}]."
            ));
        }
    }

    if merge_groups.is_empty() {
        Ok(ReviewSubmissionAction::ConfirmAsIs)
    } else {
        Ok(ReviewSubmissionAction::RegeneratePreview)
    }
}

fn selected_issue_plan(app: &ReviewApp) -> PlannedIssueSet {
    PlannedIssueSet {
        summary: app.plan.summary.clone(),
        issues: review_kept_indices(app)
            .into_iter()
            .filter_map(|index| app.plan.issues.get(index).cloned())
            .collect(),
    }
}

fn question_is_completed(question: &QuestionAnswer) -> bool {
    question.state != FollowUpAnswerState::Pending
}

fn question_progress_marker(question: &QuestionAnswer) -> &'static str {
    match question.state {
        FollowUpAnswerState::Pending => "[ ]",
        FollowUpAnswerState::Answered => "[x]",
        FollowUpAnswerState::Skipped => "[-]",
    }
}

fn collect_follow_up_responses(questions: &[QuestionAnswer]) -> Vec<FollowUpResponse> {
    questions
        .iter()
        .map(|question| FollowUpResponse {
            question: question.question.clone(),
            answer: question.answer.display_value().trim().to_string(),
            skipped: question.state == FollowUpAnswerState::Skipped,
            attachments: question.answer.prompt_attachments().to_vec(),
        })
        .collect()
}

fn next_incomplete_question(questions: &[QuestionAnswer], selected: usize) -> Option<usize> {
    (selected + 1..questions.len())
        .chain(0..selected)
        .find(|index| !question_is_completed(&questions[*index]))
}

fn render_plan_session(frame: &mut Frame<'_>, app: &PlanSessionApp) {
    frame.render_widget(Clear, frame.area());
    match &app.stage {
        PlanStage::Request(request_app) => render_request_form_frame(frame, request_app),
        PlanStage::Questions(questions_app) => render_questions_form_frame(frame, questions_app),
        PlanStage::Review(review_app) => render_review_form_frame(frame, review_app),
        PlanStage::Loading(loading_app) => render_loading_frame(frame, loading_app),
    }
}

fn render_request_form_frame(frame: &mut Frame<'_>, app: &RequestApp) {
    let layout = base_layout(frame);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(layout[0]);

    let request_block = Block::default()
        .borders(Borders::ALL)
        .title("Planning Request [editing]")
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let request_inner = request_block.inner(body[0]);
    let rendered = app.request.render_with_viewport(
        "Describe the feature or workflow you want to plan...",
        true,
        request_inner.width,
        request_inner.height,
    );
    let request = rendered.paragraph(request_block);
    frame.render_widget(request, body[0]);
    rendered.set_cursor(frame, request_inner);

    let summary = Paragraph::new(Text::from(vec![
        Line::from("This workflow stays in one dashboard while it:"),
        Line::from(""),
        Line::from("1. Captures the planning request"),
        Line::from("2. Asks targeted follow-up questions"),
        Line::from("3. Reviews the generated backlog tickets"),
        Line::from("4. Creates one or more Linear issues in Backlog"),
        Line::from(""),
        Line::from("Tip: keep the request concrete enough to describe the user or team need."),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Flow"))
    .wrap(Wrap { trim: false });
    frame.render_widget(summary, body[1]);

    render_footer(
        frame,
        layout[1],
        app.error.as_deref(),
        "Type the planning request. Up/Down and PgUp/PgDn/Home/End move through wrapped lines, and the mouse wheel scrolls while the editor is focused. Enter continues. Shift+Enter inserts a newline. Ctrl+S also continues. Ctrl+V checks for clipboard images first, otherwise pastes text. Attached images render as [Image #N] placeholders. Esc cancels.",
    );
}

fn render_questions_form_frame(frame: &mut Frame<'_>, app: &QuestionsApp) {
    let layout = base_layout(frame);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(layout[0]);
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(42), Constraint::Min(0)])
        .split(body[0]);
    let sidebar = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(38), Constraint::Min(0)])
        .split(body[1]);

    let selected = &app.questions[app.selected];
    let question = Paragraph::new(Text::from(vec![
        Line::from(selected.question.clone()),
        Line::from(""),
        Line::styled(
            "Enter records the current answer. Up/Down and PgUp/PgDn/Home/End move through wrapped content, and the mouse wheel scrolls while the answer pane is focused. Shift+Enter inserts a newline. Ctrl+S also moves to the next unanswered question, or generates the ticket plan once every answer is complete. Ctrl+V checks for clipboard images first, otherwise pastes text.",
            Style::default().add_modifier(Modifier::DIM),
        ),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!(
                "Question {} of {} [active]",
                app.selected + 1,
                app.questions.len()
            ))
            .border_style(Style::default().add_modifier(Modifier::BOLD)),
    )
    .wrap(Wrap { trim: false });
    frame.render_widget(question, main[0]);

    let answer_block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            "Answer {} of {} [editing]",
            app.selected + 1,
            app.questions.len()
        ))
        .border_style(Style::default().add_modifier(Modifier::BOLD));
    let answer_inner = answer_block.inner(main[1]);
    let rendered = selected.answer.render_with_viewport(
        "Type your answer for the active question...",
        true,
        answer_inner.width,
        answer_inner.height,
    );
    let answer = rendered.paragraph(answer_block);
    frame.render_widget(answer, main[1]);
    rendered.set_cursor(frame, answer_inner);

    let mut summary_lines = vec![
        Line::from("Original request"),
        Line::from(""),
        Line::from(app.request.clone()),
    ];
    summary_lines.push(Line::from(""));
    summary_lines.push(Line::from(format!(
        "Answered: {}/{}",
        app.questions
            .iter()
            .filter(|question| question.state == FollowUpAnswerState::Answered)
            .count(),
        app.questions.len()
    )));
    summary_lines.push(Line::from(format!(
        "Skipped: {}/{}",
        app.questions
            .iter()
            .filter(|question| question.state == FollowUpAnswerState::Skipped)
            .count(),
        app.questions.len()
    )));
    summary_lines.push(Line::from(""));
    summary_lines.push(Line::from(format!(
        "Current question: {} of {}",
        app.selected + 1,
        app.questions.len()
    )));
    let summary = Paragraph::new(Text::from(summary_lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Request Summary"),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(summary, sidebar[0]);

    let progress_lines = app
        .questions
        .iter()
        .enumerate()
        .flat_map(|(index, question)| {
            let marker = if index == app.selected { ">" } else { " " };
            [
                Line::from(format!(
                    "{marker} {} {}. {}",
                    question_progress_marker(question),
                    index + 1,
                    question.question
                )),
                Line::from(""),
            ]
        })
        .collect::<Vec<_>>();
    let progress = Paragraph::new(Text::from(progress_lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Question Progress"),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(progress, sidebar[1]);

    render_footer(
        frame,
        layout[1],
        app.error.as_deref(),
        "Tab/Shift-Tab moves between questions. Up/Down moves inside the active multiline answer, including wrapped lines. Enter records the current response; a blank answer skips that question. Shift+Enter inserts a newline. Once every question is answered or skipped, Enter generates the ticket plan. Ctrl+S remains available as an alternate submit key. Ctrl+V checks for clipboard images first, otherwise pastes text. Attached images render as [Image #N] placeholders. Esc cancels.",
    );
}

fn render_review_form_frame(frame: &mut Frame<'_>, app: &ReviewApp) {
    let layout = base_layout(frame);
    let review_layout = review_layout(frame.area());

    let mut issue_state = ListState::default();
    issue_state.select(Some(
        app.selected.min(app.plan.issues.len().saturating_sub(1)),
    ));
    let issue_items = app
        .plan
        .issues
        .iter()
        .enumerate()
        .map(|(index, issue)| {
            let marker = review_marker(app.decisions.get(index).copied().unwrap_or_default());
            ListItem::new(format!("{marker} {}", issue.title))
        })
        .collect::<Vec<_>>();
    let issue_list = List::new(issue_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    "Suggested Tickets ({}){}",
                    app.plan.issues.len(),
                    if app.focus == ReviewFocus::Tickets {
                        " [active]"
                    } else {
                        ""
                    }
                ))
                .border_style(if app.focus == ReviewFocus::Tickets {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                }),
        )
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    frame.render_stateful_widget(issue_list, review_layout.issue_list, &mut issue_state);

    let detail = scrollable_paragraph(
        app.selected_ticket_text(),
        if app.focus == ReviewFocus::SelectedTicket {
            "Selected Ticket [scroll]"
        } else {
            "Selected Ticket"
        },
        &app.selected_ticket_scroll,
    )
    .wrap(Wrap { trim: false })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(if app.focus == ReviewFocus::SelectedTicket {
                "Selected Ticket [scroll]"
            } else {
                "Selected Ticket"
            })
            .border_style(if app.focus == ReviewFocus::SelectedTicket {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            }),
    )
    .scroll((app.selected_ticket_scroll.offset(), 0));
    frame.render_widget(detail, review_layout.selected_ticket);

    let overview = scrollable_paragraph(
        app.overview_text(),
        if app.focus == ReviewFocus::Overview {
            "Overview [scroll]"
        } else {
            "Overview"
        },
        &app.overview_scroll,
    )
    .wrap(Wrap { trim: false })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(if app.focus == ReviewFocus::Overview {
                "Overview [scroll]"
            } else {
                "Overview"
            })
            .border_style(if app.focus == ReviewFocus::Overview {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            }),
    )
    .scroll((app.overview_scroll.offset(), 0));
    frame.render_widget(overview, review_layout.overview);

    let merge = scrollable_paragraph(
        app.combination_plan_text(),
        if app.focus == ReviewFocus::CombinationPlan {
            "Combination Plan [scroll]"
        } else {
            "Combination Plan"
        },
        &app.combination_scroll,
    )
    .wrap(Wrap { trim: false })
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(if app.focus == ReviewFocus::CombinationPlan {
                "Combination Plan [scroll]"
            } else {
                "Combination Plan"
            })
            .border_style(if app.focus == ReviewFocus::CombinationPlan {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            }),
    )
    .scroll((app.combination_scroll.offset(), 0));
    frame.render_widget(merge, review_layout.combination_plan);

    render_footer(
        frame,
        layout[1],
        app.error.as_deref(),
        "Tab/Shift+Tab changes review focus. In [scroll] panes, Up/Down and PgUp/PgDn/Home/End or the mouse wheel scroll. Space cycles [ ] skip -> [x] keep -> [1] -> [2] for the active ticket. Enter creates the checked batch or rebuilds the next preview when numbered merge groups are present. U clears all marks. Esc cancels.",
    );
}

fn base_layout(frame: &mut Frame<'_>) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(4)])
        .split(frame.area())
        .to_vec()
}

fn stage_kind(stage: &PlanStage) -> PlanStageKind {
    match stage {
        PlanStage::Request(_) => PlanStageKind::Request,
        PlanStage::Questions(_) => PlanStageKind::Questions,
        PlanStage::Review(_) => PlanStageKind::Review,
        PlanStage::Loading(_) => PlanStageKind::Loading,
    }
}

fn request_input_viewport(area: Rect) -> Rect {
    let layout = base_layout_for_area(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(layout[0]);
    inner_rect(body[0])
}

fn questions_answer_input_viewport(area: Rect) -> Rect {
    let layout = base_layout_for_area(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(layout[0]);
    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(42), Constraint::Min(0)])
        .split(body[0]);
    inner_rect(main[1])
}

fn base_layout_for_area(area: Rect) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(4)])
        .split(area)
        .to_vec()
}

fn inner_rect(area: Rect) -> Rect {
    Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2).max(1),
        area.height.saturating_sub(2).max(1),
    )
}

fn render_loading_frame(frame: &mut Frame<'_>, app: &LoadingApp) {
    render_loading_panel(
        frame,
        frame.area(),
        &LoadingPanelData {
            title: "Agent Working [loading]".to_string(),
            message: app.message.clone(),
            detail: app.detail.clone(),
            spinner_index: app.spinner_index,
            status_line:
                "State: loading. The dashboard advances automatically when the agent responds."
                    .to_string(),
        },
    );
}

fn render_footer(frame: &mut Frame<'_>, area: Rect, error: Option<&str>, help: &str) {
    let mut lines = vec![Line::from(help.to_string())];
    if let Some(error) = error {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("Error: {error}"),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    let footer = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL).title("Controls"))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, area);
}

fn review_marker(decision: usize) -> String {
    match decision {
        0 => "[ ]".to_string(),
        1 => "[x]".to_string(),
        value => format!("[{}]", value - 1),
    }
}

struct ReviewDecisionCounts {
    selected_count: usize,
    skipped_count: usize,
    keep_count: usize,
    group_count: usize,
}

fn review_decision_counts(app: &ReviewApp) -> ReviewDecisionCounts {
    let groups = review_merge_groups(app);
    ReviewDecisionCounts {
        selected_count: app
            .decisions
            .iter()
            .filter(|decision| **decision > 0)
            .count(),
        skipped_count: app
            .decisions
            .iter()
            .filter(|decision| **decision == 0)
            .count(),
        keep_count: app
            .decisions
            .iter()
            .filter(|decision| **decision == 1)
            .count(),
        group_count: groups.len(),
    }
}

fn review_merge_groups(app: &ReviewApp) -> BTreeMap<usize, Vec<usize>> {
    let mut groups = BTreeMap::new();
    for (index, decision) in app.decisions.iter().copied().enumerate() {
        if decision >= 2 {
            groups
                .entry(decision - 1)
                .or_insert_with(Vec::new)
                .push(index);
        }
    }
    groups
}

fn review_kept_indices(app: &ReviewApp) -> Vec<usize> {
    app.decisions
        .iter()
        .enumerate()
        .filter_map(|(index, decision)| (*decision == 1).then_some(index))
        .collect()
}

fn render_merge_group_lines(app: &ReviewApp) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (group, indices) in review_merge_groups(app) {
        let titles = indices
            .into_iter()
            .filter_map(|index| app.plan.issues.get(index).map(|issue| issue.title.clone()))
            .collect::<Vec<_>>()
            .join(" + ");
        lines.push(Line::from(format!("[{group}] {titles}")));
    }
    lines
}

fn advance_loading_spinner(app: &mut PlanSessionApp) {
    if let PlanStage::Loading(loading) = &mut app.stage {
        loading.spinner_index = (loading.spinner_index + 1) % SPINNER_FRAMES.len();
    }
}

fn process_pending_plan_job(
    app: &mut PlanSessionApp,
    root: &Path,
) -> Result<Option<InteractivePlanExit>> {
    let Some(pending) = app.pending.as_ref() else {
        return Ok(None);
    };

    match pending.receiver.try_recv() {
        Ok(result) => {
            let pending = app
                .pending
                .take()
                .ok_or_else(|| anyhow!("pending plan job disappeared unexpectedly"))?;
            app.continuation = result.continuation;
            match result.outcome {
                Ok(PlanWorkerOutcome::Questions {
                    request,
                    request_attachments,
                    questions,
                }) => {
                    if questions.is_empty() {
                        start_plan_generation(
                            app,
                            root,
                            request,
                            request_attachments,
                            Vec::new(),
                            1,
                        );
                    } else {
                        app.stage = PlanStage::Questions(build_questions_app(
                            request,
                            request_attachments,
                            questions,
                        ));
                    }
                }
                Ok(PlanWorkerOutcome::Review(review)) => {
                    app.stage = PlanStage::Review(review);
                }
                Err(error) => {
                    let mut previous_stage = pending.previous_stage;
                    set_stage_error(&mut previous_stage, error.to_string());
                    app.stage = previous_stage;
                }
            }
        }
        Err(TryRecvError::Empty) => {}
        Err(TryRecvError::Disconnected) => {
            let pending = app
                .pending
                .take()
                .ok_or_else(|| anyhow!("pending plan job disappeared unexpectedly"))?;
            let mut previous_stage = pending.previous_stage;
            set_stage_error(
                &mut previous_stage,
                "planning worker exited before returning a result".to_string(),
            );
            app.stage = previous_stage;
        }
    }

    Ok(None)
}

fn start_question_generation(
    app: &mut PlanSessionApp,
    root: &Path,
    request: String,
    request_attachments: Vec<PromptImageAttachment>,
    follow_up_question_limit: usize,
) {
    let previous_stage = app.stage.clone();
    app.stage = PlanStage::Loading(LoadingApp {
        message: "Generating follow-up questions".to_string(),
        detail: "Reviewing the request and deciding whether more context is needed.".to_string(),
        spinner_index: 0,
    });
    app.pending = Some(PendingPlanJob {
        receiver: spawn_questions_job(
            root.to_path_buf(),
            request,
            request_attachments,
            follow_up_question_limit,
            app.agent_overrides.clone(),
            app.continuation.clone(),
        ),
        previous_stage,
    });
}

fn start_plan_generation(
    app: &mut PlanSessionApp,
    root: &Path,
    request: String,
    request_attachments: Vec<PromptImageAttachment>,
    follow_ups: Vec<FollowUpResponse>,
    revision: usize,
) {
    let previous_stage = app.stage.clone();
    app.stage = PlanStage::Loading(LoadingApp {
        message: if revision == 1 {
            "Generating suggested tickets".to_string()
        } else {
            format!("Rebuilding suggested tickets (batch {revision})")
        },
        detail: "Drafting Linear-ready backlog tickets from the request and collected context."
            .to_string(),
        spinner_index: 0,
    });
    app.pending = Some(PendingPlanJob {
        receiver: spawn_plan_job(
            root.to_path_buf(),
            request,
            request_attachments,
            follow_ups,
            revision,
            app.agent_overrides.clone(),
            app.continuation.clone(),
        ),
        previous_stage,
    });
}

fn start_plan_revision(app: &mut PlanSessionApp, root: &Path, review: ReviewApp) {
    let previous_stage = app.stage.clone();
    let next_revision = review.revision + 1;
    let group_labels = review_merge_groups(&review)
        .keys()
        .map(|group| group.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    app.stage = PlanStage::Loading(LoadingApp {
        message: format!("Rebuilding preview into batch {next_revision}"),
        detail: format!("Combining merge groups {group_labels} into a new ticket draft."),
        spinner_index: 0,
    });
    app.pending = Some(PendingPlanJob {
        receiver: spawn_plan_revision_job(
            root.to_path_buf(),
            review,
            next_revision,
            app.agent_overrides.clone(),
            app.continuation.clone(),
        ),
        previous_stage,
    });
}

fn spawn_questions_job(
    root: PathBuf,
    request: String,
    request_attachments: Vec<PromptImageAttachment>,
    follow_up_question_limit: usize,
    agent_overrides: PlanningAgentOverrides,
    continuation: Option<AgentContinuation>,
) -> Receiver<PlanWorkerReport> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut continuation = continuation;
        let outcome = generate_follow_up_questions(
            &root,
            &request,
            request_attachments.clone(),
            follow_up_question_limit,
            &agent_overrides,
            &mut continuation,
        )
        .map(|questions| PlanWorkerOutcome::Questions {
            request,
            request_attachments,
            questions,
        });
        let _ = sender.send(PlanWorkerReport {
            continuation,
            outcome,
        });
    });
    receiver
}

fn spawn_plan_job(
    root: PathBuf,
    request: String,
    request_attachments: Vec<PromptImageAttachment>,
    follow_ups: Vec<FollowUpResponse>,
    revision: usize,
    agent_overrides: PlanningAgentOverrides,
    continuation: Option<AgentContinuation>,
) -> Receiver<PlanWorkerReport> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let attachments = collect_prompt_attachments(&request_attachments, &follow_ups);
        let mut continuation = continuation;
        let outcome = generate_issue_plan(
            &root,
            &request,
            &follow_ups,
            attachments,
            &agent_overrides,
            &mut continuation,
        )
        .and_then(|plan| {
            if plan.issues.is_empty() {
                bail!("planning agent returned no issues to create");
            }
            Ok(PlanWorkerOutcome::Review(build_review_app(
                request,
                request_attachments,
                follow_ups,
                plan,
                revision,
            )))
        });
        let _ = sender.send(PlanWorkerReport {
            continuation,
            outcome,
        });
    });
    receiver
}

fn spawn_plan_revision_job(
    root: PathBuf,
    review: ReviewApp,
    revision: usize,
    agent_overrides: PlanningAgentOverrides,
    continuation: Option<AgentContinuation>,
) -> Receiver<PlanWorkerReport> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut continuation = continuation;
        let outcome = revise_issue_plan(
            &root,
            &review.request,
            &review.request_attachments,
            &review.follow_ups,
            &review.plan,
            &review_kept_indices(&review),
            &review_merge_groups(&review),
            &agent_overrides,
            &mut continuation,
        )
        .and_then(|plan| {
            if plan.issues.is_empty() {
                bail!("planning agent returned no issues to create");
            }
            Ok(PlanWorkerOutcome::Review(build_review_app(
                review.request,
                review.request_attachments,
                review.follow_ups,
                plan,
                revision,
            )))
        });
        let _ = sender.send(PlanWorkerReport {
            continuation,
            outcome,
        });
    });
    receiver
}

fn set_stage_error(stage: &mut PlanStage, error: String) {
    match stage {
        PlanStage::Request(request_app) => request_app.error = Some(error),
        PlanStage::Questions(questions_app) => questions_app.error = Some(error),
        PlanStage::Review(review_app) => review_app.error = Some(error),
        PlanStage::Loading(_) => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn revise_issue_plan(
    root: &Path,
    request: &str,
    request_attachments: &[PromptImageAttachment],
    follow_ups: &[FollowUpResponse],
    plan: &PlannedIssueSet,
    kept_indices: &[usize],
    merge_groups: &BTreeMap<usize, Vec<usize>>,
    overrides: &PlanningAgentOverrides,
    continuation: &mut Option<AgentContinuation>,
) -> Result<PlannedIssueSet> {
    let prompt =
        render_issue_merge_prompt(root, request, follow_ups, plan, kept_indices, merge_groups)?;
    let output = run_agent_capture_with_continuation(
        &RunAgentArgs {
            root: Some(root.to_path_buf()),
            route_key: Some(AGENT_ROUTE_BACKLOG_PLAN.to_string()),
            agent: overrides.agent.clone(),
            prompt,
            instructions: None,
            model: overrides.model.clone(),
            reasoning: overrides.reasoning.clone(),
            transport: None,
            attachments: collect_prompt_attachments(request_attachments, follow_ups),
        },
        continuation,
    )?;
    let parsed: PlannedIssueSet = parse_agent_json(&output.stdout, "issue plan revision")?;

    Ok(PlannedIssueSet {
        summary: parsed.summary.trim().to_string(),
        issues: parsed
            .issues
            .into_iter()
            .map(|draft| PlannedIssueDraft {
                title: draft.title.trim().to_string(),
                description: draft.description.trim().to_string(),
                acceptance_criteria: draft
                    .acceptance_criteria
                    .into_iter()
                    .map(|criterion| criterion.trim().to_string())
                    .filter(|criterion| !criterion.is_empty())
                    .collect(),
                priority: draft.priority,
            })
            .filter(|draft| !draft.title.is_empty() && !draft.description.is_empty())
            .collect(),
    })
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
    }
}

#[cfg(test)]
fn snapshot(backend: &TestBackend) -> String {
    let buffer = backend.buffer();
    let mut lines = Vec::new();

    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_string());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::{
        FollowUpAnswerState, FollowUpQuestions, FollowUpResponse, LoadingApp, PendingPlanJob,
        PlanSessionApp, PlanStage, PlanWorkerOutcome, PlanWorkerReport, PlannedIssueDraft,
        PlannedIssueSet, PlanningAgentOverrides, QuestionAnswer, QuestionsApp, RequestApp,
        ReviewApp, ReviewFocus, ReviewSubmissionAction, SKIPPED_FOLLOW_UP_LABEL, SessionAction,
        build_review_app, handle_questions_step_key, handle_questions_step_key_with_viewport,
        handle_questions_step_mouse, handle_questions_step_paste, handle_request_step_key,
        handle_request_step_key_with_viewport, handle_request_step_mouse,
        handle_request_step_paste, handle_review_step_key, handle_review_step_mouse,
        next_incomplete_question, parse_agent_json, process_pending_plan_job,
        questions_answer_input_viewport, render_issue_merge_prompt, render_loading_frame,
        render_plan_session, render_question_prompt, render_questions_form_frame,
        render_request_form_frame, render_review_form_frame, request_input_viewport,
        review_kept_indices, review_layout, review_marker, review_merge_groups,
        review_submission_action, selected_issue_plan, snapshot,
    };
    use crate::config::DEFAULT_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT;
    use crate::tui::fields::InputFieldState;
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use std::sync::mpsc;
    use tempfile::tempdir;

    fn answered_question(question: &str, answer: &str) -> QuestionAnswer {
        QuestionAnswer {
            question: question.to_string(),
            answer: InputFieldState::multiline(answer),
            state: FollowUpAnswerState::Answered,
        }
    }

    fn pending_question(question: &str) -> QuestionAnswer {
        QuestionAnswer {
            question: question.to_string(),
            answer: InputFieldState::multiline_with_prompt_attachments(String::new()),
            state: FollowUpAnswerState::Pending,
        }
    }

    fn answered_follow_up(question: &str, answer: &str) -> FollowUpResponse {
        FollowUpResponse {
            question: question.to_string(),
            answer: answer.to_string(),
            skipped: false,
            attachments: Vec::new(),
        }
    }

    fn skipped_follow_up(question: &str) -> FollowUpResponse {
        FollowUpResponse {
            question: question.to_string(),
            answer: String::new(),
            skipped: true,
            attachments: Vec::new(),
        }
    }

    fn render_request_snapshot_with_size(app: &RequestApp, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_request_form_frame(frame, app))
            .expect("request form should render");
        snapshot(terminal.backend())
    }

    fn render_request_snapshot(app: &RequestApp) -> String {
        render_request_snapshot_with_size(app, 120, 32)
    }

    fn render_questions_snapshot_with_size(app: &QuestionsApp, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_questions_form_frame(frame, app))
            .expect("questions form should render");
        snapshot(terminal.backend())
    }

    fn render_questions_snapshot(app: &QuestionsApp) -> String {
        render_questions_snapshot_with_size(app, 140, 36)
    }

    fn render_review_snapshot(app: &ReviewApp) -> String {
        let backend = TestBackend::new(140, 36);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_review_form_frame(frame, app))
            .expect("review form should render");
        snapshot(terminal.backend())
    }

    fn render_loading_snapshot(app: &LoadingApp) -> String {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_loading_frame(frame, app))
            .expect("loading frame should render");
        snapshot(terminal.backend())
    }

    #[test]
    fn request_dashboard_snapshot_shows_planning_flow() {
        let snapshot = render_request_snapshot(&RequestApp {
            request: InputFieldState::multiline("Plan a dashboard for multi-ticket backlog work"),
            error: None,
        });

        assert!(snapshot.contains("Planning Request [editing]"));
        assert!(!snapshot.contains("1. Request"));
        assert!(snapshot.contains("stays in one dashboard"));
    }

    #[test]
    fn questions_dashboard_highlights_the_active_question_and_progress() {
        let snapshot = render_questions_snapshot(&QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![
                answered_question("Who uses the feature?", "CLI maintainers"),
                answered_question("Should it create one issue or many?", "Many if needed"),
            ],
            selected: 1,
            error: None,
        });

        assert!(snapshot.contains("Question 2 of 2 [active]"));
        assert!(snapshot.contains("Question Progress"));
        assert!(snapshot.contains("Answered: 2/2"));
        assert!(snapshot.contains("Many if needed"));
    }

    #[test]
    fn questions_dashboard_renders_more_than_three_follow_up_questions() {
        let snapshot = render_questions_snapshot(&QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![
                answered_question("Who uses it?", "CLI maintainers"),
                answered_question("What workflow changes?", "Interactive planning"),
                answered_question("What should stay unchanged?", "--no-interactive"),
                pending_question("How should it be validated?"),
            ],
            selected: 3,
            error: None,
        });

        assert!(snapshot.contains("Question 4 of 4 [active]"));
        assert!(snapshot.contains("Answered: 3/4"));
        assert!(snapshot.contains("Who uses it?"));
        assert!(snapshot.contains("How should it be validated?"));
    }

    #[test]
    fn request_step_paste_keeps_text_in_the_editor() {
        let mut app = RequestApp {
            request: InputFieldState::multiline("Plan:"),
            error: Some("stale".to_string()),
        };

        handle_request_step_paste(&mut app, " add dashboard flow\nand follow-up capture\n");

        assert_eq!(
            app.request.value(),
            "Plan: add dashboard flow\nand follow-up capture\n"
        );
        assert_eq!(app.error, None);
    }

    #[test]
    fn request_step_paste_accepts_image_paths_and_submit_preserves_attachments() {
        use image::{ImageBuffer, Rgba};

        let temp = tempdir().expect("temp dir");
        let image_path = temp.path().join("request.png");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([1, 2, 3, 255]))
            .save(&image_path)
            .expect("save image");

        let mut app = RequestApp {
            request: InputFieldState::multiline_with_prompt_attachments("Plan: "),
            error: Some("stale".to_string()),
        };

        handle_request_step_paste(&mut app, image_path.to_str().expect("utf8"));

        assert_eq!(app.request.display_value(), "Plan: [Image #1]");
        assert_eq!(app.request.prompt_attachments().len(), 1);
        assert_eq!(app.error, None);

        let action = handle_request_step_key(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter),
            80,
        );

        match action {
            SessionAction::GenerateQuestions {
                request,
                request_attachments,
            } => {
                assert_eq!(request, "Plan: [Image #1]");
                assert_eq!(request_attachments.len(), 1);
                assert_eq!(request_attachments[0].display_name, "request.png");
            }
            _ => panic!("expected enter to preserve request attachments"),
        }
    }

    #[test]
    fn request_step_shift_enter_adds_a_newline() {
        let mut app = RequestApp {
            request: InputFieldState::multiline("Plan:"),
            error: Some("stale".to_string()),
        };

        let action = handle_request_step_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::SHIFT,
            ),
            80,
        );

        assert!(matches!(action, SessionAction::None));
        assert_eq!(app.request.value(), "Plan:\n");
        assert_eq!(app.error, None);
    }

    #[test]
    fn request_step_enter_submits_when_text_is_present() {
        let mut app = RequestApp {
            request: InputFieldState::multiline("Plan a new command"),
            error: None,
        };

        let action = handle_request_step_key(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter),
            80,
        );

        match action {
            SessionAction::GenerateQuestions { request, .. } => {
                assert_eq!(request, "Plan a new command")
            }
            _ => panic!("expected enter to continue to question generation"),
        }
    }

    #[test]
    fn request_step_ctrl_s_submits_when_text_is_present() {
        let mut app = RequestApp {
            request: InputFieldState::multiline("Plan a new command"),
            error: None,
        };

        let action = handle_request_step_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('s'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
            80,
        );

        match action {
            SessionAction::GenerateQuestions { request, .. } => {
                assert_eq!(request, "Plan a new command")
            }
            _ => panic!("expected ctrl+s to continue to question generation"),
        }
    }

    #[test]
    fn request_editor_snapshot_scrolls_to_visible_bottom_rows() {
        let mut app = RequestApp {
            request: InputFieldState::multiline(
                (1..=20)
                    .map(|index| format!("REQ-{index:02}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            error: None,
        };

        let action = handle_request_step_key_with_viewport(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::End),
            request_input_viewport(Rect::new(0, 0, 120, 16)),
        );

        assert!(matches!(action, SessionAction::None));

        let snapshot = render_request_snapshot_with_size(&app, 120, 16);
        assert!(snapshot.contains("REQ-20"));
        assert!(!snapshot.contains("REQ-01"));
    }

    #[test]
    fn request_step_mouse_wheel_scrolls_the_request_editor() {
        let mut app = RequestApp {
            request: InputFieldState::multiline(
                (1..=20)
                    .map(|index| format!("REQ-{index:02}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            error: None,
        };
        let viewport = request_input_viewport(Rect::new(0, 0, 120, 16));
        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: viewport.x + 1,
            row: viewport.y + 1,
            modifiers: KeyModifiers::NONE,
        };

        assert!(handle_request_step_mouse(&mut app, mouse, viewport));
        assert!(
            app.request
                .render_with_viewport("", true, viewport.width, viewport.height)
                .scroll_offset
                > 0
        );
    }

    #[test]
    fn questions_step_paste_updates_only_the_active_answer() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![
                answered_question("Who uses the feature?", "CLI maintainers"),
                pending_question("Should it create one issue or many?"),
            ],
            selected: 1,
            error: Some("stale".to_string()),
        };

        handle_questions_step_paste(&mut app, "Many tickets\nif review requires it\n");

        assert_eq!(app.questions[0].answer.value(), "CLI maintainers");
        assert_eq!(
            app.questions[1].answer.value(),
            "Many tickets\nif review requires it\n"
        );
        assert_eq!(app.questions[1].state, FollowUpAnswerState::Pending);
        assert_eq!(app.error, None);
    }

    #[test]
    fn questions_step_paste_accepts_image_paths_and_generate_plan_preserves_order() {
        use image::{ImageBuffer, Rgba};

        let temp = tempdir().expect("temp dir");
        let request_image_path = temp.path().join("request.png");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([1, 2, 3, 255]))
            .save(&request_image_path)
            .expect("save request image");
        let request_attachment = crate::tui::prompt_images::resolve_attachment_from_pasted_text(
            request_image_path.to_str().expect("utf8"),
        )
        .expect("resolve request attachment")
        .expect("request attachment");

        let answer_image_path = temp.path().join("answer.png");
        ImageBuffer::<Rgba<u8>, Vec<u8>>::from_pixel(2, 2, Rgba([4, 5, 6, 255]))
            .save(&answer_image_path)
            .expect("save answer image");

        let mut app = QuestionsApp {
            request: "Plan a new command [Image #1]".to_string(),
            request_attachments: vec![request_attachment.clone()],
            questions: vec![pending_question("Attach the design reference?")],
            selected: 0,
            error: Some("stale".to_string()),
        };

        handle_questions_step_paste(&mut app, answer_image_path.to_str().expect("utf8"));

        assert_eq!(app.questions[0].answer.display_value(), "[Image #1]");
        assert_eq!(app.questions[0].answer.prompt_attachments().len(), 1);
        assert_eq!(app.questions[0].state, FollowUpAnswerState::Pending);
        assert_eq!(app.error, None);

        let action = handle_questions_step_key(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter),
            80,
        );

        match action {
            SessionAction::GeneratePlan {
                request,
                request_attachments,
                follow_ups,
            } => {
                assert_eq!(request, "Plan a new command [Image #1]");
                assert_eq!(request_attachments.len(), 1);
                assert_eq!(request_attachments[0].display_name, "request.png");
                assert_eq!(follow_ups.len(), 1);
                assert_eq!(follow_ups[0].answer, "[Image #1]");
                assert_eq!(follow_ups[0].attachments.len(), 1);
                assert_eq!(follow_ups[0].attachments[0].display_name, "answer.png");

                let combined = super::collect_prompt_attachments(&request_attachments, &follow_ups);
                assert_eq!(combined.len(), 2);
                assert_eq!(combined[0].display_name, "request.png");
                assert_eq!(combined[1].display_name, "answer.png");
            }
            _ => panic!("expected enter to preserve follow-up attachments"),
        }
    }

    #[test]
    fn questions_step_shift_enter_adds_a_newline_in_active_answer() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![pending_question("How should it be validated?")],
            selected: 0,
            error: Some("stale".to_string()),
        };

        let action = handle_questions_step_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::SHIFT,
            ),
            80,
        );

        assert!(matches!(action, SessionAction::None));
        assert_eq!(app.questions[0].answer.value(), "\n");
        assert_eq!(app.questions[0].state, FollowUpAnswerState::Pending);
        assert_eq!(app.error, None);
    }

    #[test]
    fn questions_step_up_down_moves_inside_active_answer_without_changing_selection() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![
                pending_question("Who owns it?"),
                answered_question("How should it be validated?", "12345\n12"),
            ],
            selected: 1,
            error: None,
        };

        let action = handle_questions_step_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Up,
                crossterm::event::KeyModifiers::NONE,
            ),
            4,
        );

        assert!(matches!(action, SessionAction::None));
        assert_eq!(app.selected, 1);
        assert_eq!(app.questions[1].answer.cursor(), 5);

        let action = handle_questions_step_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ),
            4,
        );

        assert!(matches!(action, SessionAction::None));
        assert_eq!(app.selected, 1);
        assert_eq!(
            app.questions[1].answer.cursor(),
            app.questions[1].answer.value().len()
        );
    }

    #[test]
    fn questions_step_page_down_advances_within_active_answer() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![QuestionAnswer {
                question: "How should it be validated?".to_string(),
                answer: InputFieldState::multiline(
                    (1..=20)
                        .map(|index| format!("line {index}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                state: FollowUpAnswerState::Pending,
            }],
            selected: 0,
            error: None,
        };
        let _ = app.questions[0]
            .answer
            .handle_key(crossterm::event::KeyEvent::from(
                crossterm::event::KeyCode::Home,
            ));

        let before = app.questions[0].answer.cursor();
        let action = handle_questions_step_key(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::PageDown),
            12,
        );

        assert!(matches!(action, SessionAction::None));
        assert_eq!(app.selected, 0);
        assert!(app.questions[0].answer.cursor() > before);
    }

    #[test]
    fn questions_editor_snapshot_scrolls_to_visible_bottom_rows() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![QuestionAnswer {
                question: "How should it be validated?".to_string(),
                answer: InputFieldState::multiline(
                    (1..=20)
                        .map(|index| format!("ANS-{index:02}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                state: FollowUpAnswerState::Pending,
            }],
            selected: 0,
            error: None,
        };

        let action = handle_questions_step_key_with_viewport(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::End),
            questions_answer_input_viewport(Rect::new(0, 0, 120, 18)),
        );

        assert!(matches!(action, SessionAction::None));

        let snapshot = render_questions_snapshot_with_size(&app, 120, 18);
        assert!(snapshot.contains("ANS-20"));
        assert!(!snapshot.contains("ANS-01"));
    }

    #[test]
    fn questions_step_mouse_wheel_scrolls_only_the_active_answer() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![
                answered_question("Who owns it?", "owner"),
                QuestionAnswer {
                    question: "How should it be validated?".to_string(),
                    answer: InputFieldState::multiline(
                        (1..=20)
                            .map(|index| format!("ANS-{index:02}"))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    state: FollowUpAnswerState::Pending,
                },
            ],
            selected: 1,
            error: None,
        };
        let viewport = questions_answer_input_viewport(Rect::new(0, 0, 120, 18));
        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: viewport.x + 1,
            row: viewport.y + 1,
            modifiers: KeyModifiers::NONE,
        };

        assert!(handle_questions_step_mouse(
            &mut app.questions[1],
            mouse,
            viewport
        ));
        assert!(
            app.questions[1]
                .answer
                .render_with_viewport("", true, viewport.width, viewport.height)
                .scroll_offset
                > 0
        );
    }

    #[test]
    fn questions_step_enter_records_answer_and_advances() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![
                pending_question("How should it be validated?"),
                pending_question("Who owns it?"),
            ],
            selected: 0,
            error: Some("stale".to_string()),
        };
        let _ = app.questions[0]
            .answer
            .paste("Direct command-path proofs and targeted tests");

        let action = handle_questions_step_key(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter),
            80,
        );

        assert!(matches!(action, SessionAction::None));
        assert_eq!(app.questions[0].state, FollowUpAnswerState::Answered);
        assert_eq!(app.selected, 1);
        assert_eq!(app.error, None);
    }

    #[test]
    fn questions_step_enter_generates_plan_when_last_answer_is_recorded() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![
                answered_question("Who uses it?", "CLI maintainers"),
                pending_question("How should it be validated?"),
            ],
            selected: 1,
            error: Some("stale".to_string()),
        };
        let _ = app.questions[1]
            .answer
            .paste("Direct command-path proofs and targeted tests");

        let action = handle_questions_step_key(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Enter),
            80,
        );

        match action {
            SessionAction::GeneratePlan {
                request,
                follow_ups,
                ..
            } => {
                assert_eq!(request, "Plan a new command");
                assert_eq!(follow_ups.len(), 2);
                assert_eq!(follow_ups[1].question, "How should it be validated?");
                assert_eq!(
                    follow_ups[1].answer,
                    "Direct command-path proofs and targeted tests"
                );
                assert!(!follow_ups[1].skipped);
            }
            _ => panic!("expected the last enter to generate a plan"),
        }
        assert_eq!(app.questions[1].state, FollowUpAnswerState::Answered);
        assert_eq!(app.error, None);
    }

    #[test]
    fn questions_step_ctrl_s_generates_a_plan_after_more_than_three_answers_are_complete() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![
                answered_question("Who uses it?", "CLI maintainers"),
                answered_question("What workflow changes?", "Interactive planning"),
                answered_question("What should stay unchanged?", "--no-interactive"),
                answered_question("How should it be validated?", "CLI tests and snapshots"),
            ],
            selected: 3,
            error: None,
        };

        let action = handle_questions_step_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('s'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
            80,
        );

        match action {
            SessionAction::GeneratePlan {
                request,
                follow_ups,
                ..
            } => {
                assert_eq!(request, "Plan a new command");
                assert_eq!(follow_ups.len(), 4);
                assert_eq!(follow_ups[3].answer, "CLI tests and snapshots");
                assert!(!follow_ups[3].skipped);
            }
            _ => panic!("expected the completed question set to generate a plan"),
        }
    }

    #[test]
    fn questions_step_empty_ctrl_s_skips_active_question_and_advances() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![
                answered_question("Who uses it?", "CLI maintainers"),
                pending_question("What workflow changes?"),
                pending_question("How should it be validated?"),
            ],
            selected: 1,
            error: None,
        };

        let action = handle_questions_step_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('s'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
            80,
        );

        assert!(matches!(action, SessionAction::None));
        assert_eq!(app.questions[1].state, FollowUpAnswerState::Skipped);
        assert_eq!(app.selected, 2);
        assert_eq!(app.error, None);
    }

    #[test]
    fn questions_step_ctrl_s_generates_plan_for_mixed_answered_and_skipped_follow_ups() {
        let mut app = QuestionsApp {
            request: "Plan a new command".to_string(),
            request_attachments: Vec::new(),
            questions: vec![
                answered_question("Who uses it?", "CLI maintainers"),
                QuestionAnswer {
                    question: "What workflow changes?".to_string(),
                    answer: InputFieldState::multiline(String::new()),
                    state: FollowUpAnswerState::Skipped,
                },
                answered_question("How should it be validated?", "CLI tests"),
            ],
            selected: 2,
            error: None,
        };

        let action = handle_questions_step_key(
            &mut app,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('s'),
                crossterm::event::KeyModifiers::CONTROL,
            ),
            80,
        );

        match action {
            SessionAction::GeneratePlan {
                request,
                follow_ups,
                ..
            } => {
                assert_eq!(request, "Plan a new command");
                assert_eq!(follow_ups.len(), 3);
                assert_eq!(follow_ups[1].question, "What workflow changes?");
                assert!(follow_ups[1].skipped);
                assert_eq!(follow_ups[1].answer, "");
            }
            _ => panic!("expected the mixed response set to generate a plan"),
        }
    }

    #[test]
    fn review_dashboard_lists_generated_issues() {
        let mut app = build_review_app(
            "Plan a meta plan command".to_string(),
            vec![],
            vec![],
            PlannedIssueSet {
                summary: "Split the work into command wiring and dashboard behavior.".to_string(),
                issues: vec![
                    PlannedIssueDraft {
                        title: "Add the plan command".to_string(),
                        description: "Wire the top-level command and non-interactive flow."
                            .to_string(),
                        acceptance_criteria: vec!["`meta plan --help` works".to_string()],
                        priority: Some(2),
                    },
                    PlannedIssueDraft {
                        title: "Add the planning dashboard".to_string(),
                        description: "Capture request, follow-up answers, and review.".to_string(),
                        acceptance_criteria: vec![],
                        priority: Some(3),
                    },
                ],
            },
            2,
        );
        app.decisions = vec![1, 2];
        let snapshot = render_review_snapshot(&app);

        assert!(snapshot.contains("Suggested Tickets (2)"));
        assert!(snapshot.contains("Add the plan command"));
        assert!(snapshot.contains("Selected Ticket"));
        assert!(snapshot.contains("Follow-ups: 0 answered, 0 skipped"));
        assert!(snapshot.contains("Draft batch: 2"));
        assert!(snapshot.contains("Combination Plan"));
        assert!(snapshot.contains("[ ] Skip the ticket"));
        assert!(snapshot.contains("[1] Add the planning dashboard"));
    }

    #[test]
    fn review_step_tab_focuses_scrollable_review_panes() {
        let mut app = build_review_app(
            "Plan a meta plan command".to_string(),
            vec![],
            vec![],
            PlannedIssueSet {
                summary: "Split the work into command wiring and dashboard behavior.".to_string(),
                issues: vec![PlannedIssueDraft {
                    title: "Add the review UI".to_string(),
                    description: "Capture request, follow-up answers, and review.".to_string(),
                    acceptance_criteria: vec![],
                    priority: Some(2),
                }],
            },
            1,
        );
        let layout = review_layout(Rect::new(0, 0, 140, 36));

        let action = handle_review_step_key(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Tab),
            layout,
        );
        assert!(matches!(action, SessionAction::None));
        assert_eq!(app.focus, ReviewFocus::SelectedTicket);

        let action = handle_review_step_key(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::Tab),
            layout,
        );
        assert!(matches!(action, SessionAction::None));
        assert_eq!(app.focus, ReviewFocus::Overview);

        let snapshot = render_review_snapshot(&app);
        assert!(snapshot.contains("Overview [scroll]"));
    }

    #[test]
    fn review_selected_ticket_snapshot_scrolls_to_visible_bottom_rows() {
        let mut app = build_review_app(
            "Plan a long review".to_string(),
            vec![],
            vec![],
            PlannedIssueSet {
                summary: "Split the work.".to_string(),
                issues: vec![PlannedIssueDraft {
                    title: "Scroll the selected ticket".to_string(),
                    description: (1..=80)
                        .map(|index| format!("DETAIL-{index:02}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    acceptance_criteria: vec!["Criterion A".to_string(), "Criterion B".to_string()],
                    priority: Some(2),
                }],
            },
            1,
        );
        app.focus = ReviewFocus::SelectedTicket;
        let layout = review_layout(Rect::new(0, 0, 100, 18));

        let action = handle_review_step_key(
            &mut app,
            crossterm::event::KeyEvent::from(crossterm::event::KeyCode::End),
            layout,
        );

        assert!(matches!(action, SessionAction::None));
        assert!(app.selected_ticket_scroll.offset() > 0);

        let backend = TestBackend::new(100, 18);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");
        terminal
            .draw(|frame| render_review_form_frame(frame, &app))
            .expect("review form should render");
        let snapshot = snapshot(terminal.backend());

        assert!(snapshot.contains("DETAIL-80"));
        assert!(!snapshot.contains("DETAIL-01"));
    }

    #[test]
    fn review_step_mouse_wheel_scrolls_only_the_focused_selected_ticket() {
        let mut app = build_review_app(
            "Plan a long review".to_string(),
            vec![],
            vec![],
            PlannedIssueSet {
                summary: "Split the work.".to_string(),
                issues: vec![PlannedIssueDraft {
                    title: "Scroll the selected ticket".to_string(),
                    description: (1..=80)
                        .map(|index| format!("DETAIL-{index:02}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    acceptance_criteria: vec![],
                    priority: Some(2),
                }],
            },
            1,
        );
        let layout = review_layout(Rect::new(0, 0, 100, 18));
        let mouse = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: layout.selected_ticket.x + 1,
            row: layout.selected_ticket.y + 1,
            modifiers: KeyModifiers::NONE,
        };

        assert!(!handle_review_step_mouse(&mut app, mouse, layout));
        assert_eq!(app.selected_ticket_scroll.offset(), 0);

        app.focus = ReviewFocus::SelectedTicket;
        assert!(handle_review_step_mouse(&mut app, mouse, layout));
        assert!(app.selected_ticket_scroll.offset() > 0);
    }

    #[test]
    fn loading_dashboard_shows_spinner_message() {
        let snapshot = render_loading_snapshot(&LoadingApp {
            message: "Generating suggested tickets".to_string(),
            detail: "Drafting Linear-ready backlog tickets from the request.".to_string(),
            spinner_index: 1,
        });

        assert!(snapshot.contains("[==  ] Generating suggested tickets"));
        assert!(snapshot.contains("Agent Working [loading]"));
    }

    #[test]
    fn plan_session_clears_previous_stage_content_before_redraw() {
        let backend = TestBackend::new(120, 32);
        let mut terminal = Terminal::new(backend).expect("terminal should initialize");

        terminal
            .draw(|frame| {
                render_plan_session(
                    frame,
                    &PlanSessionApp {
                        stage: PlanStage::Request(RequestApp {
                            request: InputFieldState::multiline(
                                "Plan a dashboard for multi-ticket backlog work",
                            ),
                            error: None,
                        }),
                        agent_overrides: PlanningAgentOverrides::default(),
                        continuation: None,
                        pending: None,
                    },
                )
            })
            .expect("request frame should render");
        terminal
            .draw(|frame| {
                render_plan_session(
                    frame,
                    &PlanSessionApp {
                        stage: PlanStage::Loading(LoadingApp {
                            message: "Generating suggested tickets".to_string(),
                            detail: "Drafting Linear-ready backlog tickets from the request."
                                .to_string(),
                            spinner_index: 0,
                        }),
                        agent_overrides: PlanningAgentOverrides::default(),
                        continuation: None,
                        pending: None,
                    },
                )
            })
            .expect("loading frame should render");

        let snapshot = snapshot(terminal.backend());
        assert!(snapshot.contains("Agent Working [loading]"));
        assert!(!snapshot.contains("Planning Request [editing]"));
        assert!(!snapshot.contains("Plan a dashboard for multi-ticket backlog work"));
    }

    #[test]
    fn merge_prompt_includes_grouped_ticket_subset() {
        let temp = tempdir().expect("tempdir should create");
        let root = temp.path();
        let metastack_dir = root.join(".metastack");
        std::fs::create_dir_all(&metastack_dir).expect("planning dir should exist");
        for file in [
            "SCAN.md",
            "ARCHITECTURE.md",
            "CONVENTIONS.md",
            "STACK.md",
            "STRUCTURE.md",
            "TESTING.md",
        ] {
            std::fs::write(metastack_dir.join(file), format!("# {file}\n")).expect("context file");
        }

        let plan = PlannedIssueSet {
            summary: "Split command and UI work.".to_string(),
            issues: vec![
                PlannedIssueDraft {
                    title: "Add CLI wiring".to_string(),
                    description: "Handle non-interactive creation.".to_string(),
                    acceptance_criteria: vec!["CLI works".to_string()],
                    priority: Some(2),
                },
                PlannedIssueDraft {
                    title: "Add the review UI".to_string(),
                    description: "Support review before create.".to_string(),
                    acceptance_criteria: vec!["Review is interactive".to_string()],
                    priority: Some(3),
                },
            ],
        };
        let mut review = build_review_app(
            "Plan a better `meta plan` workflow".to_string(),
            vec![],
            vec![answered_follow_up("Who uses it?", "CLI maintainers")],
            plan,
            1,
        );
        review.decisions = vec![2, 2];
        let prompt = render_issue_merge_prompt(
            root,
            "Plan a better `meta plan` workflow",
            &review.follow_ups,
            &review.plan,
            &review_kept_indices(&review),
            &review_merge_groups(&review),
        )
        .expect("prompt should render");

        assert!(prompt.contains("Merge groups"));
        assert!(prompt.contains("Add CLI wiring"));
        assert!(prompt.contains("Add the review UI"));
        assert!(prompt.contains("\"group\": 1"));
    }

    #[test]
    fn merge_prompt_mentions_selected_standalone_tickets_and_skipped_scope() {
        let temp = tempdir().expect("tempdir should create");
        let root = temp.path();
        let metastack_dir = root.join(".metastack");
        std::fs::create_dir_all(&metastack_dir).expect("planning dir should exist");
        for file in [
            "SCAN.md",
            "ARCHITECTURE.md",
            "CONVENTIONS.md",
            "STACK.md",
            "STRUCTURE.md",
            "TESTING.md",
        ] {
            std::fs::write(metastack_dir.join(file), format!("# {file}\n")).expect("context file");
        }

        let plan = PlannedIssueSet {
            summary: "Split command and UI work.".to_string(),
            issues: vec![
                PlannedIssueDraft {
                    title: "Keep CLI wiring".to_string(),
                    description: "Keep this draft as-is.".to_string(),
                    acceptance_criteria: vec!["CLI works".to_string()],
                    priority: Some(2),
                },
                PlannedIssueDraft {
                    title: "Merge the review UI".to_string(),
                    description: "Combine this ticket.".to_string(),
                    acceptance_criteria: vec!["Review is interactive".to_string()],
                    priority: Some(3),
                },
                PlannedIssueDraft {
                    title: "Merge the create path".to_string(),
                    description: "Combine this ticket too.".to_string(),
                    acceptance_criteria: vec!["Creation is interactive".to_string()],
                    priority: Some(2),
                },
                PlannedIssueDraft {
                    title: "Skip this ticket".to_string(),
                    description: "Do not include this draft.".to_string(),
                    acceptance_criteria: vec!["Skipped".to_string()],
                    priority: Some(4),
                },
            ],
        };
        let mut review = build_review_app(
            "Plan a better `meta plan` workflow".to_string(),
            vec![],
            vec![
                answered_follow_up("Who uses it?", "CLI maintainers"),
                skipped_follow_up("What should stay unchanged?"),
            ],
            plan,
            1,
        );
        review.decisions = vec![1, 2, 2, 0];

        let prompt = render_issue_merge_prompt(
            root,
            "Plan a better `meta plan` workflow",
            &review.follow_ups,
            &review.plan,
            &review_kept_indices(&review),
            &review_merge_groups(&review),
        )
        .expect("prompt should render");

        assert!(prompt.contains("Selected standalone tickets to preserve"));
        assert!(prompt.contains("Keep CLI wiring"));
        assert!(prompt.contains("Tickets omitted from both lists were intentionally skipped"));
        assert!(prompt.contains("Merge the review UI"));
        assert!(prompt.contains("Merge the create path"));
        assert!(prompt.contains(SKIPPED_FOLLOW_UP_LABEL));
    }

    #[test]
    fn question_prompt_uses_the_default_interactive_follow_up_limit() {
        let temp = tempdir().expect("tempdir should create");
        let root = temp.path();
        let metastack_dir = root.join(".metastack");
        std::fs::create_dir_all(&metastack_dir).expect("planning dir should exist");
        for file in [
            "SCAN.md",
            "ARCHITECTURE.md",
            "CONVENTIONS.md",
            "STACK.md",
            "STRUCTURE.md",
            "TESTING.md",
        ] {
            std::fs::write(metastack_dir.join(file), format!("# {file}\n")).expect("context file");
        }

        let prompt = render_question_prompt(
            root,
            "Plan a dashboard flow",
            DEFAULT_INTERACTIVE_PLAN_FOLLOW_UP_QUESTION_LIMIT,
        )
        .expect("prompt should render");

        assert!(prompt.contains("Injected workflow contract:"));
        assert!(prompt.contains("## Built-in Workflow Contract"));
        assert!(prompt.contains("Default scope: the full repository rooted at"));
        assert!(prompt.contains("Ask at most 10 concise follow-up questions"));
        assert!(!prompt.contains("MetaStack CLI"));
    }

    #[test]
    fn question_prompt_uses_a_custom_interactive_follow_up_limit() {
        let temp = tempdir().expect("tempdir should create");
        let root = temp.path();
        let metastack_dir = root.join(".metastack");
        std::fs::create_dir_all(&metastack_dir).expect("planning dir should exist");
        for file in [
            "SCAN.md",
            "ARCHITECTURE.md",
            "CONVENTIONS.md",
            "STACK.md",
            "STRUCTURE.md",
            "TESTING.md",
        ] {
            std::fs::write(metastack_dir.join(file), format!("# {file}\n")).expect("context file");
        }

        let prompt =
            render_question_prompt(root, "Plan a dashboard flow", 4).expect("prompt should render");

        assert!(prompt.contains("Ask at most 4 concise follow-up questions"));
        assert!(
            prompt.contains("create backlog issues only for work inside this repository directory")
        );
        assert!(!prompt.contains("Ask at most 3 concise follow-up questions"));
    }

    #[test]
    fn interactive_zero_question_result_skips_directly_to_plan_generation() {
        let temp = tempdir().expect("tempdir should create");
        let (sender, receiver) = mpsc::channel();
        sender
            .send(PlanWorkerReport {
                continuation: None,
                outcome: Ok(PlanWorkerOutcome::Questions {
                    request: "Plan a dashboard flow".to_string(),
                    request_attachments: Vec::new(),
                    questions: Vec::new(),
                }),
            })
            .expect("worker result should send");
        drop(sender);

        let mut app = PlanSessionApp {
            stage: PlanStage::Loading(LoadingApp {
                message: "Generating follow-up questions".to_string(),
                detail: "Reviewing the request.".to_string(),
                spinner_index: 0,
            }),
            agent_overrides: PlanningAgentOverrides::default(),
            continuation: None,
            pending: Some(PendingPlanJob {
                receiver,
                previous_stage: PlanStage::Request(RequestApp {
                    request: InputFieldState::multiline("Plan a dashboard flow"),
                    error: None,
                }),
            }),
        };

        process_pending_plan_job(&mut app, temp.path()).expect("pending job should process");

        match app.stage {
            PlanStage::Loading(LoadingApp { ref message, .. }) => {
                assert_eq!(message, "Generating suggested tickets");
            }
            _ => panic!("expected zero questions to skip the question step"),
        }
        assert!(app.pending.is_some());
    }

    #[test]
    fn review_marker_cycles_from_blank_to_keep_to_merge_groups() {
        assert_eq!(review_marker(0), "[ ]");
        assert_eq!(review_marker(1), "[x]");
        assert_eq!(review_marker(2), "[1]");
        assert_eq!(review_marker(5), "[4]");
    }

    #[test]
    fn review_submission_allows_skipping_unchecked_tickets_when_some_are_kept() {
        let mut app = build_review_app(
            "Plan a selective backlog batch".to_string(),
            vec![],
            vec![],
            PlannedIssueSet {
                summary: "Keep only one ticket.".to_string(),
                issues: vec![
                    PlannedIssueDraft {
                        title: "Keep this ticket".to_string(),
                        description: "Create this issue.".to_string(),
                        acceptance_criteria: vec![],
                        priority: Some(2),
                    },
                    PlannedIssueDraft {
                        title: "Skip this ticket".to_string(),
                        description: "Do not create this issue.".to_string(),
                        acceptance_criteria: vec![],
                        priority: Some(3),
                    },
                ],
            },
            1,
        );
        app.decisions = vec![1, 0];

        assert!(matches!(
            review_submission_action(&app),
            Ok(ReviewSubmissionAction::ConfirmAsIs)
        ));
    }

    #[test]
    fn review_submission_requires_at_least_one_selected_ticket() {
        let app = build_review_app(
            "Plan a selective backlog batch".to_string(),
            vec![],
            vec![],
            PlannedIssueSet {
                summary: "Skip everything.".to_string(),
                issues: vec![PlannedIssueDraft {
                    title: "Candidate ticket".to_string(),
                    description: "Maybe later.".to_string(),
                    acceptance_criteria: vec![],
                    priority: Some(2),
                }],
            },
            1,
        );

        match review_submission_action(&app) {
            Ok(_) => panic!("empty selection should be rejected"),
            Err(error) => assert_eq!(
                error,
                "Select at least one suggested ticket before continuing. Leave [ ] on any ticket you want to skip, use [x] to keep it, or assign a number to merge it."
            ),
        }
    }

    #[test]
    fn selected_issue_plan_filters_out_skipped_tickets() {
        let mut app = build_review_app(
            "Plan a selective backlog batch".to_string(),
            vec![],
            vec![],
            PlannedIssueSet {
                summary: "Keep only explicit tickets.".to_string(),
                issues: vec![
                    PlannedIssueDraft {
                        title: "Keep this ticket".to_string(),
                        description: "Create this issue.".to_string(),
                        acceptance_criteria: vec![],
                        priority: Some(2),
                    },
                    PlannedIssueDraft {
                        title: "Skip this ticket".to_string(),
                        description: "Do not create this issue.".to_string(),
                        acceptance_criteria: vec![],
                        priority: Some(3),
                    },
                ],
            },
            1,
        );
        app.decisions = vec![1, 0];

        let selected = selected_issue_plan(&app);

        assert_eq!(selected.issues.len(), 1);
        assert_eq!(selected.issues[0].title, "Keep this ticket");
    }

    #[test]
    fn parse_agent_json_accepts_fenced_payloads() {
        let parsed: FollowUpQuestions = parse_agent_json(
            "```json\n{\"questions\":[\"What repo area changes?\"]}\n```",
            "follow-up question generation",
        )
        .expect("fenced JSON should parse");

        assert_eq!(parsed.questions, vec!["What repo area changes?"]);
    }

    #[test]
    fn next_incomplete_question_wraps_to_the_first_unanswered_entry() {
        let questions = vec![
            answered_question("Who uses the feature?", "CLI maintainers"),
            pending_question("What workflow changes?"),
            answered_question("How should it be validated?", "Snapshot tests"),
        ];

        assert_eq!(next_incomplete_question(&questions, 2), Some(1));
    }
}

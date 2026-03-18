use std::env;
use std::fs;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::Url;
use toml::Value;

use crate::agents::{
    command_args_for_invocation, resolve_agent_invocation_for_planning,
    validate_invocation_command_surface,
};
use crate::cli::RunAgentArgs;
use crate::config::{
    AGENT_ROUTE_AGENTS_LISTEN, AppConfig, LinearConfig, PlanningMeta, no_agent_selected_route_key,
};
use crate::linear::{LinearClient, LinearService};

pub(super) struct ListenPreflightRequest<'a> {
    pub(super) working_dir: &'a Path,
    pub(super) agent: Option<&'a str>,
    pub(super) model: Option<&'a str>,
    pub(super) reasoning: Option<&'a str>,
    pub(super) require_write_access: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ListenPreflightReport {
    provider: String,
    checks: Vec<String>,
    warnings: Vec<String>,
}

impl ListenPreflightReport {
    fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            checks: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn push_check(&mut self, check: impl Into<String>) {
        self.checks.push(check.into());
    }

    fn push_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }

    pub(super) fn render(&self) -> String {
        let mut lines = vec![format!(
            "Listen preflight passed for provider `{}`.",
            self.provider
        )];

        for check in &self.checks {
            lines.push(format!("- {check}"));
        }

        for warning in &self.warnings {
            lines.push(format!("Warning: {warning}"));
        }

        lines.join("\n")
    }
}

#[derive(Debug, Clone)]
struct CodexGlobalConfigStatus {
    path: PathBuf,
    approval_policy: Option<String>,
    sandbox_mode: Option<String>,
    linear_mcp_configured: bool,
}

pub(super) fn emit_listen_preflight_warnings(report: &ListenPreflightReport) {
    for warning in &report.warnings {
        eprintln!("listen preflight warning: {warning}");
    }
}

pub(super) fn render_listen_preflight_report(
    result: Result<&ListenPreflightReport, &anyhow::Error>,
) -> String {
    match result {
        Ok(report) => report.render(),
        Err(error) => format!("Listen preflight failed.\n{error}"),
    }
}

pub(super) fn is_missing_agent_selection(error: &anyhow::Error) -> bool {
    no_agent_selected_route_key(error) == Some(AGENT_ROUTE_AGENTS_LISTEN)
}

pub(super) async fn run_listen_preflight<C>(
    service: &LinearService<C>,
    linear_config: &LinearConfig,
    app_config: &AppConfig,
    planning_meta: &PlanningMeta,
    request: ListenPreflightRequest<'_>,
) -> Result<ListenPreflightReport>
where
    C: LinearClient,
{
    let report = run_listen_provider_preflight(app_config, planning_meta, request)?;
    complete_listen_preflight(service, linear_config, report).await
}

pub(super) fn run_listen_provider_preflight(
    app_config: &AppConfig,
    planning_meta: &PlanningMeta,
    request: ListenPreflightRequest<'_>,
) -> Result<ListenPreflightReport> {
    let invocation = resolve_agent_invocation_for_planning(
        app_config,
        planning_meta,
        &RunAgentArgs {
            root: None,
            route_key: Some(AGENT_ROUTE_AGENTS_LISTEN.to_string()),
            agent: request.agent.map(str::to_string),
            prompt: "listen preflight".to_string(),
            instructions: None,
            model: request.model.map(str::to_string),
            reasoning: request.reasoning.map(str::to_string),
            transport: None,
        },
    )?;
    let command_args = command_args_for_invocation(&invocation, Some(request.working_dir))?;
    let attempted_command = validate_invocation_command_surface(&invocation, &command_args)?;
    verify_listen_command_capabilities(&invocation.agent, &command_args)?;

    let mut report = ListenPreflightReport::new(invocation.agent.clone());
    if request.require_write_access {
        verify_workspace_write_access(request.working_dir)?;
        report.push_check(format!(
            "Workspace `{}` is writable.",
            request.working_dir.display()
        ));
    }
    let display_command = attempted_command
        .lines()
        .next()
        .unwrap_or(&attempted_command);
    report.push_check(format!("Resolved listen command: `{display_command} ...`"));

    match invocation.agent.as_str() {
        "codex" => verify_codex_listen_prerequisites(&mut report)?,
        "claude" => verify_claude_listen_prerequisites(&mut report)?,
        _ => {}
    }

    Ok(report)
}

pub(super) async fn complete_listen_preflight<C>(
    service: &LinearService<C>,
    linear_config: &LinearConfig,
    mut report: ListenPreflightReport,
) -> Result<ListenPreflightReport>
where
    C: LinearClient,
{
    verify_network_connectivity(&linear_config.api_url)?;
    report.push_check(format!(
        "Linear API endpoint is reachable at `{}`.",
        linear_config.api_url
    ));
    verify_linear_api_access(service).await?;
    report.push_check("Linear API authentication succeeded.");
    Ok(report)
}

pub(super) fn verify_workspace_write_access(workspace_path: &Path) -> Result<()> {
    let probe_path = workspace_path
        .join(".metastack")
        .join(format!(".listen-preflight-{}", std::process::id()));
    if let Some(parent) = probe_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to prepare `{}`", parent.display()))?;
    }
    fs::write(&probe_path, "preflight")
        .with_context(|| format!("workspace `{}` is not writable", workspace_path.display()))?;
    fs::remove_file(&probe_path)
        .with_context(|| format!("failed to remove `{}`", probe_path.display()))?;
    Ok(())
}

pub(super) fn verify_network_connectivity(api_url: &str) -> Result<()> {
    let url = Url::parse(api_url)
        .with_context(|| format!("failed to parse Linear API URL `{api_url}` for preflight"))?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("Linear API URL `{api_url}` does not include a hostname"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow!("Linear API URL `{api_url}` does not include a known port"))?;
    let mut last_error = None;
    for address in (host, port)
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve `{host}:{port}` during listen preflight"))?
    {
        match TcpStream::connect_timeout(&address, Duration::from_secs(2)) {
            Ok(stream) => {
                drop(stream);
                return Ok(());
            }
            Err(error) => last_error = Some(error),
        }
    }

    let detail = last_error
        .map(|error| error.to_string())
        .unwrap_or_else(|| "no addresses available".to_string());
    bail!("failed to connect to `{host}:{port}` during listen preflight: {detail}");
}

pub(super) async fn verify_linear_api_access<C>(service: &LinearService<C>) -> Result<()>
where
    C: LinearClient,
{
    service
        .viewer()
        .await
        .context("failed to access Linear API during listen preflight")?;
    Ok(())
}

fn verify_codex_listen_prerequisites(report: &mut ListenPreflightReport) -> Result<()> {
    verify_command_on_path("codex")?;
    report.push_check("`codex` is available on PATH.");

    let status = load_codex_global_config_status()?;
    let mut missing_settings = Vec::new();

    if status.approval_policy.as_deref() != Some("never") {
        missing_settings.push("approval_policy = \"never\"");
    }
    if status.sandbox_mode.as_deref() != Some("danger-full-access") {
        missing_settings.push("sandbox_mode = \"danger-full-access\"");
    }

    if !missing_settings.is_empty() {
        bail!(
            "Listen requires the Codex global config at `~/.codex/config.toml` (resolved to `{}`) to include:\n{}\nCurrent values:\n- approval_policy = {}\n- sandbox_mode = {}",
            status.path.display(),
            missing_settings.join("\n"),
            display_setting(status.approval_policy.as_deref()),
            display_setting(status.sandbox_mode.as_deref()),
        );
    }

    report.push_check(format!(
        "Codex global config loaded from `{}`.",
        status.path.display()
    ));
    report.push_check("`approval_policy = \"never\"` is configured.");
    report.push_check("`sandbox_mode = \"danger-full-access\"` is configured.");

    if status.linear_mcp_configured {
        report.push_warning(format!(
            "Linear MCP is configured in `{}`. Linear MCP should be removed from codex config because the harness manages Linear directly. Disable it with `-c mcp_servers.linear.enabled=false` or remove `[mcp_servers.linear]`.",
            status.path.display()
        ));
    }

    Ok(())
}

fn verify_claude_listen_prerequisites(report: &mut ListenPreflightReport) -> Result<()> {
    verify_command_on_path("claude")?;
    report.push_check("`claude` is available on PATH.");

    if env::var_os("ANTHROPIC_API_KEY")
        .map(|value| !value.is_empty())
        .unwrap_or(false)
    {
        bail!(
            "Listen cannot launch built-in `claude` workers while `ANTHROPIC_API_KEY` is set. Headless listen should use the local Claude subscription without that override."
        );
    }

    report.push_check("`ANTHROPIC_API_KEY` is not set.");
    Ok(())
}

fn verify_command_on_path(command: &str) -> Result<()> {
    let Some(paths) = env::var_os("PATH") else {
        bail!("listen requires `{command}` on PATH, but PATH is not set");
    };

    let available = env::split_paths(&paths).any(|entry| {
        let candidate = entry.join(command);
        if candidate.is_file() {
            return true;
        }

        #[cfg(windows)]
        {
            entry.join(format!("{command}.exe")).is_file()
        }

        #[cfg(not(windows))]
        {
            false
        }
    });

    if !available {
        bail!("listen requires `{command}` on PATH");
    }

    Ok(())
}

fn load_codex_global_config_status() -> Result<CodexGlobalConfigStatus> {
    let path = resolve_codex_global_config_path()?;
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "Listen requires the Codex global config at `~/.codex/config.toml` (resolved to `{}`) to exist and include:\napproval_policy = \"never\"\nsandbox_mode = \"danger-full-access\"",
                path.display()
            );
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read `{}`", path.display()));
        }
    };

    let value: Value = toml::from_str(&contents).with_context(|| {
        format!(
            "failed to parse `{}`. Listen requires:\napproval_policy = \"never\"\nsandbox_mode = \"danger-full-access\"",
            path.display()
        )
    })?;

    Ok(CodexGlobalConfigStatus {
        approval_policy: value
            .get("approval_policy")
            .and_then(Value::as_str)
            .map(str::to_string),
        sandbox_mode: value
            .get("sandbox_mode")
            .and_then(Value::as_str)
            .map(str::to_string),
        linear_mcp_configured: value
            .get("mcp_servers")
            .and_then(Value::as_table)
            .and_then(|servers| servers.get("linear"))
            .is_some(),
        path,
    })
}

fn resolve_codex_global_config_path() -> Result<PathBuf> {
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home).join(".codex").join("config.toml"));
    }

    #[cfg(windows)]
    if let Some(home) = env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(home).join(".codex").join("config.toml"));
    }

    bail!(
        "listen requires `~/.codex/config.toml`, but HOME is not set. Configure HOME or add:\napproval_policy = \"never\"\nsandbox_mode = \"danger-full-access\""
    )
}

fn display_setting(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{value}\""))
        .unwrap_or_else(|| "<missing>".to_string())
}

pub(super) fn verify_listen_command_capabilities(
    agent: &str,
    command_args: &[String],
) -> Result<()> {
    match agent {
        "codex" => {
            if command_args
                .iter()
                .any(|arg| arg == "--dangerously-bypass-approvals-and-sandbox")
            {
                return Ok(());
            }
            bail!(
                "listen worker for `codex` requires `--dangerously-bypass-approvals-and-sandbox`; `codex exec --full-auto` remains sandboxed (`workspace-write`) and is not sufficient for listen. Command args were: {}",
                command_args.join(" ")
            );
        }
        "claude" => {
            if command_args.iter().any(|arg| {
                arg == "--dangerously-skip-permissions"
                    || arg == "--permission-mode=bypassPermissions"
            }) || command_args
                .windows(2)
                .any(|pair| pair[0] == "--permission-mode" && pair[1] == "bypassPermissions")
            {
                return Ok(());
            }
            bail!(
                "listen worker for `claude` requires bypassed permissions; command args were: {}",
                command_args.join(" ")
            );
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::env;
    use std::fs;
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};

    use anyhow::{Result, anyhow};
    use async_trait::async_trait;
    use tempfile::tempdir;

    use super::{
        ListenPreflightReport, ListenPreflightRequest, display_setting,
        load_codex_global_config_status, run_listen_preflight, verify_claude_listen_prerequisites,
        verify_listen_command_capabilities,
    };
    use crate::config::{
        AgentCommandConfig, AgentSettings, AppConfig, LinearConfig, PlanningMeta, PromptTransport,
    };
    use crate::linear::{
        AttachmentCreateRequest, AttachmentSummary, IssueComment, IssueCreateRequest,
        IssueLabelCreateRequest, IssueListFilters, IssueSummary, LabelRef, LinearClient,
        LinearService, ProjectSummary, TeamSummary, UserRef,
    };

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn stub_app_config() -> AppConfig {
        AppConfig {
            agents: AgentSettings {
                default_agent: Some("stub".to_string()),
                commands: BTreeMap::from([(
                    "stub".to_string(),
                    AgentCommandConfig {
                        command: "stub-agent".to_string(),
                        args: vec!["{{payload}}".to_string()],
                        transport: PromptTransport::Arg,
                    },
                )]),
                ..AgentSettings::default()
            },
            ..AppConfig::default()
        }
    }

    #[derive(Debug, Clone)]
    struct StubLinearClient {
        viewer_error: Option<String>,
    }

    #[async_trait]
    impl LinearClient for StubLinearClient {
        async fn list_projects(&self, _: usize) -> Result<Vec<ProjectSummary>> {
            unreachable!("unused in preflight tests")
        }

        async fn list_issues(&self, _: usize) -> Result<Vec<IssueSummary>> {
            unreachable!("unused in preflight tests")
        }

        async fn list_filtered_issues(&self, _: &IssueListFilters) -> Result<Vec<IssueSummary>> {
            unreachable!("unused in preflight tests")
        }

        async fn list_issue_labels(&self, _: Option<&str>) -> Result<Vec<LabelRef>> {
            unreachable!("unused in preflight tests")
        }

        async fn get_issue(&self, _: &str) -> Result<IssueSummary> {
            unreachable!("unused in preflight tests")
        }

        async fn list_teams(&self) -> Result<Vec<TeamSummary>> {
            unreachable!("unused in preflight tests")
        }

        async fn viewer(&self) -> Result<UserRef> {
            if let Some(error) = &self.viewer_error {
                Err(anyhow!(error.clone()))
            } else {
                Ok(UserRef {
                    id: "viewer-1".to_string(),
                    name: "Viewer".to_string(),
                    email: Some("viewer@example.com".to_string()),
                })
            }
        }

        async fn create_issue(&self, _: IssueCreateRequest) -> Result<IssueSummary> {
            unreachable!("unused in preflight tests")
        }

        async fn create_issue_label(&self, _: IssueLabelCreateRequest) -> Result<LabelRef> {
            unreachable!("unused in preflight tests")
        }

        async fn update_issue(
            &self,
            _: &str,
            _: crate::linear::IssueUpdateRequest,
        ) -> Result<IssueSummary> {
            unreachable!("unused in preflight tests")
        }

        async fn create_comment(&self, _: &str, _: String) -> Result<IssueComment> {
            unreachable!("unused in preflight tests")
        }

        async fn update_comment(&self, _: &str, _: String) -> Result<IssueComment> {
            unreachable!("unused in preflight tests")
        }

        async fn upload_file(&self, _: &str, _: &str, _: Vec<u8>) -> Result<String> {
            unreachable!("unused in preflight tests")
        }

        async fn create_attachment(&self, _: AttachmentCreateRequest) -> Result<AttachmentSummary> {
            unreachable!("unused in preflight tests")
        }

        async fn delete_attachment(&self, _: &str) -> Result<()> {
            unreachable!("unused in preflight tests")
        }

        async fn download_file(&self, _: &str) -> Result<Vec<u8>> {
            unreachable!("unused in preflight tests")
        }
    }

    #[tokio::test]
    async fn listen_preflight_reports_linear_connectivity_and_viewer_access() -> Result<()> {
        let temp = tempdir()?;
        let workspace = temp.path();
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let linear_config = LinearConfig {
            api_key: "token".to_string(),
            api_url: format!("http://{}/graphql", listener.local_addr()?),
            default_team: None,
        };
        let service = LinearService::new(
            StubLinearClient { viewer_error: None },
            linear_config.default_team.clone(),
        );

        let report = run_listen_preflight(
            &service,
            &linear_config,
            &stub_app_config(),
            &PlanningMeta::default(),
            ListenPreflightRequest {
                working_dir: workspace,
                agent: None,
                model: None,
                reasoning: None,
                require_write_access: false,
            },
        )
        .await?;

        let rendered = report.render();
        assert!(rendered.contains("Linear API endpoint is reachable"));
        assert!(rendered.contains("Linear API authentication succeeded."));
        Ok(())
    }

    #[tokio::test]
    async fn listen_preflight_fails_when_linear_viewer_check_fails() {
        let temp = tempdir().expect("tempdir");
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let linear_config = LinearConfig {
            api_key: "token".to_string(),
            api_url: format!(
                "http://{}/graphql",
                listener.local_addr().expect("local addr")
            ),
            default_team: None,
        };
        let service = LinearService::new(
            StubLinearClient {
                viewer_error: Some("viewer rejected".to_string()),
            },
            linear_config.default_team.clone(),
        );

        let error = run_listen_preflight(
            &service,
            &linear_config,
            &stub_app_config(),
            &PlanningMeta::default(),
            ListenPreflightRequest {
                working_dir: temp.path(),
                agent: None,
                model: None,
                reasoning: None,
                require_write_access: false,
            },
        )
        .await
        .expect_err("viewer failure should fail preflight");

        assert!(
            error
                .to_string()
                .contains("failed to access Linear API during listen preflight")
        );
    }

    #[test]
    fn codex_config_status_reads_required_fields_and_linear_mcp() -> Result<()> {
        let _guard = env_lock().lock().expect("env lock");
        let temp = tempdir()?;
        let codex_dir = temp.path().join(".codex");
        fs::create_dir_all(&codex_dir)?;
        fs::write(
            codex_dir.join("config.toml"),
            r#"
approval_policy = "never"
sandbox_mode = "danger-full-access"

[mcp_servers.linear]
enabled = true
"#,
        )?;

        let original_home = env::var_os("HOME");
        unsafe {
            env::set_var("HOME", temp.path());
        }
        let status = load_codex_global_config_status()?;
        if let Some(value) = original_home {
            unsafe {
                env::set_var("HOME", value);
            }
        } else {
            unsafe {
                env::remove_var("HOME");
            }
        }

        assert_eq!(status.approval_policy.as_deref(), Some("never"));
        assert_eq!(status.sandbox_mode.as_deref(), Some("danger-full-access"));
        assert!(status.linear_mcp_configured);
        Ok(())
    }

    #[test]
    fn claude_preflight_rejects_anthropic_api_key_override() {
        let _guard = env_lock().lock().expect("env lock");
        let mut report = ListenPreflightReport::new("claude");
        let original_path = env::var_os("PATH");
        let original_api_key = env::var_os("ANTHROPIC_API_KEY");
        let temp = tempdir().expect("tempdir");
        let claude_path = temp.path().join("claude");
        fs::write(&claude_path, "#!/bin/sh\nexit 0\n").expect("write claude stub");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&claude_path)
                .expect("claude metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&claude_path, permissions).expect("chmod claude stub");
        }
        unsafe {
            env::set_var("PATH", temp.path());
            env::set_var("ANTHROPIC_API_KEY", "token");
        }

        let error = verify_claude_listen_prerequisites(&mut report)
            .expect_err("ANTHROPIC_API_KEY should be rejected");

        if let Some(value) = original_path {
            unsafe {
                env::set_var("PATH", value);
            }
        } else {
            unsafe {
                env::remove_var("PATH");
            }
        }
        if let Some(value) = original_api_key {
            unsafe {
                env::set_var("ANTHROPIC_API_KEY", value);
            }
        } else {
            unsafe {
                env::remove_var("ANTHROPIC_API_KEY");
            }
        }

        assert!(error.to_string().contains("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn display_setting_formats_missing_and_present_values() {
        assert_eq!(display_setting(None), "<missing>");
        assert_eq!(display_setting(Some("never")), "\"never\"");
    }

    #[test]
    fn codex_listen_capability_check_rejects_workspace_write_alias() {
        let error = verify_listen_command_capabilities(
            "codex",
            &[
                "--full-auto".to_string(),
                "--cd".to_string(),
                "/tmp/workspace".to_string(),
                "exec".to_string(),
            ],
        )
        .expect_err("codex listen should reject sandboxed full-auto mode");

        assert!(error.to_string().contains("--full-auto"));
        assert!(error.to_string().contains("workspace-write"));
    }

    #[test]
    fn claude_listen_capability_check_accepts_bypass_permission_mode() -> Result<()> {
        verify_listen_command_capabilities(
            "claude",
            &[
                "--permission-mode=bypassPermissions".to_string(),
                "-p".to_string(),
                "--model=sonnet".to_string(),
            ],
        )?;
        Ok(())
    }
}

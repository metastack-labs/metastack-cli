use std::env;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::DoctorArgs;
use crate::config::{
    AppConfig, LinearConfig, LinearConfigOverrides, PlanningMeta, detect_supported_agents,
    resolve_config_path,
};

/// Status of a single doctor check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
}

/// A single doctor check result.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

/// Full doctor report covering all environment checks.
#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<CheckResult>,
}

/// Run the `meta doctor` command and print the report.
///
/// Returns an error when any check has `Fail` status so the caller can set a
/// non-zero exit code.
pub async fn run_doctor(args: &DoctorArgs) -> Result<()> {
    let report = build_report().await;

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .context("failed to serialize doctor report as JSON")?
        );
    } else {
        println!("{}", report.render());
    }

    if report.has_failures() {
        anyhow::bail!("one or more doctor checks failed");
    }

    Ok(())
}

async fn build_report() -> DoctorReport {
    let mut checks = Vec::new();

    // 1. Required tools on PATH
    check_command_on_path(&mut checks, "git", true);
    check_command_on_path(&mut checks, "gh", true);

    // 2. Optional tools
    check_command_on_path(&mut checks, "expect", false);

    // 3. gh auth status
    check_gh_auth(&mut checks);

    // 4. Install-scoped config parse
    check_install_config(&mut checks);

    // 5. Load AppConfig for subsequent checks (best-effort)
    let app_config = AppConfig::load().ok();

    // 6. Linear API key + viewer query
    check_linear_api(&mut checks).await;

    // 7. Agent provider availability
    check_agent_providers(&mut checks, app_config.as_ref());

    // 8. Repo-scoped config (if in a repo context)
    check_repo_config(&mut checks);

    DoctorReport { checks }
}

fn check_command_on_path(checks: &mut Vec<CheckResult>, command: &str, required: bool) {
    let found = command_exists(command);
    let (status, message) = if found {
        (
            CheckStatus::Pass,
            format!("`{command}` is available on PATH"),
        )
    } else if required {
        (
            CheckStatus::Fail,
            format!("`{command}` is not found on PATH"),
        )
    } else {
        (
            CheckStatus::Warn,
            format!("`{command}` is not found on PATH (optional)"),
        )
    };
    checks.push(CheckResult {
        name: format!("{command}_on_path"),
        status,
        message,
    });
}

fn check_gh_auth(checks: &mut Vec<CheckResult>) {
    let result = Command::new("gh").args(["auth", "status"]).output();
    let (status, message) = match result {
        Ok(output) if output.status.success() => {
            (CheckStatus::Pass, "`gh` is authenticated".to_string())
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.lines().next().unwrap_or("unknown error").trim();
            (
                CheckStatus::Fail,
                format!("`gh` auth check failed: {detail}"),
            )
        }
        Err(_) => (
            CheckStatus::Fail,
            "`gh` auth check failed: could not run `gh auth status`".to_string(),
        ),
    };
    checks.push(CheckResult {
        name: "gh_auth".to_string(),
        status,
        message,
    });
}

fn check_install_config(checks: &mut Vec<CheckResult>) {
    let config_path = match resolve_config_path() {
        Ok(path) => path,
        Err(_) => {
            checks.push(CheckResult {
                name: "install_config".to_string(),
                status: CheckStatus::Warn,
                message: "could not determine install config path".to_string(),
            });
            return;
        }
    };

    if !config_path.is_file() {
        checks.push(CheckResult {
            name: "install_config".to_string(),
            status: CheckStatus::Warn,
            message: format!(
                "install config not found at `{}`; using defaults",
                config_path.display()
            ),
        });
        return;
    }

    match std::fs::read_to_string(&config_path) {
        Ok(contents) => match toml::from_str::<AppConfig>(&contents) {
            Ok(_) => {
                checks.push(CheckResult {
                    name: "install_config".to_string(),
                    status: CheckStatus::Pass,
                    message: format!(
                        "install config at `{}` is valid TOML",
                        config_path.display()
                    ),
                });
            }
            Err(err) => {
                checks.push(CheckResult {
                    name: "install_config".to_string(),
                    status: CheckStatus::Fail,
                    message: format!(
                        "install config at `{}` failed to parse: {err}",
                        config_path.display()
                    ),
                });
            }
        },
        Err(err) => {
            checks.push(CheckResult {
                name: "install_config".to_string(),
                status: CheckStatus::Fail,
                message: format!(
                    "failed to read install config at `{}`: {err}",
                    config_path.display()
                ),
            });
        }
    }
}

async fn check_linear_api(checks: &mut Vec<CheckResult>) {
    let linear_config = LinearConfig::new_with_root(None, LinearConfigOverrides::default());
    let config = match linear_config {
        Ok(config) => config,
        Err(_) => {
            checks.push(CheckResult {
                name: "linear_api_key".to_string(),
                status: CheckStatus::Fail,
                message: "Linear API key is not configured".to_string(),
            });
            return;
        }
    };

    checks.push(CheckResult {
        name: "linear_api_key".to_string(),
        status: CheckStatus::Pass,
        message: "Linear API key is set".to_string(),
    });

    match crate::linear::ReqwestLinearClient::new(config) {
        Ok(client) => {
            let service = crate::linear::LinearService::new(client, None);
            match service.viewer().await {
                Ok(viewer) => {
                    checks.push(CheckResult {
                        name: "linear_viewer".to_string(),
                        status: CheckStatus::Pass,
                        message: format!("Linear viewer query succeeded ({})", viewer.name),
                    });
                }
                Err(err) => {
                    checks.push(CheckResult {
                        name: "linear_viewer".to_string(),
                        status: CheckStatus::Fail,
                        message: format!("Linear viewer query failed: {err}"),
                    });
                }
            }
        }
        Err(err) => {
            checks.push(CheckResult {
                name: "linear_viewer".to_string(),
                status: CheckStatus::Fail,
                message: format!("failed to create Linear client: {err}"),
            });
        }
    }
}

fn check_agent_providers(checks: &mut Vec<CheckResult>, app_config: Option<&AppConfig>) {
    let configured_default = app_config
        .and_then(|config| config.agents.default_agent.as_deref())
        .map(str::to_string);

    let detected = detect_supported_agents();

    if let Some(ref default_agent) = configured_default {
        if detected.iter().any(|name| name == default_agent) {
            checks.push(CheckResult {
                name: "agent_provider".to_string(),
                status: CheckStatus::Pass,
                message: format!("configured default agent `{default_agent}` is available on PATH"),
            });
        } else {
            checks.push(CheckResult {
                name: "agent_provider".to_string(),
                status: CheckStatus::Fail,
                message: format!("configured default agent `{default_agent}` is not found on PATH"),
            });
        }
    } else if !detected.is_empty() {
        checks.push(CheckResult {
            name: "agent_provider".to_string(),
            status: CheckStatus::Pass,
            message: format!(
                "no default agent configured; detected: {}",
                detected.join(", ")
            ),
        });
    } else {
        checks.push(CheckResult {
            name: "agent_provider".to_string(),
            status: CheckStatus::Warn,
            message: "no default agent configured and no supported agents found on PATH"
                .to_string(),
        });
    }
}

fn check_repo_config(checks: &mut Vec<CheckResult>) {
    let cwd = match env::current_dir() {
        Ok(dir) => dir,
        Err(_) => return,
    };

    let meta_json = find_metastack_root(&cwd);
    let Some(meta_json_path) = meta_json else {
        checks.push(CheckResult {
            name: "repo_config".to_string(),
            status: CheckStatus::Warn,
            message: "not inside a repo with `.metastack/meta.json`".to_string(),
        });
        return;
    };

    let root = meta_json_path
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&cwd);

    match PlanningMeta::load(root) {
        Ok(_) => {
            checks.push(CheckResult {
                name: "repo_config".to_string(),
                status: CheckStatus::Pass,
                message: format!(
                    "`.metastack/meta.json` is valid ({})",
                    meta_json_path.display()
                ),
            });
        }
        Err(err) => {
            checks.push(CheckResult {
                name: "repo_config".to_string(),
                status: CheckStatus::Fail,
                message: format!("`.metastack/meta.json` failed to parse: {err}"),
            });
        }
    }
}

/// Walk upward from `start` looking for `.metastack/meta.json`.
fn find_metastack_root(start: &Path) -> Option<std::path::PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        let candidate = current.join(".metastack").join("meta.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !current.pop() {
            return None;
        }
    }
}

fn command_exists(command: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&paths).any(|entry| {
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
    })
}

impl DoctorReport {
    /// Returns `true` when any check has [`CheckStatus::Fail`].
    pub fn has_failures(&self) -> bool {
        self.checks.iter().any(|c| c.status == CheckStatus::Fail)
    }

    /// Render a human-readable checklist.
    pub fn render(&self) -> String {
        let mut lines = vec!["Environment health check".to_string()];

        for check in &self.checks {
            let icon = match check.status {
                CheckStatus::Pass => "pass",
                CheckStatus::Warn => "warn",
                CheckStatus::Fail => "FAIL",
            };
            lines.push(format!("  [{icon}] {}", check.message));
        }

        let (pass, warn, fail) = self.summary_counts();
        lines.push(String::new());
        lines.push(format!(
            "Result: {pass} passed, {warn} warnings, {fail} failures"
        ));

        lines.join("\n")
    }

    fn summary_counts(&self) -> (usize, usize, usize) {
        let pass = self
            .checks
            .iter()
            .filter(|c| c.status == CheckStatus::Pass)
            .count();
        let warn = self
            .checks
            .iter()
            .filter(|c| c.status == CheckStatus::Warn)
            .count();
        let fail = self
            .checks
            .iter()
            .filter(|c| c.status == CheckStatus::Fail)
            .count();
        (pass, warn, fail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_render_shows_pass_warn_fail_icons() {
        let report = DoctorReport {
            checks: vec![
                CheckResult {
                    name: "check_a".to_string(),
                    status: CheckStatus::Pass,
                    message: "all good".to_string(),
                },
                CheckResult {
                    name: "check_b".to_string(),
                    status: CheckStatus::Warn,
                    message: "optional missing".to_string(),
                },
                CheckResult {
                    name: "check_c".to_string(),
                    status: CheckStatus::Fail,
                    message: "broken".to_string(),
                },
            ],
        };

        let rendered = report.render();
        assert!(rendered.contains("[pass] all good"));
        assert!(rendered.contains("[warn] optional missing"));
        assert!(rendered.contains("[FAIL] broken"));
        assert!(rendered.contains("1 passed, 1 warnings, 1 failures"));
        assert!(report.has_failures());
    }

    #[test]
    fn report_without_failures_returns_false() {
        let report = DoctorReport {
            checks: vec![CheckResult {
                name: "ok".to_string(),
                status: CheckStatus::Pass,
                message: "fine".to_string(),
            }],
        };
        assert!(!report.has_failures());
    }

    #[test]
    fn json_serialization_uses_snake_case_status() {
        let check = CheckResult {
            name: "test".to_string(),
            status: CheckStatus::Pass,
            message: "ok".to_string(),
        };
        let json = serde_json::to_string(&check).expect("serialize");
        assert!(json.contains("\"status\":\"pass\""));
    }
}

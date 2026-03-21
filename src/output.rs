use anyhow::Error;
use clap::error::ErrorKind;
use serde::Serialize;

use crate::linear::IssueSummary;

#[derive(Debug, Serialize)]
struct SuccessEnvelope<'a, T> {
    status: &'static str,
    command: &'a str,
    result: T,
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope<'a> {
    status: &'static str,
    command: &'a str,
    error: StructuredError,
}

#[derive(Debug, Serialize)]
struct StructuredError {
    code: &'static str,
    message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    context: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MachineIssueSummary {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub team: String,
}

impl From<&IssueSummary> for MachineIssueSummary {
    fn from(issue: &IssueSummary) -> Self {
        Self {
            id: issue.id.clone(),
            identifier: issue.identifier.clone(),
            title: issue.title.clone(),
            url: issue.url.clone(),
            state: issue.state.as_ref().map(|state| state.name.clone()),
            project: issue.project.as_ref().map(|project| project.name.clone()),
            team: issue.team.key.clone(),
        }
    }
}

/// Wrap a command result in the standard machine-readable success envelope.
pub(crate) fn render_json_success<T>(command: &'static str, result: &T) -> anyhow::Result<String>
where
    T: Serialize,
{
    serde_json::to_string_pretty(&SuccessEnvelope {
        status: "ok",
        command,
        result,
    })
    .map_err(Into::into)
}

/// Render a command failure in the standard machine-readable error envelope.
pub(crate) fn render_json_error(command: &'static str, error: &Error) -> String {
    let chain = error.chain().map(ToString::to_string).collect::<Vec<_>>();
    let message = chain
        .first()
        .cloned()
        .unwrap_or_else(|| "command failed".to_string());
    let context = chain.into_iter().skip(1).collect::<Vec<_>>();
    render_structured_error(command, &message, context, None)
}

/// Render a clap parse failure in the standard machine-readable error envelope.
pub(crate) fn render_json_clap_error(command: &'static str, error: &clap::Error) -> String {
    let rendered = error.to_string();
    let mut sections = rendered.split("\n\n");
    let message = sections
        .next()
        .map(str::trim)
        .and_then(|line| line.strip_prefix("error: ").or(Some(line)))
        .unwrap_or("command failed")
        .trim()
        .to_string();
    let context = sections
        .flat_map(|section| section.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    render_structured_error(command, &message, context, Some(error.kind()))
}

fn render_structured_error(
    command: &'static str,
    message: &str,
    context: Vec<String>,
    clap_kind: Option<ErrorKind>,
) -> String {
    let payload = ErrorEnvelope {
        status: "error",
        command,
        error: StructuredError {
            code: classify_error_code(message, &context, clap_kind),
            message: message.to_string(),
            context,
        },
    };

    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| {
        format!(
            "{{\"status\":\"error\",\"command\":\"{command}\",\"error\":{{\"code\":\"command_failed\",\"message\":\"failed to encode structured error payload\"}}}}"
        )
    })
}

fn classify_error_code(
    message: &str,
    context: &[String],
    clap_kind: Option<ErrorKind>,
) -> &'static str {
    if matches!(
        clap_kind,
        Some(
            ErrorKind::ArgumentConflict
                | ErrorKind::InvalidValue
                | ErrorKind::UnknownArgument
                | ErrorKind::InvalidSubcommand
                | ErrorKind::NoEquals
                | ErrorKind::ValueValidation
                | ErrorKind::TooManyValues
                | ErrorKind::TooFewValues
                | ErrorKind::WrongNumberOfValues
                | ErrorKind::MissingRequiredArgument
                | ErrorKind::MissingSubcommand
                | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        )
    ) {
        return "invalid_input";
    }

    let combined = std::iter::once(message.to_ascii_lowercase())
        .chain(context.iter().map(|item| item.to_ascii_lowercase()))
        .collect::<Vec<_>>()
        .join(" | ");

    if combined.contains("required")
        || combined.contains("requires")
        || combined.contains("missing")
        || combined.contains("must be")
        || combined.contains("must not")
        || combined.contains("only accepts")
        || combined.contains("cannot be used")
        || combined.contains("without a tty")
        || combined.contains("rerun in a tty")
        || combined.contains("not found in linear")
    {
        "invalid_input"
    } else if combined.contains("api key")
        || combined.contains("auth")
        || combined.contains("permission")
        || combined.contains("unauthorized")
        || combined.contains("forbidden")
    {
        "auth_error"
    } else if combined.contains("metastack.toml")
        || combined.contains("config.toml")
        || combined.contains("unknown agent command route key")
        || combined.contains("supported keys:")
        || combined.contains("invalid `") && combined.contains(".toml")
    {
        "configuration_error"
    } else if combined.contains("interactive terminal")
        || combined.contains("local changes")
        || combined.contains("choose overwrite")
    {
        "invalid_input"
    } else if combined.contains("configured local agent")
        || combined.contains("default agent")
        || combined.contains("meta runtime config")
        || combined.contains(".metastack/meta.json")
    {
        "configuration_error"
    } else {
        "command_failed"
    }
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;
    use clap::{CommandFactory, Parser};
    use serde_json::Value;

    use super::{render_json_clap_error, render_json_error, render_json_success};

    #[derive(Debug, Parser)]
    struct ClapConflictArgs {
        #[arg(long, conflicts_with = "render_once")]
        json: bool,
        #[arg(long)]
        render_once: bool,
    }

    #[test]
    fn render_json_success_wraps_command_and_result() {
        let encoded = render_json_success(
            "backlog.plan",
            &serde_json::json!({ "issues": ["ENG-10142"] }),
        )
        .expect("json success payload should encode");
        let value: Value = serde_json::from_str(&encoded).expect("payload should parse");

        assert_eq!(value["status"], "ok");
        assert_eq!(value["command"], "backlog.plan");
        assert_eq!(value["result"]["issues"][0], "ENG-10142");
    }

    #[test]
    fn render_json_error_includes_code_message_and_context() {
        let error = anyhow!("inner failure")
            .context("`--request` is required when `--no-interactive` is used");
        let encoded = render_json_error("backlog.plan", &error);
        let value: Value = serde_json::from_str(&encoded).expect("payload should parse");

        assert_eq!(value["status"], "error");
        assert_eq!(value["command"], "backlog.plan");
        assert_eq!(value["error"]["code"], "invalid_input");
        assert_eq!(
            value["error"]["message"],
            "`--request` is required when `--no-interactive` is used"
        );
        assert_eq!(value["error"]["context"][0], "inner failure");
    }

    #[test]
    fn render_json_clap_error_extracts_message_and_usage_context() {
        let error = ClapConflictArgs::command()
            .try_get_matches_from(["meta", "--json", "--render-once"])
            .expect_err("conflicting arguments should fail");
        let encoded = render_json_clap_error("agents.listen", &error);
        let value: Value = serde_json::from_str(&encoded).expect("payload should parse");

        assert_eq!(value["status"], "error");
        assert_eq!(value["command"], "agents.listen");
        assert_eq!(value["error"]["code"], "invalid_input");
        assert_eq!(
            value["error"]["message"],
            "the argument '--json' cannot be used with '--render-once'"
        );
        assert_eq!(value["error"]["context"][0], "Usage: meta --json");
    }
}

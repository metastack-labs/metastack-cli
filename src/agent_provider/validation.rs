use std::collections::HashMap;
use std::env;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};

#[derive(Clone, Copy)]
pub(crate) struct FlagSupport {
    emitted_flag: &'static str,
    accepted_patterns: &'static [&'static str],
}

impl FlagSupport {
    pub(crate) const fn new(
        emitted_flag: &'static str,
        accepted_patterns: &'static [&'static str],
    ) -> Self {
        Self {
            emitted_flag,
            accepted_patterns,
        }
    }
}

static HELP_OUTPUT_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

/// Validates that the installed CLI tool advertises the flags we intend to emit.
///
/// Checks each `(required, scope, flags)` tuple: when `required` is true, runs the tool's help
/// output and verifies every flag's accepted patterns appear. Returns an error describing the
/// first missing flag.
pub(crate) fn validate_help_surface(
    command: &str,
    help_args: &[&str],
    checks: &[(bool, &str, &[FlagSupport])],
) -> Result<()> {
    if checks.iter().all(|(required, _, _)| !*required) {
        return Ok(());
    }

    let help_output = cached_help_output(command, help_args)?;
    for (required, scope, flags) in checks.iter().copied() {
        if !required {
            continue;
        }
        for flag in flags {
            if flag
                .accepted_patterns
                .iter()
                .all(|pattern| !help_output.contains(*pattern))
            {
                bail!(
                    "installed `{}` {} does not advertise emitted flag `{}`; checked `{}`",
                    command,
                    scope,
                    flag.emitted_flag,
                    std::iter::once(command)
                        .chain(help_args.iter().copied())
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        }
    }

    Ok(())
}

fn cached_help_output(command: &str, help_args: &[&str]) -> Result<String> {
    let path_key = env::var("PATH").unwrap_or_default();
    let cache_key = std::iter::once(command)
        .chain(help_args.iter().copied())
        .chain(std::iter::once(path_key.as_str()))
        .collect::<Vec<_>>()
        .join("\u{0}");
    let cache = HELP_OUTPUT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    {
        let cache_guard = cache
            .lock()
            .map_err(|_| anyhow::anyhow!("built-in CLI help cache lock is poisoned"))?;
        if let Some(output) = cache_guard.get(&cache_key) {
            return Ok(output.clone());
        }
    }

    let output = Command::new(command)
        .args(help_args)
        .output()
        .with_context(|| {
            format!(
                "failed to run `{}` while validating built-in CLI flags",
                std::iter::once(command)
                    .chain(help_args.iter().copied())
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })?;
    if !output.status.success() {
        bail!(
            "`{}` failed while validating built-in CLI flags: {}",
            std::iter::once(command)
                .chain(help_args.iter().copied())
                .collect::<Vec<_>>()
                .join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let help_output = String::from_utf8_lossy(&output.stdout).to_string();
    let mut cache_guard = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("built-in CLI help cache lock is poisoned"))?;
    cache_guard.insert(cache_key, help_output.clone());
    Ok(help_output)
}

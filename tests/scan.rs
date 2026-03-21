#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[cfg(unix)]
#[test]
fn scan_runs_configured_agent_and_refreshes_repository_context_files() -> Result<(), Box<dyn Error>>
{
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let output_dir = temp.path().join("agent-output");
    let stub_path = temp.path().join("scan-agent");

    fs::create_dir_all(repo_root.join("src"))?;
    fs::create_dir_all(&output_dir)?;
    fs::write(
        repo_root.join("Cargo.toml"),
        r#"[package]
name = "demo-cli"
version = "0.1.0"
edition = "2024"

[dependencies]
clap = "4"
"#,
    )?;
    fs::write(repo_root.join("README.md"), "# Demo CLI\n")?;
    fs::write(repo_root.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(
        &config_path,
        format!(
            r#"[onboarding]
completed = true

[agents]
default_agent = "scan-stub"

[agents.commands.scan-stub]
command = "{}"
args = ["{{payload}}"]
transport = "arg"
"#,
            stub_path.display()
        ),
    )?;
    fs::write(
        &stub_path,
        r#"#!/bin/sh
echo "RAW AGENT LOG: starting scan"
echo "RAW AGENT STDERR: token-by-token noise" >&2
printf '%s' "$PWD" > "$TEST_OUTPUT_DIR/cwd.txt"
printf '%s' "$1" > "$TEST_OUTPUT_DIR/prompt.txt"
printf '%s' "$METASTACK_AGENT_PROMPT" > "$TEST_OUTPUT_DIR/agent-prompt.txt"
printf '%s' "$METASTACK_AGENT_PROVIDER_SOURCE" > "$TEST_OUTPUT_DIR/provider-source.txt"
printf '%s' "$METASTACK_AGENT_ROUTE_KEY" > "$TEST_OUTPUT_DIR/route-key.txt"
printf '%s' "$METASTACK_SCAN_FACT_BASE" > "$TEST_OUTPUT_DIR/fact-base.txt"
printf '%s' "$METASTACK_SCAN_DOCUMENTS" > "$TEST_OUTPUT_DIR/documents.txt"
mkdir -p .metastack/codebase
for pair in \
  "ARCHITECTURE.md:# Architecture" \
  "CONCERNS.md:# Codebase Concerns" \
  "CONVENTIONS.md:# Coding Conventions" \
  "INTEGRATIONS.md:# External Integrations" \
  "STACK.md:# Technology Stack" \
  "STRUCTURE.md:# Codebase Structure" \
  "TESTING.md:# Testing Patterns"
do
  file="${pair%%:*}"
  header="${pair#*:}"
  printf '%s\n' "$header" > ".metastack/codebase/$file"
done
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .arg("scan")
        .assert()
        .success()
        .stdout(predicate::str::contains("Codebase scan completed"))
        .stdout(predicate::str::contains("Steps:"))
        .stdout(predicate::str::contains("Files:"))
        .stdout(predicate::str::contains(".metastack/codebase/CONCERNS.md"))
        .stdout(predicate::str::contains(
            ".metastack/codebase/INTEGRATIONS.md",
        ))
        .stdout(predicate::str::contains("RAW AGENT LOG: starting scan").not())
        .stdout(predicate::str::contains("RAW AGENT STDERR: token-by-token noise").not());

    let json_assert = cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args(["scan", "--json"])
        .assert()
        .success();

    let payload: serde_json::Value = serde_json::from_slice(&json_assert.get_output().stdout)?;
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["command"], "context.scan");
    assert_eq!(payload["result"]["agent"], "scan-stub");
    assert_eq!(
        payload["result"]["log_path"],
        ".metastack/agents/sessions/scan.log"
    );
    assert!(
        payload["result"]["written_files"]
            .as_array()
            .is_some_and(|files| files
                .iter()
                .any(|path| path == ".metastack/codebase/SCAN.md"))
    );

    let scan = fs::read_to_string(repo_root.join(".metastack/codebase/SCAN.md"))?;
    let architecture = fs::read_to_string(repo_root.join(".metastack/codebase/ARCHITECTURE.md"))?;
    let concerns = fs::read_to_string(repo_root.join(".metastack/codebase/CONCERNS.md"))?;
    let conventions = fs::read_to_string(repo_root.join(".metastack/codebase/CONVENTIONS.md"))?;
    let integrations = fs::read_to_string(repo_root.join(".metastack/codebase/INTEGRATIONS.md"))?;
    let stack = fs::read_to_string(repo_root.join(".metastack/codebase/STACK.md"))?;
    let structure = fs::read_to_string(repo_root.join(".metastack/codebase/STRUCTURE.md"))?;
    let testing = fs::read_to_string(repo_root.join(".metastack/codebase/TESTING.md"))?;
    let codebase_entries = fs::read_dir(repo_root.join(".metastack/codebase"))?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().to_string()))
        .collect::<Result<Vec<_>, _>>()?;

    assert!(scan.contains("demo-cli"));
    assert!(scan.contains("Manual directory sweep used as the fact base for the scan agent."));
    assert_eq!(architecture.trim(), "# Architecture");
    assert_eq!(concerns.trim(), "# Codebase Concerns");
    assert_eq!(conventions.trim(), "# Coding Conventions");
    assert_eq!(integrations.trim(), "# External Integrations");
    assert_eq!(stack.trim(), "# Technology Stack");
    assert_eq!(structure.trim(), "# Codebase Structure");
    assert_eq!(testing.trim(), "# Testing Patterns");
    assert!(codebase_entries.iter().any(|entry| entry == "SCAN.md"));
    assert!(
        codebase_entries
            .iter()
            .any(|entry| entry == "ARCHITECTURE.md")
    );
    assert!(codebase_entries.iter().any(|entry| entry == "CONCERNS.md"));
    assert!(
        codebase_entries
            .iter()
            .any(|entry| entry == "CONVENTIONS.md")
    );
    assert!(
        codebase_entries
            .iter()
            .any(|entry| entry == "INTEGRATIONS.md")
    );
    assert!(codebase_entries.iter().any(|entry| entry == "STACK.md"));
    assert!(codebase_entries.iter().any(|entry| entry == "STRUCTURE.md"));
    assert!(codebase_entries.iter().any(|entry| entry == "TESTING.md"));
    assert!(!codebase_entries.iter().any(|entry| entry == "overview.md"));
    assert!(!codebase_entries.iter().any(|entry| entry == "stack.md"));
    assert!(!codebase_entries.iter().any(|entry| entry == "details.md"));
    assert_eq!(
        fs::read_to_string(output_dir.join("cwd.txt"))?,
        repo_root.canonicalize()?.display().to_string()
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("fact-base.txt"))?,
        ".metastack/codebase/SCAN.md"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("provider-source.txt"))?,
        "explicit_override"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("route-key.txt"))?,
        "context.scan"
    );
    let prompt = fs::read_to_string(output_dir.join("agent-prompt.txt"))?;
    assert!(prompt.contains("Target repository:"));
    assert!(prompt.contains("Scan only the target repository rooted above."));
    assert!(prompt.contains("Default scope: the full repository rooted at"));
    assert!(!prompt.contains("MetaStack CLI"));
    assert!(fs::read_to_string(output_dir.join("documents.txt"))?.contains("ARCHITECTURE.md"));
    assert!(fs::read_to_string(output_dir.join("documents.txt"))?.contains("INTEGRATIONS.md"));
    let scan_log = fs::read_to_string(repo_root.join(".metastack/agents/sessions/scan.log"))?;
    assert!(scan_log.contains("RAW AGENT LOG: starting scan"));
    assert!(scan_log.contains("RAW AGENT STDERR: token-by-token noise"));
    assert!(scan_log.contains("Resolved provider: scan-stub"));
    assert!(scan_log.contains("Resolved route key: context.scan"));
    assert!(scan_log.contains("Provider source: explicit_override"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn scan_failure_hides_raw_agent_output_and_reports_log_path() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("scan-agent");

    fs::create_dir_all(repo_root.join("src"))?;
    fs::write(
        repo_root.join("Cargo.toml"),
        r#"[package]
name = "demo-cli"
version = "0.1.0"
edition = "2024"

[dependencies]
clap = "4"
"#,
    )?;
    fs::write(repo_root.join("README.md"), "# Demo CLI\n")?;
    fs::write(repo_root.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(
        &config_path,
        format!(
            r#"[onboarding]
completed = true

[agents]
default_agent = "scan-stub"

[agents.commands.scan-stub]
command = "{}"
args = ["{{payload}}"]
transport = "arg"
"#,
            stub_path.display()
        ),
    )?;
    fs::write(
        &stub_path,
        r#"#!/bin/sh
echo "RAW AGENT LOG: failing scan"
echo "RAW AGENT STDERR: failure noise" >&2
exit 7
"#,
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .arg("scan")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "full agent output was saved to `.metastack/agents/sessions/scan.log`",
        ))
        .stderr(predicate::str::contains("RAW AGENT LOG: failing scan").not())
        .stderr(predicate::str::contains("RAW AGENT STDERR: failure noise").not());

    let scan_log = fs::read_to_string(repo_root.join(".metastack/agents/sessions/scan.log"))?;
    assert!(scan_log.contains("RAW AGENT LOG: failing scan"));
    assert!(scan_log.contains("RAW AGENT STDERR: failure noise"));

    Ok(())
}

#![allow(dead_code, unused_imports)]

include!("support/common.rs");

use metastack_cli::branding;

fn write_onboarded_config(
    config_path: &Path,
    config: impl AsRef<str>,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        config_path,
        format!(
            "{}\n[onboarding]\ncompleted = true\n",
            config.as_ref().trim_end()
        ),
    )?;
    Ok(())
}

#[test]
fn context_show_displays_instructions_rules_and_repo_map() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(repo_root.join("src"))?;
    write_onboarded_config(&config_path, "")?;
    fs::create_dir_all(repo_root.join("instructions"))?;
    fs::write(
        repo_root.join("Cargo.toml"),
        r#"[package]
name = "context-demo"
version = "0.1.0"
edition = "2024"
"#,
    )?;
    fs::write(repo_root.join("README.md"), "# Context Demo\n")?;
    fs::write(repo_root.join("src/main.rs"), "fn main() {}\n")?;
    fs::write(
        repo_root.join("AGENTS.md"),
        "# AGENTS\nFollow repo rules.\n",
    )?;
    fs::write(
        repo_root.join("WORKFLOW.md"),
        "# WORKFLOW\nKeep prompts deterministic.\n",
    )?;
    fs::write(
        repo_root.join("instructions/listen.md"),
        "# Listener Instructions\nKeep the workpad current.\n",
    )?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  },
  "listen": {
    "instructions_path": "instructions/listen.md"
  }
}
"#,
    )?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "context",
            "show",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Effective Context"))
        .stdout(predicate::str::contains("Built-in Workflow Contract"))
        .stdout(predicate::str::contains("Repository Scope"))
        .stdout(predicate::str::contains("Repo Overlay Sources"))
        .stdout(predicate::str::contains("Repo Overlay Contents"))
        .stdout(predicate::str::contains("Repo-Scoped Instructions"))
        .stdout(predicate::str::contains("Listener Instructions"))
        .stdout(predicate::str::contains("AGENTS"))
        .stdout(predicate::str::contains("WORKFLOW"))
        .stdout(predicate::str::contains("Repo Map"))
        .stdout(predicate::str::contains("src/main.rs"));

    Ok(())
}

#[test]
fn context_show_works_without_repo_overlay_files() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(repo_root.join("src"))?;
    write_onboarded_config(&config_path, "")?;
    fs::write(
        repo_root.join("Cargo.toml"),
        r#"[package]
name = "context-demo"
version = "0.1.0"
edition = "2024"
"#,
    )?;
    fs::write(repo_root.join("README.md"), "# Context Demo\n")?;
    fs::write(repo_root.join("src/main.rs"), "fn main() {}\n")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  }
}
"#,
    )?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "context",
            "show",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Built-in Workflow Contract"))
        .stdout(predicate::str::contains("Repo Overlay Sources"))
        .stdout(predicate::str::contains("No repo overlay files were found"))
        .stdout(predicate::str::contains("Repo-Scoped Instructions"))
        .stdout(predicate::str::contains(
            "No repo-scoped instructions file is configured",
        ));

    Ok(())
}

#[test]
fn context_map_renders_repo_summary() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(repo_root.join("src"))?;
    write_onboarded_config(&config_path, "")?;
    fs::write(
        repo_root.join("Cargo.toml"),
        r#"[package]
name = "map-demo"
version = "0.1.0"
edition = "2024"
"#,
    )?;
    fs::write(repo_root.join("README.md"), "# Map Demo\n")?;
    fs::write(repo_root.join("src/main.rs"), "fn main() {}\n")?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "context",
            "map",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Repo Map"))
        .stdout(predicate::str::contains("Candidate entry points"))
        .stdout(predicate::str::contains("src/main.rs"));

    Ok(())
}

#[test]
fn context_doctor_reports_missing_inputs() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path, "")?;
    fs::write(repo_root.join("README.md"), "# Missing Context\n")?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "context",
            "doctor",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!(
            "Missing `{}/meta.json`",
            branding::PROJECT_DIR
        )))
        .stderr(predicate::str::contains(
            "No repo overlay files were found; relying on the injected workflow contract",
        ))
        .stderr(predicate::str::contains("Missing codebase context files"));

    Ok(())
}

#[test]
fn context_doctor_succeeds_without_repo_overlay_files_when_required_inputs_exist()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let bin_dir = temp.path().join("bin");
    fs::create_dir_all(repo_root.join(format!("{}/codebase", branding::PROJECT_DIR)))?;
    fs::create_dir_all(&bin_dir)?;
    write_onboarded_config(&config_path, "")?;
    fs::write(repo_root.join("README.md"), "# Context OK\n")?;
    write_minimal_planning_context(
        &repo_root,
        r#"{
  "linear": {
    "team": "MET"
  }
}
"#,
    )?;
    for file in [
        "SCAN.md",
        "ARCHITECTURE.md",
        "CONCERNS.md",
        "CONVENTIONS.md",
        "INTEGRATIONS.md",
        "STACK.md",
        "STRUCTURE.md",
        "TESTING.md",
    ] {
        fs::write(
            repo_root
                .join(format!("{}/codebase", branding::PROJECT_DIR))
                .join(file),
            format!("# {file}\n"),
        )?;
    }

    let stub_path = bin_dir.join("codex");
    fs::write(&stub_path, "#!/bin/sh\nexit 0\n")?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;
    let current_path = std::env::var("PATH")?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", format!("{}:{}", bin_dir.display(), current_path))
        .args([
            "context",
            "doctor",
            "--root",
            repo_root.to_string_lossy().as_ref(),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Status: ok"))
        .stdout(predicate::str::contains(
            "No repo overlay files were found; relying on the injected workflow contract.",
        ));

    Ok(())
}

#[cfg(unix)]
#[test]
fn context_reload_refreshes_repository_context_files() -> Result<(), Box<dyn Error>> {
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
name = "reload-demo"
version = "0.1.0"
edition = "2024"

[dependencies]
clap = "4"
"#,
    )?;
    fs::write(repo_root.join("README.md"), "# Reload Demo\n")?;
    fs::write(repo_root.join("src/main.rs"), "fn main() {}\n")?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
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
        format!(
            "#!/bin/sh\n\
printf '%s' \"$METASTACK_AGENT_PROVIDER_SOURCE\" > \"$TEST_OUTPUT_DIR/provider-source.txt\"\n\
printf '%s' \"$METASTACK_AGENT_ROUTE_KEY\" > \"$TEST_OUTPUT_DIR/route-key.txt\"\n\
mkdir -p {0}/codebase\n\
for pair in \\\n\
  \"ARCHITECTURE.md:# Architecture\" \\\n\
  \"CONCERNS.md:# Codebase Concerns\" \\\n\
  \"CONVENTIONS.md:# Coding Conventions\" \\\n\
  \"INTEGRATIONS.md:# External Integrations\" \\\n\
  \"STACK.md:# Technology Stack\" \\\n\
  \"STRUCTURE.md:# Codebase Structure\" \\\n\
  \"TESTING.md:# Testing Patterns\"\n\
do\n\
  file=\"${{pair%%:*}}\"\n\
  header=\"${{pair#*:}}\"\n\
  printf '%s\\n' \"$header\" > \"{0}/codebase/$file\"\n\
done\n",
            branding::PROJECT_DIR
        ),
    )?;
    let mut permissions = fs::metadata(&stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&stub_path, permissions)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "context",
            "reload",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Codebase scan completed"))
        .stdout(predicate::str::contains(format!(
            "{}/codebase/STRUCTURE.md",
            branding::PROJECT_DIR
        )));

    assert!(
        repo_root
            .join(format!("{}/codebase/SCAN.md", branding::PROJECT_DIR))
            .is_file()
    );
    assert_eq!(
        fs::read_to_string(repo_root.join(format!(
            "{}/codebase/ARCHITECTURE.md",
            branding::PROJECT_DIR
        )))?
        .trim(),
        "# Architecture"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("provider-source.txt"))?,
        "explicit_override"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("route-key.txt"))?,
        "context.reload"
    );
    let scan_log = fs::read_to_string(repo_root.join(format!(
        "{}/agents/sessions/scan.log",
        branding::PROJECT_DIR
    )))?;
    assert!(scan_log.contains("Resolved provider: scan-stub"));
    assert!(scan_log.contains("Resolved route key: context.reload"));
    assert!(scan_log.contains("Provider source: explicit_override"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn context_scan_and_reload_can_use_different_route_specific_agents() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let output_dir = temp.path().join("agent-output");
    let scan_stub_path = temp.path().join("scan-agent");
    let reload_stub_path = temp.path().join("reload-agent");

    fs::create_dir_all(repo_root.join("src"))?;
    fs::create_dir_all(&output_dir)?;
    fs::write(
        repo_root.join("Cargo.toml"),
        r#"[package]
name = "route-demo"
version = "0.1.0"
edition = "2024"

[dependencies]
clap = "4"
"#,
    )?;
    fs::write(repo_root.join("README.md"), "# Route Demo\n")?;
    fs::write(repo_root.join("src/main.rs"), "fn main() {}\n")?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "codex"

[agents.routing.commands."context.scan"]
provider = "scan-stub"

[agents.routing.commands."context.reload"]
provider = "reload-stub"

[agents.commands.scan-stub]
command = "{}"
args = ["{{payload}}"]
transport = "arg"

[agents.commands.reload-stub]
command = "{}"
args = ["{{payload}}"]
transport = "arg"
"#,
            scan_stub_path.display(),
            reload_stub_path.display()
        ),
    )?;
    fs::write(
        &scan_stub_path,
        format!(
            "#!/bin/sh\n\
printf 'scan-stub' > \"$TEST_OUTPUT_DIR/scan-agent.txt\"\n\
printf '%s' \"$METASTACK_AGENT_ROUTE_KEY\" > \"$TEST_OUTPUT_DIR/scan-route.txt\"\n\
mkdir -p {0}/codebase\n\
for pair in \\\n\
  \"ARCHITECTURE.md:# Scan Architecture\" \\\n\
  \"CONCERNS.md:# Scan Concerns\" \\\n\
  \"CONVENTIONS.md:# Scan Conventions\" \\\n\
  \"INTEGRATIONS.md:# Scan Integrations\" \\\n\
  \"STACK.md:# Scan Stack\" \\\n\
  \"STRUCTURE.md:# Scan Structure\" \\\n\
  \"TESTING.md:# Scan Testing\"\n\
do\n\
  file=\"${{pair%%:*}}\"\n\
  header=\"${{pair#*:}}\"\n\
  printf '%s\\n' \"$header\" > \"{0}/codebase/$file\"\n\
done\n",
            branding::PROJECT_DIR
        ),
    )?;
    fs::write(
        &reload_stub_path,
        format!(
            "#!/bin/sh\n\
printf 'reload-stub' > \"$TEST_OUTPUT_DIR/reload-agent.txt\"\n\
printf '%s' \"$METASTACK_AGENT_ROUTE_KEY\" > \"$TEST_OUTPUT_DIR/reload-route.txt\"\n\
mkdir -p {0}/codebase\n\
for pair in \\\n\
  \"ARCHITECTURE.md:# Reload Architecture\" \\\n\
  \"CONCERNS.md:# Reload Concerns\" \\\n\
  \"CONVENTIONS.md:# Reload Conventions\" \\\n\
  \"INTEGRATIONS.md:# Reload Integrations\" \\\n\
  \"STACK.md:# Reload Stack\" \\\n\
  \"STRUCTURE.md:# Reload Structure\" \\\n\
  \"TESTING.md:# Reload Testing\"\n\
do\n\
  file=\"${{pair%%:*}}\"\n\
  header=\"${{pair#*:}}\"\n\
  printf '%s\\n' \"$header\" > \"{0}/codebase/$file\"\n\
done\n",
            branding::PROJECT_DIR
        ),
    )?;
    let mut permissions = fs::metadata(&scan_stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&scan_stub_path, permissions)?;
    let mut permissions = fs::metadata(&reload_stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&reload_stub_path, permissions)?;

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "context",
            "scan",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Codebase scan completed"));

    assert_eq!(
        fs::read_to_string(output_dir.join("scan-agent.txt"))?,
        "scan-stub"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("scan-route.txt"))?,
        "context.scan"
    );
    assert_eq!(
        fs::read_to_string(repo_root.join(format!(
            "{}/codebase/ARCHITECTURE.md",
            branding::PROJECT_DIR
        )))?
        .trim(),
        "# Scan Architecture"
    );

    cli()
        .current_dir(&repo_root)
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "context",
            "reload",
            "--root",
            repo_root.to_str().expect("temp path should be utf-8"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Codebase scan completed"));

    assert_eq!(
        fs::read_to_string(output_dir.join("reload-agent.txt"))?,
        "reload-stub"
    );
    assert_eq!(
        fs::read_to_string(output_dir.join("reload-route.txt"))?,
        "context.reload"
    );
    assert_eq!(
        fs::read_to_string(repo_root.join(format!(
            "{}/codebase/ARCHITECTURE.md",
            branding::PROJECT_DIR
        )))?
        .trim(),
        "# Reload Architecture"
    );

    Ok(())
}

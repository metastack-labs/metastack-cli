#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[cfg(unix)]
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

#[cfg(unix)]
fn write_spec_agent_stub(stub_path: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        stub_path,
        r#"#!/bin/sh
count_file="$TEST_OUTPUT_DIR/count.txt"
count=0
if [ -f "$count_file" ]; then
  count=$(cat "$count_file")
fi
count=$((count + 1))
printf '%s' "$count" > "$count_file"
printf '%s' "$METASTACK_AGENT_PROMPT" > "$TEST_OUTPUT_DIR/prompt-$count.txt"
printf '%s' "$METASTACK_AGENT_INSTRUCTIONS" > "$TEST_OUTPUT_DIR/instructions-$count.txt"

case "$METASTACK_AGENT_PROMPT" in
  *"Return JSON only using this exact shape"*)
    case "$METASTACK_AGENT_PROMPT" in
      *"Skip follow-up questions entirely"*)
        printf '%s' '{"questions":[]}'
        ;;
      *)
        printf '%s' '{"questions":["Who is the primary user for this workflow?","What should stay explicitly out of scope?"]}'
        ;;
    esac
    ;;
  *"Return an invalid SPEC missing required headings"*)
    printf '%s' '# OVERVIEW

This response is intentionally incomplete.

## GOALS

- Prove the validator rejects malformed SPEC output.

## FEATURES

- Skip one required heading on purpose.
'
    ;;
  *"Mode: Improve SPEC"*)
    printf '%s' '# OVERVIEW

Clarify the current repo-local contract and preserve the existing intent.

## GOALS

- Tighten the current SPEC lifecycle.
- Keep the command repo-local.

## FEATURES

- Revise `.metastack/SPEC.md` in place.
- Reuse the existing SPEC content when it is still valid.

## NON-GOALS

- No Linear mutations.
- No `.metastack/backlog/` packet writes.
'
    ;;
  *)
    printf '%s' '# OVERVIEW

Define a repo-local specification workflow for the active repository.

## GOALS

- Capture build intent through a staged flow.
- Persist only `.metastack/SPEC.md`.

## FEATURES

- Ask follow-up questions before drafting the SPEC.
- Keep generation scoped to the active repository root.

## NON-GOALS

- No Linear mutations.
- No backlog packet generation.
'
    ;;
esac
"#,
    )?;
    let mut permissions = fs::metadata(stub_path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(stub_path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn setup_spec_repo() -> Result<(tempfile::TempDir, PathBuf, PathBuf, PathBuf), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let stub_path = temp.path().join("spec-agent-stub");
    let output_dir = temp.path().join("agent-output");

    fs::create_dir_all(&repo_root)?;
    fs::create_dir_all(&output_dir)?;
    write_minimal_planning_context(&repo_root, "{}")?;
    write_spec_agent_stub(&stub_path)?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[agents]
default_agent = "spec-stub"

[agents.commands.spec-stub]
command = "{}"
transport = "stdin"
"#,
            stub_path.display()
        ),
    )?;

    Ok((temp, repo_root, config_path, output_dir))
}

#[cfg(unix)]
#[test]
fn spec_command_creates_repo_local_spec_on_first_run() -> Result<(), Box<dyn Error>> {
    let (_temp, repo_root, config_path, output_dir) = setup_spec_repo()?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "spec",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--no-interactive",
            "--request",
            "Add a repo-local SPEC workflow for this repository",
            "--answer",
            "CLI maintainers own the flow",
            "--answer",
            "Keep Linear and backlog packets untouched",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created repo-local spec"));

    let spec_path = repo_root.join(".metastack/SPEC.md");
    let spec = fs::read_to_string(&spec_path)?;
    assert!(spec.contains("OVERVIEW"));
    assert!(spec.contains("GOALS"));
    assert!(spec.contains("FEATURES"));
    assert!(spec.contains("NON-GOALS"));
    assert!(!repo_root.join(".metastack/backlog/MET-46").exists());
    assert!(!repo_root.join(".metastack/backlog/_TEMPLATE").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn spec_command_improves_existing_repo_local_spec() -> Result<(), Box<dyn Error>> {
    let (_temp, repo_root, config_path, output_dir) = setup_spec_repo()?;
    let spec_path = repo_root.join(".metastack/SPEC.md");
    fs::write(
        &spec_path,
        "# OVERVIEW\n\nOld overview.\n\n## GOALS\n\n- Old goal.\n\n## FEATURES\n\n- Old feature.\n\n## NON-GOALS\n\n- Old non-goal.\n",
    )?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "spec",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--no-interactive",
            "--request",
            "Improve the current SPEC so it is clearer about scope",
            "--answer",
            "Call out the repo-local contract explicitly",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated repo-local spec"));

    let spec = fs::read_to_string(&spec_path)?;
    assert!(spec.contains("Clarify the current repo-local contract"));
    assert!(spec.contains("Revise `.metastack/SPEC.md` in place."));
    let prompt = fs::read_to_string(output_dir.join("prompt-1.txt"))?;
    assert!(prompt.contains("Old overview."));
    assert!(!repo_root.join(".metastack/backlog").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn spec_command_rejects_generated_spec_missing_required_headings() -> Result<(), Box<dyn Error>> {
    let (_temp, repo_root, config_path, output_dir) = setup_spec_repo()?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "spec",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--no-interactive",
            "--request",
            "Return an invalid SPEC missing required headings",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "generated SPEC is missing the required `NON-GOALS` heading",
        ));

    assert!(!repo_root.join(".metastack/SPEC.md").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn spec_command_render_once_covers_major_tui_states() -> Result<(), Box<dyn Error>> {
    let (_temp, repo_root, config_path, output_dir) = setup_spec_repo()?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "spec",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--render-once",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "What should this repository build?",
        ));

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "spec",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--render-once",
            "--request",
            "Add a repo-local SPEC workflow",
            "--events",
            "enter",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Analyzing follow-up context"));

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "spec",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--render-once",
            "--request",
            "Add a repo-local SPEC workflow",
            "--events",
            "enter,wait,wait",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Create SPEC follow-up interview"))
        .stdout(predicate::str::contains(
            "Who is the primary user for this workflow?",
        ));

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "spec",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--render-once",
            "--request",
            "Skip follow-up questions entirely",
            "--events",
            "enter,wait",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Drafting repo-local SPEC"))
        .stdout(predicate::str::contains(
            "without touching Linear or backlog packets.",
        ))
        .stdout(predicate::str::contains("Create SPEC follow-up interview").not());

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .env("TEST_OUTPUT_DIR", &output_dir)
        .args([
            "backlog",
            "spec",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--render-once",
            "--request",
            "Add a repo-local SPEC workflow",
            "--answer",
            "CLI maintainers",
            "--answer",
            "No Linear mutations",
            "--events",
            "enter,wait,enter,wait",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("SPEC Preview"))
        .stdout(predicate::str::contains("NON-GOALS"));

    assert!(!repo_root.join(".metastack/SPEC.md").exists());
    Ok(())
}

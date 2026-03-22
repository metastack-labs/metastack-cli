#![allow(dead_code, unused_imports)]

include!("support/common.rs");

fn write_onboarded_config(config_path: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(config_path, "[onboarding]\ncompleted = true\n")?;
    Ok(())
}

fn write_backlog_issue(
    repo_root: &Path,
    identifier: &str,
    title: &str,
    extra_note: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    let issue_dir = repo_root.join(".metastack/backlog").join(identifier);
    fs::create_dir_all(&issue_dir)?;
    fs::write(
        issue_dir.join(".linear.json"),
        format!(
            r#"{{
  "issue_id": "issue-{identifier}",
  "identifier": "{identifier}",
  "title": "{title}",
  "url": "https://linear.app/issues/{identifier}",
  "team_key": "MET"
}}
"#
        ),
    )?;
    fs::write(
        issue_dir.join("index.md"),
        format!("# {title}\n\n{}\n", extra_note.unwrap_or("No extra notes.")),
    )?;
    fs::write(
        issue_dir.join("implementation.md"),
        extra_note.unwrap_or(""),
    )?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn release_local_plan_with_json_output() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    write_onboarded_config(&config_path)?;
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
    write_backlog_issue(&repo_root, "MET-10", "Lay release foundations", None)?;
    write_backlog_issue(
        &repo_root,
        "MET-11",
        "Ship release batching UX",
        Some("This rollout depends on MET-10 before the UX can ship."),
    )?;
    write_backlog_issue(&repo_root, "MET-12", "Add stretch automation", None)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "release",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--name",
            "sprint-1",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"sprint-1\""))
        .stdout(predicate::str::contains("\"included_count\""))
        .stdout(predicate::str::contains("\"deferred_count\""))
        .stdout(predicate::str::contains("\"identifier\": \"MET-10\""))
        .stdout(predicate::str::contains("\"identifier\": \"MET-11\""))
        .stdout(predicate::str::contains("\"identifier\": \"MET-12\""));

    let plan_md = fs::read_to_string(
        repo_root
            .join(".metastack/releases/sprint-1")
            .join("plan.md"),
    )?;
    let plan_json = fs::read_to_string(
        repo_root
            .join(".metastack/releases/sprint-1")
            .join("plan.json"),
    )?;

    assert!(plan_md.contains("# Release Plan: sprint-1"));
    assert!(plan_md.contains("MET-10"));
    assert!(plan_md.contains("MET-11"));
    assert!(plan_md.contains("MET-12"));
    assert!(plan_md.contains("## Cut Line"));
    assert!(plan_md.contains("## Recommended Ordering"));
    assert!(plan_json.contains("\"name\": \"sprint-1\""));
    assert!(plan_json.contains("\"total_items\": 3"));

    Ok(())
}

#[cfg(unix)]
#[test]
fn release_empty_backlog_produces_clear_message() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    write_onboarded_config(&config_path)?;
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
            "backlog",
            "release",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--name",
            "empty-sprint",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No backlog items found"));

    // Verify no release plan directory was created.
    assert!(!repo_root.join(".metastack/releases/empty-sprint").exists());

    Ok(())
}

#[cfg(unix)]
#[test]
fn release_single_item_succeeds_gracefully() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    write_onboarded_config(&config_path)?;
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
    write_backlog_issue(&repo_root, "MET-10", "Lay release foundations", None)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "backlog",
            "release",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--name",
            "single",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_items\": 1"))
        .stdout(predicate::str::contains("\"identifier\": \"MET-10\""));

    let plan_md = fs::read_to_string(repo_root.join(".metastack/releases/single").join("plan.md"))?;
    assert!(plan_md.contains("MET-10"));

    Ok(())
}

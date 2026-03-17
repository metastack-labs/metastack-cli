#![allow(dead_code, unused_imports)]

include!("support/common.rs");
use walkdir::WalkDir;

#[test]
fn top_level_help_lists_domain_families_aliases_and_examples() {
    cli()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("\n  backlog "))
        .stdout(predicate::str::contains("\n  agents "))
        .stdout(predicate::str::contains("\n  linear "))
        .stdout(predicate::str::contains("\n  context "))
        .stdout(predicate::str::contains("\n  runtime "))
        .stdout(predicate::str::contains("\n  dashboard "))
        .stdout(predicate::str::contains("\n  merge "))
        .stdout(predicate::str::contains("\n  cron "))
        .stdout(predicate::str::contains("\n  plan "))
        .stdout(predicate::str::contains("\n  config "))
        .stdout(predicate::str::contains("\n  scaffold ").not())
        .stdout(predicate::str::contains("\n  technical "))
        .stdout(predicate::str::contains("\n  sync "))
        .stdout(predicate::str::contains("\n  projects "))
        .stdout(predicate::str::contains("\n  issues "))
        .stdout(predicate::str::contains("\n  setup "))
        .stdout(predicate::str::contains(
            "Compatibility alias for `meta backlog plan`",
        ))
        .stdout(predicate::str::contains("engineer:"))
        .stdout(predicate::str::contains("team lead:"))
        .stdout(predicate::str::contains("ops operator:"));
}

#[test]
fn agents_help_lists_listen_and_workflows() {
    cli()
        .args(["agents", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\n  listen "))
        .stdout(predicate::str::contains("\n  workflows "));
}

#[test]
fn backlog_help_lists_tech_and_sync_commands() {
    cli()
        .args(["backlog", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\n  plan "))
        .stdout(predicate::str::contains("\n  tech "))
        .stdout(predicate::str::contains("\n  sync "))
        .stdout(predicate::str::contains("meta backlog tech MET-35"));
}

#[test]
fn legacy_config_alias_prints_runtime_hint() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "config",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .stderr(predicate::str::contains(
            "hint: `meta config` is a compatibility alias; prefer `meta runtime config`.",
        ));

    Ok(())
}

#[test]
fn runtime_config_help_describes_precedence_catalog_and_dry_run_diagnostics() {
    cli()
        .args(["runtime", "config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Resolution precedence for built-in provider/model/reasoning:",
        ))
        .stdout(predicate::str::contains(
            "1. explicit CLI overrides such as --agent/--provider, --model, and --reasoning",
        ))
        .stdout(predicate::str::contains("codex: gpt-5.4"))
        .stdout(predicate::str::contains("claude: sonnet, opus"))
        .stdout(predicate::str::contains(
            "meta agents workflows run ticket-implementation --root . --dry-run",
        ));
}

#[test]
fn runtime_setup_help_describes_repo_precedence_and_validation() {
    cli()
        .args(["runtime", "setup", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "explicit CLI override -> command route -> family route -> repo default -> install default",
        ))
        .stdout(predicate::str::contains(
            "Built-in provider/model/reasoning combinations are validated before they are saved.",
        ))
        .stdout(predicate::str::contains(
            "resolved provider, model, reasoning, route key, and config source",
        ));
}

#[test]
fn merge_help_lists_discovery_and_execution_flags() {
    cli()
        .args(["merge", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Inspect open pull requests"))
        .stdout(predicate::str::contains("--json"))
        .stdout(predicate::str::contains("--render-once"))
        .stdout(predicate::str::contains("--no-interactive"))
        .stdout(predicate::str::contains("--pull-request"))
        .stdout(predicate::str::contains(
            "meta merge --render-once --events",
        ))
        .stdout(predicate::str::contains("one-shot dashboard"));
}

#[test]
fn scaffold_creates_planning_layout_and_is_repeat_safe() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "setup",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"metastack_meta_path\""));

    assert!(repo_root.join(".metastack").is_dir());
    assert!(repo_root.join(".metastack/backlog").is_dir());
    assert!(repo_root.join(".metastack/backlog/README.md").is_file());
    assert!(repo_root.join(".metastack/backlog/_TEMPLATE").is_dir());
    let canonical_template_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tmp")
        .join("_TEMPLATE");
    for entry in WalkDir::new(&canonical_template_root) {
        let entry = entry?;
        let relative_path = entry.path().strip_prefix(&canonical_template_root)?;
        let actual_path = repo_root
            .join(".metastack/backlog/_TEMPLATE")
            .join(relative_path);

        if entry.file_type().is_dir() {
            assert!(
                actual_path.is_dir(),
                "missing directory {}",
                actual_path.display()
            );
        } else {
            assert!(
                actual_path.is_file(),
                "missing file {}",
                actual_path.display()
            );
            assert_eq!(
                fs::read_to_string(entry.path())?,
                fs::read_to_string(&actual_path)?,
                "seeded file diverged for {}",
                actual_path.display()
            );
        }
    }
    assert!(repo_root.join(".metastack/agents").is_dir());
    assert!(repo_root.join(".metastack/agents/README.md").is_file());
    assert!(repo_root.join(".metastack/agents/briefs").is_dir());
    assert!(repo_root.join(".metastack/agents/sessions").is_dir());
    assert!(repo_root.join(".metastack/codebase").is_dir());
    assert!(repo_root.join(".metastack/codebase/README.md").is_file());
    assert!(!repo_root.join(".metastack/codebase/SCAN.md").exists());
    assert!(repo_root.join(".metastack/workflows").is_dir());
    assert!(repo_root.join(".metastack/workflows/README.md").is_file());
    assert!(repo_root.join(".metastack/cron").is_dir());
    assert!(repo_root.join(".metastack/cron/README.md").is_file());
    assert!(repo_root.join(".metastack/meta.json").is_file());
    assert!(repo_root.join(".metastack/README.md").is_file());

    cli()
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "setup",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"metastack_meta_path\""));

    Ok(())
}

#![allow(dead_code, unused_imports)]

include!("support/common.rs");
use walkdir::WalkDir;

fn write_onboarded_config(config_path: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(config_path, "[onboarding]\ncompleted = true\n")?;
    Ok(())
}

fn assert_machine_parse_failure(args: &[&str], command: &str, expected_message: &str) {
    cli()
        .args(args)
        .assert()
        .code(2)
        .stdout(predicate::str::contains("\"status\": \"error\""))
        .stdout(predicate::str::contains(format!(
            "\"command\": \"{command}\""
        )))
        .stdout(predicate::str::contains("\"code\": \"invalid_input\""))
        .stdout(predicate::str::contains(expected_message))
        .stderr(predicate::str::is_empty());
}

#[test]
fn top_level_help_lists_primary_commands_and_examples() {
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
        .stdout(predicate::str::contains("\n  workspace "))
        .stdout(predicate::str::contains("\n  upgrade "))
        .stdout(predicate::str::contains("\n  help "))
        .stdout(predicate::str::contains("\n  cron ").not())
        .stdout(predicate::str::contains("\n  plan ").not())
        .stdout(predicate::str::contains("\n  config ").not())
        .stdout(predicate::str::contains("\n  scaffold ").not())
        .stdout(predicate::str::contains("\n  technical ").not())
        .stdout(predicate::str::contains("\n  sync ").not())
        .stdout(predicate::str::contains("\n  projects ").not())
        .stdout(predicate::str::contains("\n  issues ").not())
        .stdout(predicate::str::contains("\n  setup ").not())
        .stdout(predicate::str::contains("\n  listen ").not())
        .stdout(predicate::str::contains("\n  scan ").not())
        .stdout(predicate::str::contains("\n  workflows ").not())
        .stdout(predicate::str::contains("Compatibility alias for `meta backlog plan`").not())
        .stdout(predicate::str::contains("engineer:"))
        .stdout(predicate::str::contains("team lead:"))
        .stdout(predicate::str::contains("ops operator:"));
}

#[test]
fn bare_meta_renders_the_same_cleaned_top_level_help() {
    cli()
        .assert()
        .failure()
        .stderr(predicate::str::contains("\n  backlog "))
        .stderr(predicate::str::contains("\n  agents "))
        .stderr(predicate::str::contains("\n  linear "))
        .stderr(predicate::str::contains("\n  context "))
        .stderr(predicate::str::contains("\n  runtime "))
        .stderr(predicate::str::contains("\n  dashboard "))
        .stderr(predicate::str::contains("\n  merge "))
        .stderr(predicate::str::contains("\n  workspace "))
        .stderr(predicate::str::contains("\n  upgrade "))
        .stderr(predicate::str::contains("\n  help "))
        .stderr(predicate::str::contains("\n  plan ").not())
        .stderr(predicate::str::contains("\n  technical ").not())
        .stderr(predicate::str::contains("\n  listen ").not())
        .stderr(predicate::str::contains("\n  issues ").not())
        .stderr(predicate::str::contains("\n  projects ").not())
        .stderr(predicate::str::contains("\n  cron ").not())
        .stderr(predicate::str::contains("\n  scan ").not())
        .stderr(predicate::str::contains("\n  workflows ").not())
        .stderr(predicate::str::contains("\n  config ").not())
        .stderr(predicate::str::contains("\n  setup ").not())
        .stderr(predicate::str::contains("\n  sync ").not());
}

#[test]
fn explicit_meta_help_renders_the_same_cleaned_top_level_help() {
    cli()
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("\n  backlog "))
        .stdout(predicate::str::contains("\n  agents "))
        .stdout(predicate::str::contains("\n  linear "))
        .stdout(predicate::str::contains("\n  context "))
        .stdout(predicate::str::contains("\n  runtime "))
        .stdout(predicate::str::contains("\n  dashboard "))
        .stdout(predicate::str::contains("\n  merge "))
        .stdout(predicate::str::contains("\n  workspace "))
        .stdout(predicate::str::contains("\n  upgrade "))
        .stdout(predicate::str::contains("\n  help "))
        .stdout(predicate::str::contains("\n  plan ").not())
        .stdout(predicate::str::contains("\n  technical ").not())
        .stdout(predicate::str::contains("\n  listen ").not())
        .stdout(predicate::str::contains("\n  issues ").not())
        .stdout(predicate::str::contains("\n  projects ").not())
        .stdout(predicate::str::contains("\n  cron ").not())
        .stdout(predicate::str::contains("\n  scan ").not())
        .stdout(predicate::str::contains("\n  workflows ").not())
        .stdout(predicate::str::contains("\n  config ").not())
        .stdout(predicate::str::contains("\n  setup ").not())
        .stdout(predicate::str::contains("\n  sync ").not());
}

#[test]
fn workspace_help_lists_lifecycle_commands() {
    cli()
        .args(["workspace", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\n  list "))
        .stdout(predicate::str::contains("\n  clean "))
        .stdout(predicate::str::contains("\n  prune "))
        .stdout(predicate::str::contains(
            "meta workspace prune --dry-run --root .",
        ));
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
fn backlog_sync_push_help_describes_opt_in_description_updates() {
    cli()
        .args(["backlog", "sync", "push", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--update-description"))
        .stdout(predicate::str::contains("index.md"))
        .stdout(predicate::str::contains("stays local unless"));
}

#[test]
fn backlog_improve_help_points_to_issue_refine_for_single_issue_rewrites() {
    cli()
        .args(["backlog", "improve", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Use `meta backlog improve` for a repo-scoped backlog sweep across existing issues in one state.",
        ))
        .stdout(predicate::str::contains("meta linear issues refine"))
        .stdout(predicate::str::contains(
            "the primary goal is improving that issue's description rather than scanning the backlog",
        ));
}

#[test]
fn issue_refine_help_points_back_to_backlog_improve_for_sweeps() {
    cli()
        .args(["linear", "issues", "refine", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "focused description-quality pass with auditable refinement artifacts",
        ))
        .stdout(predicate::str::contains("meta backlog improve"))
        .stdout(predicate::str::contains(
            "parent-child structure opportunities",
        ));
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
        .stdout(predicate::str::contains("--replay-onboarding"))
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
fn agents_listen_machine_parse_failures_emit_json_errors() {
    cli()
        .args(["agents", "listen", "--json", "--render-once"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("\"status\": \"error\""))
        .stdout(predicate::str::contains("\"command\": \"agents.listen\""))
        .stdout(predicate::str::contains("\"code\": \"invalid_input\""))
        .stdout(predicate::str::contains(
            "the argument '--json' cannot be used with '--render-once'",
        ))
        .stderr(predicate::str::is_empty());
}

#[test]
fn agents_listen_once_and_render_once_conflict_emit_text_error() {
    cli()
        .args(["agents", "listen", "--once", "--render-once"])
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains(
            "the argument '--once' cannot be used with '--render-once'",
        ));
}

#[test]
fn backlog_sync_machine_parse_failures_emit_json_errors() {
    cli()
        .args(["backlog", "sync", "--json", "--render-once"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("\"status\": \"error\""))
        .stdout(predicate::str::contains("\"command\": \"backlog.sync\""))
        .stdout(predicate::str::contains("\"code\": \"invalid_input\""))
        .stdout(predicate::str::contains(
            "the argument '--json' cannot be used with '--render-once'",
        ))
        .stderr(predicate::str::is_empty());
}

#[test]
fn backlog_sync_non_interactive_and_render_once_conflict_emits_json_error() {
    assert_machine_parse_failure(
        &["backlog", "sync", "--no-interactive", "--render-once"],
        "backlog.sync",
        "the argument '--no-interactive' cannot be used with '--render-once'",
    );
}

#[test]
fn runtime_config_machine_parse_failures_emit_json_errors() {
    assert_machine_parse_failure(
        &["runtime", "config", "--json", "--render-once"],
        "runtime.config",
        "the argument '--json' cannot be used with '--render-once'",
    );
}

#[test]
fn runtime_setup_machine_parse_failures_emit_json_errors() {
    assert_machine_parse_failure(
        &["runtime", "setup", "--json", "--render-once"],
        "runtime.setup",
        "the argument '--json' cannot be used with '--render-once'",
    );
}

#[test]
fn linear_issues_list_machine_parse_failures_emit_json_errors() {
    assert_machine_parse_failure(
        &["linear", "issues", "list", "--json", "--render-once"],
        "linear.issues.list",
        "the argument '--json' cannot be used with '--render-once'",
    );
}

#[test]
fn linear_issues_create_machine_parse_failures_emit_json_errors() {
    assert_machine_parse_failure(
        &[
            "linear",
            "issues",
            "create",
            "--no-interactive",
            "--render-once",
        ],
        "linear.issues.create",
        "the argument '--no-interactive' cannot be used with '--render-once'",
    );
}

#[test]
fn runtime_cron_init_machine_parse_failures_emit_json_errors() {
    assert_machine_parse_failure(
        &[
            "runtime",
            "cron",
            "init",
            "--no-interactive",
            "--render-once",
        ],
        "runtime.cron.init",
        "the argument '--no-interactive' cannot be used with '--render-once'",
    );
}

#[test]
fn scaffold_creates_planning_layout_and_is_repeat_safe() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    fs::create_dir_all(&repo_root)?;
    write_onboarded_config(&config_path)?;

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
        .join("src")
        .join("artifacts")
        .join("BACKLOG_TEMPLATE");
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

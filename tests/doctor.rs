#![allow(dead_code, unused_imports)]

include!("support/common.rs");

#[test]
fn doctor_reports_pass_for_tools_on_path() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    // The test environment has `git` on PATH; doctor should pass that check.
    // Linear API key is not set, so that check will fail, making exit code 1.
    let output = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args(["doctor"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output)?;
    assert!(
        stdout.contains("[pass]"),
        "expected at least one pass check"
    );
    assert!(stdout.contains("git"), "expected git check in output");
    assert!(
        stdout.contains("Environment health check"),
        "expected header in output"
    );
    Ok(())
}

#[test]
fn doctor_json_outputs_structured_checks() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    let output = cli()
        .env("METASTACK_CONFIG", &config_path)
        .args(["doctor", "--json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output)?;
    let parsed: serde_json::Value = serde_json::from_str(&stdout)?;
    let checks = parsed["checks"]
        .as_array()
        .expect("checks should be an array");
    assert!(!checks.is_empty(), "expected at least one check");

    // Every check must have name, status, message
    for check in checks {
        assert!(check["name"].is_string(), "check should have a name");
        assert!(check["status"].is_string(), "check should have a status");
        assert!(check["message"].is_string(), "check should have a message");
    }

    Ok(())
}

#[test]
fn doctor_fails_when_required_tool_missing() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    fs::write(&config_path, "[onboarding]\ncompleted = true\n")?;

    // Provide a PATH with only a stub directory to ensure `git` and `gh` are missing.
    let stub_dir = temp.path().join("empty-bin");
    fs::create_dir_all(&stub_dir)?;

    let output = cli()
        .env("METASTACK_CONFIG", &config_path)
        .env("PATH", &stub_dir)
        .args(["doctor", "--json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output)?;
    let parsed: serde_json::Value = serde_json::from_str(&stdout)?;
    let checks = parsed["checks"]
        .as_array()
        .expect("checks should be an array");

    // git check should fail
    let git_check = checks
        .iter()
        .find(|c| c["name"] == "git_on_path")
        .expect("should have git_on_path check");
    assert_eq!(git_check["status"], "fail");

    // expect check should warn (optional)
    let expect_check = checks
        .iter()
        .find(|c| c["name"] == "expect_on_path")
        .expect("should have expect_on_path check");
    assert_eq!(expect_check["status"], "warn");

    Ok(())
}

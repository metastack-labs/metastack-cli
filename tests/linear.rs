#![allow(dead_code, unused_imports)]

include!("support/common.rs");

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
fn issues_commands_require_auth_when_not_in_demo_mode() {
    let temp = tempdir().expect("tempdir should build");
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "").expect("config file should write");

    cli()
        .current_dir(temp.path())
        .env_remove("LINEAR_API_KEY")
        .env("METASTACK_CONFIG", &config_path)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("HOME")
        .args(["issues", "list"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("LINEAR_API_KEY")
                .or(predicate::str::contains("Linear profile")
                    .and(predicate::str::contains("is not configured"))),
        );
}

#[test]
fn linear_list_commands_work_against_a_mock_server() {
    let temp = tempdir().expect("tempdir should build");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    write_onboarded_config(&config_path, "").expect("config file should write");

    fs::create_dir_all(temp.path().join(".metastack")).expect("planning dir should build");
    fs::write(
        temp.path().join(".metastack/meta.json"),
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "MetaStack CLI"
  }
}
"#,
    )
    .expect("meta file should write");

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "token")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": "CLI platform work",
                        "url": "https://linear.app/projects/1",
                        "progress": 0.42,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "token")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-1",
                        "identifier": "MET-11",
                        "title": "CLI Scaffolding & Modules",
                        "description": "Ship scaffold and scan commands",
                        "url": "https://linear.app/issues/1",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }]
                }
            }
        }));
    });

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "projects",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "list",
            "--team",
            "MET",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MetaStack CLI"));

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "issues",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "list",
            "--team",
            "MET",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"identifier\": \"MET-11\""));

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "linear",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "issues",
            "list",
            "--team",
            "MET",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"identifier\": \"MET-11\""));
}

#[test]
fn linear_commands_can_read_auth_from_config_file() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let config_path = temp.path().join("metastack.toml");

    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"
team = "MET"
"#
        ),
    )?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-1",
                        "identifier": "MET-14",
                        "title": "Add Agent Support",
                        "description": "Allow local agent launch flows",
                        "url": "https://linear.app/issues/14",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }]
                }
            }
        }));
    });

    cli()
        .current_dir(temp.path())
        .env_remove("LINEAR_API_KEY")
        .env("METASTACK_CONFIG", &config_path)
        .args(["issues", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"identifier\": \"MET-14\""));

    Ok(())
}

#[cfg(unix)]
#[test]
fn issues_command_uses_repo_scoped_api_key_over_global_auth() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let right_server = MockServer::start();
    let right_api_url = right_server.url("/graphql");

    fs::create_dir_all(repo_root.join(".metastack"))?;
    let canonical_repo_root = fs::canonicalize(&repo_root)?;
    fs::write(
        repo_root.join(".metastack/meta.json"),
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "global-token"
api_url = "{right_api_url}"
team = "PER"

[linear.repo_auth."{}"]
api_key = "repo-token"
"#,
            canonical_repo_root.to_string_lossy()
        ),
    )?;

    let issues_mock = right_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-selected",
                        "identifier": "MET-210",
                        "title": "Repo auth issue",
                        "description": "Issue returned by repo-scoped auth",
                        "url": "https://linear.app/issues/MET-210",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "Repo Project"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });
    cli()
        .current_dir(&repo_root)
        .env_remove("LINEAR_API_KEY")
        .env_remove("LINEAR_API_URL")
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "issues",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "--api-url",
            &right_api_url,
            "list",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-210"));

    issues_mock.assert();
    Ok(())
}

#[cfg(unix)]
#[test]
fn projects_command_uses_repo_selected_profile_and_team_over_global_defaults()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let right_server = MockServer::start();
    let wrong_server = MockServer::start();
    let right_api_url = right_server.url("/graphql");
    let wrong_api_url = wrong_server.url("/graphql");

    fs::create_dir_all(repo_root.join(".metastack"))?;
    fs::write(
        repo_root.join(".metastack/meta.json"),
        r#"{
  "linear": {
    "profile": "work",
    "team": "MET"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "global-token"
api_url = "{wrong_api_url}"
team = "PER"
default_profile = "personal"

[linear.profiles.work]
api_key = "repo-token"
api_url = "{right_api_url}"
team = "MET"

[linear.profiles.personal]
api_key = "personal-token"
api_url = "{wrong_api_url}"
team = "PER"
"#
        ),
    )?;

    let projects_mock = right_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "Repo Project",
                        "description": "Project selected by the repo-scoped team",
                        "url": "https://linear.app/projects/1",
                        "progress": 0.42,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }, {
                        "id": "project-2",
                        "name": "Personal Project",
                        "description": "Project that should be filtered by the wrong global team",
                        "url": "https://linear.app/projects/2",
                        "progress": 0.11,
                        "teams": {
                            "nodes": [{
                                "id": "team-2",
                                "key": "PER",
                                "name": "Personal"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    cli()
        .current_dir(&repo_root)
        .env_remove("LINEAR_API_KEY")
        .env_remove("LINEAR_API_URL")
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "projects",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "list",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Repo Project"))
        .stdout(predicate::str::contains("Personal Project").not());

    projects_mock.assert();
    Ok(())
}

#[cfg(unix)]
#[test]
fn issues_command_uses_repo_selected_profile_and_project_over_global_defaults()
-> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let repo_root = temp.path().join("repo");
    let config_path = temp.path().join("metastack.toml");
    let right_server = MockServer::start();
    let wrong_server = MockServer::start();
    let right_api_url = right_server.url("/graphql");
    let wrong_api_url = wrong_server.url("/graphql");

    fs::create_dir_all(repo_root.join(".metastack"))?;
    fs::write(
        repo_root.join(".metastack/meta.json"),
        r#"{
  "linear": {
    "profile": "work",
    "team": "MET",
    "project_id": "project-1"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "global-token"
api_url = "{wrong_api_url}"
team = "PER"
default_profile = "personal"

[linear.profiles.work]
api_key = "repo-token"
api_url = "{right_api_url}"
team = "MET"

[linear.profiles.personal]
api_key = "personal-token"
api_url = "{wrong_api_url}"
team = "PER"
"#
        ),
    )?;

    let issues_mock = right_server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "repo-token")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-selected",
                        "identifier": "MET-110",
                        "title": "Repo selected issue",
                        "description": "Issue from the repo-selected project",
                        "url": "https://linear.app/issues/MET-110",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "Repo Project"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }, {
                        "id": "issue-wrong-project",
                        "identifier": "MET-111",
                        "title": "Wrong project issue",
                        "description": "Issue from a different project that should be filtered out",
                        "url": "https://linear.app/issues/MET-111",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-2",
                            "name": "Wrong Project"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }, {
                        "id": "issue-wrong-team",
                        "identifier": "PER-112",
                        "title": "Wrong team issue",
                        "description": "Issue from the conflicting global team",
                        "url": "https://linear.app/issues/PER-112",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "team": {
                            "id": "team-2",
                            "key": "PER",
                            "name": "Personal"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "Repo Project"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });

    cli()
        .current_dir(&repo_root)
        .env_remove("LINEAR_API_KEY")
        .env_remove("LINEAR_API_URL")
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "issues",
            "--root",
            repo_root.to_string_lossy().as_ref(),
            "list",
            "--json",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("MET-110"))
        .stdout(predicate::str::contains("MET-111").not())
        .stdout(predicate::str::contains("PER-112").not());

    issues_mock.assert();
    Ok(())
}

#[cfg(unix)]
#[test]
fn linear_issue_list_render_once_launches_issue_browser_filters() {
    let temp = tempdir().expect("tempdir should build");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    write_onboarded_config(&config_path, "").expect("config file should write");

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "token")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": "CLI platform work",
                        "url": "https://linear.app/projects/1",
                        "progress": 0.42,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "token")
            .body_includes("query Issues")
            .body_includes("estimate");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-1",
                        "identifier": "MET-11",
                        "title": "CLI Scaffolding & Modules",
                        "description": "Ship scaffold and scan commands",
                        "url": "https://linear.app/issues/1",
                        "priority": 2,
                        "estimate": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }, {
                        "id": "issue-2",
                        "identifier": "MET-12",
                        "title": "Add Tests",
                        "description": "Cover the new CLI modules and runtime proofs",
                        "url": "https://linear.app/issues/2",
                        "priority": 1,
                        "estimate": 5,
                        "updatedAt": "2026-03-14T16:05:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "issues",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "list",
            "--team",
            "MET",
            "--render-once",
            "--events",
            "tab,tab,down,down,enter",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Status [focus]"))
        .stdout(predicate::str::contains("Visible issues: 1/2"))
        .stdout(predicate::str::contains("MET-12"))
        .stdout(predicate::str::contains("MET-11").not());
}

#[test]
fn linear_issue_create_and_edit_work_against_a_mock_server() {
    let temp = tempdir().expect("tempdir should build");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    write_onboarded_config(&config_path, "").expect("config file should write");

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(json!({
            "data": {
                "teams": {
                    "nodes": [{
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack",
                        "states": {
                            "nodes": [
                                {
                                    "id": "state-backlog",
                                    "name": "Backlog",
                                    "type": "backlog"
                                },
                                {
                                    "id": "state-1",
                                    "name": "Todo",
                                    "type": "unstarted"
                                },
                                {
                                    "id": "state-2",
                                    "name": "In Progress",
                                    "type": "started"
                                }
                            ]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": "CLI platform work",
                        "url": "https://linear.app/projects/1",
                        "progress": 0.42,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": "CLI platform work",
                        "url": "https://linear.app/projects/1",
                        "progress": 0.42,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": "CLI platform work",
                        "url": "https://linear.app/projects/1",
                        "progress": 0.42,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-1",
                        "identifier": "MET-13",
                        "title": "Add docs",
                        "description": "Cover the new CLI flows",
                        "url": "https://linear.app/issues/13",
                        "priority": 1,
                        "updatedAt": "2026-03-14T16:10:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-1",
                        "identifier": "MET-11",
                        "title": "CLI Scaffolding & Modules",
                        "description": "Ship scaffold and scan commands",
                        "url": "https://linear.app/issues/11",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation UpdateIssue");
        then.status(200).json_body(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-1",
                        "identifier": "MET-11",
                        "title": "CLI Foundation",
                        "description": "Ship scaffold and scan commands",
                        "url": "https://linear.app/issues/11",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:20:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-2",
                            "name": "In Progress",
                            "type": "started"
                        }
                    }
                }
            }
        }));
    });

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .current_dir(temp.path())
        .args([
            "linear",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "issues",
            "create",
            "--no-interactive",
            "--team",
            "MET",
            "--title",
            "Add docs",
            "--description",
            "Cover the new CLI flows",
            "--project",
            "MetaStack CLI",
            "--state",
            "Todo",
            "--priority",
            "1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created issue: MET-13"));

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .current_dir(temp.path())
        .args([
            "linear",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "issues",
            "edit",
            "--no-interactive",
            "--issue",
            "MET-11",
            "--title",
            "CLI Foundation",
            "--state",
            "In Progress",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Updated issue: MET-11"));
}

#[test]
fn linear_issue_create_launches_interactive_form_by_default() {
    let temp = tempdir().expect("tempdir should build");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    write_onboarded_config(&config_path, "").expect("config file should write");

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(json!({
            "data": {
                "teams": {
                    "nodes": [{
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack",
                        "states": {
                            "nodes": [
                                {
                                    "id": "state-backlog",
                                    "name": "Backlog",
                                    "type": "backlog"
                                },
                                {
                                    "id": "state-1",
                                    "name": "Todo",
                                    "type": "unstarted"
                                },
                                {
                                    "id": "state-2",
                                    "name": "In Progress",
                                    "type": "started"
                                }
                            ]
                        }
                    }]
                }
            }
        }));
    });

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "linear",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "issues",
            "create",
            "--team",
            "MET",
            "--title",
            "Add docs",
            "--description",
            "Cover the new CLI flows",
            "--render-once",
            "--events",
            "tab,tab,right,down",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Create Linear Issue (MET)"))
        .stdout(predicate::str::contains("3. Status / Priority"))
        .stdout(predicate::str::contains("Priority [focus]"))
        .stdout(predicate::str::contains("Urgent (1)"));
}

#[test]
fn linear_issue_edit_launches_interactive_form_by_default() {
    let temp = tempdir().expect("tempdir should build");
    let config_path = temp.path().join("metastack.toml");
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    write_onboarded_config(&config_path, "").expect("config file should write");

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Issues");
        then.status(200).json_body(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-1",
                        "identifier": "MET-11",
                        "title": "CLI Scaffolding & Modules",
                        "description": "Ship scaffold and scan commands",
                        "url": "https://linear.app/issues/11",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(json!({
            "data": {
                "teams": {
                    "nodes": [{
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack",
                        "states": {
                            "nodes": [
                                {
                                    "id": "state-backlog",
                                    "name": "Backlog",
                                    "type": "backlog"
                                },
                                {
                                    "id": "state-1",
                                    "name": "Todo",
                                    "type": "unstarted"
                                },
                                {
                                    "id": "state-2",
                                    "name": "In Progress",
                                    "type": "started"
                                }
                            ]
                        }
                    }]
                }
            }
        }));
    });

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "linear",
            "--api-key",
            "token",
            "--api-url",
            &api_url,
            "issues",
            "edit",
            "--issue",
            "MET-11",
            "--render-once",
            "--events",
            "tab,tab,right,down",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Edit Linear Issue (MET-11)"))
        .stdout(predicate::str::contains("3. Status / Priority"))
        .stdout(predicate::str::contains("Priority [focus]"))
        .stdout(predicate::str::contains("Normal (3)"));
}

#[test]
fn linear_issue_create_requires_title_for_non_interactive_mode() {
    let temp = tempdir().expect("tempdir should build");
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "").expect("config file should write");

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args(["issues", "--api-key", "token", "create", "--no-interactive"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("`--title` is required"));
}

#[test]
fn dashboard_render_once_uses_ratatui_snapshot_output() {
    let temp = tempdir().expect("tempdir should build");
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "").expect("config file should write");

    cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "dashboard",
            "--api-key",
            "token",
            "--demo",
            "--render-once",
            "--events",
            "tab",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Description Preview [focus]"))
        .stdout(predicate::str::contains("MET-11"))
        .stdout(predicate::str::contains("Wheel scroll preview"));
}

#[test]
fn dashboard_linear_matches_legacy_dashboard_output() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let config_path = temp.path().join("metastack.toml");
    write_onboarded_config(&config_path, "")?;

    let legacy = cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "dashboard",
            "--api-key",
            "token",
            "--demo",
            "--render-once",
            "--events",
            "tab",
        ])
        .output()?;
    assert!(legacy.status.success());

    let preferred = cli()
        .current_dir(temp.path())
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "dashboard",
            "linear",
            "--api-key",
            "token",
            "--demo",
            "--render-once",
            "--events",
            "tab",
        ])
        .output()?;
    assert!(preferred.status.success());

    assert_eq!(
        String::from_utf8(legacy.stdout)?,
        String::from_utf8(preferred.stdout)?
    );
    Ok(())
}

#[test]
fn linear_issue_create_uses_repo_meta_defaults() -> Result<(), Box<dyn Error>> {
    let temp = tempdir()?;
    let server = MockServer::start();
    let api_url = server.url("/graphql");
    let config_path = temp.path().join("metastack.toml");

    fs::create_dir_all(temp.path().join(".metastack"))?;
    fs::write(
        temp.path().join(".metastack/meta.json"),
        r#"{
  "linear": {
    "team": "MET",
    "project_id": "project-1"
  }
}
"#,
    )?;
    write_onboarded_config(
        &config_path,
        format!(
            r#"[linear]
api_key = "token"
api_url = "{api_url}"
"#
        ),
    )?;

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Teams");
        then.status(200).json_body(json!({
            "data": {
                "teams": {
                    "nodes": [{
                        "id": "team-1",
                        "key": "MET",
                        "name": "Metastack",
                        "states": {
                            "nodes": [{
                                "id": "state-1",
                                "name": "Todo",
                                "type": "unstarted"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("query Projects");
        then.status(200).json_body(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": "CLI platform work",
                        "url": "https://linear.app/projects/1",
                        "progress": 0.42,
                        "teams": {
                            "nodes": [{
                                "id": "team-1",
                                "key": "MET",
                                "name": "Metastack"
                            }]
                        }
                    }]
                }
            }
        }));
    });

    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("mutation CreateIssue")
            .body_includes("\"projectId\":\"project-1\"")
            .body_includes("\"teamId\":\"team-1\"");
        then.status(200).json_body(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-1",
                        "identifier": "MET-31",
                        "title": "Use repo defaults",
                        "description": "Create issues with .metastack/meta.json defaults",
                        "url": "https://linear.app/issues/31",
                        "priority": 1,
                        "updatedAt": "2026-03-14T17:10:00Z",
                        "team": {
                            "id": "team-1",
                            "key": "MET",
                            "name": "Metastack"
                        },
                        "project": {
                            "id": "project-1",
                            "name": "MetaStack CLI"
                        },
                        "state": {
                            "id": "state-1",
                            "name": "Todo",
                            "type": "unstarted"
                        }
                    }
                }
            }
        }));
    });

    cli()
        .current_dir(temp.path())
        .env_remove("LINEAR_API_KEY")
        .env("METASTACK_CONFIG", &config_path)
        .args([
            "linear",
            "issues",
            "create",
            "--no-interactive",
            "--title",
            "Use repo defaults",
            "--description",
            "Create issues with .metastack/meta.json defaults",
            "--priority",
            "1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created issue: MET-31"));

    Ok(())
}

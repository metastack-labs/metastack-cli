use std::error::Error;
use std::fs;
use std::hash::{Hash, Hasher};
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command as ProcessCommand;
#[cfg(unix)]
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
#[cfg(unix)]
use std::sync::OnceLock;
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;

use assert_cmd::Command;
use httpmock::Method::{GET, POST};
use httpmock::MockServer;
use predicates::prelude::*;
use serde_json::json;
use tempfile::tempdir;

const TEST_ENV_REMOVALS: &[&str] = &[
    "LINEAR_API_KEY",
    "LINEAR_API_URL",
    "LINEAR_TEAM",
    "GITHUB_TOKEN",
    "GH_TOKEN",
    "GITHUB_API_URL",
    "METASTACK_AGENT_INSTRUCTIONS",
    "METASTACK_AGENT_MODEL",
    "METASTACK_AGENT_NAME",
    "METASTACK_AGENT_PROMPT",
    "METASTACK_AGENT_REASONING",
    "METASTACK_CONFIG",
    "METASTACK_LINEAR_ATTACHMENT_CONTEXT_PATH",
    "METASTACK_LINEAR_BACKLOG_ISSUE_IDENTIFIER",
    "METASTACK_LINEAR_BACKLOG_ISSUE_URL",
    "METASTACK_LINEAR_BACKLOG_PATH",
    "METASTACK_LINEAR_ISSUE_ID",
    "METASTACK_LINEAR_ISSUE_IDENTIFIER",
    "METASTACK_LINEAR_ISSUE_URL",
    "METASTACK_LINEAR_WORKPAD_COMMENT_ID",
    "METASTACK_LISTEN_UNATTENDED",
    "METASTACK_SOURCE_ROOT",
    "METASTACK_WORKTREE_PATH",
    "METASTACK_WORKSPACE_PATH",
    "XDG_CONFIG_HOME",
];

fn isolated_home_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "metastack-test-home-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("main")
    ));
    fs::create_dir_all(path.join(".config")).expect("test home directory should be creatable");
    path
}

fn test_command() -> Command {
    let meta_bin = std::env::var_os("CARGO_BIN_EXE_meta")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::current_exe().ok().and_then(|path| {
                let target_dir = path.parent()?.parent()?;
                let candidates = ["meta", "meta.exe"];
                candidates
                    .into_iter()
                    .map(|name| target_dir.join(name))
                    .find(|candidate| candidate.is_file())
            })
        })
        .expect("meta binary should build for tests");
    let mut command = Command::new(meta_bin);
    for key in TEST_ENV_REMOVALS {
        command.env_remove(key);
    }
    let home_dir = isolated_home_dir();
    command.env("HOME", &home_dir);
    command.env("XDG_CONFIG_HOME", home_dir.join(".config"));
    command
}

fn cli() -> Command {
    test_command()
}

fn meta() -> Command {
    test_command()
}

#[cfg(unix)]
fn listen_project_store_dir(
    config_path: &Path,
    repo_root: &Path,
    project_selector: Option<&str>,
) -> Result<PathBuf, Box<dyn Error>> {
    let source_root = listen_source_root(repo_root)?;
    let metastack_root = source_root.join(".metastack").canonicalize()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    metastack_root.display().to_string().hash(&mut hasher);
    listen_project_scope_key(project_selector, repo_root)?.hash(&mut hasher);
    let project_key = format!("{:016x}", hasher.finish());
    Ok(config_path
        .parent()
        .expect("config path should have a parent")
        .join("data")
        .join("listen")
        .join("projects")
        .join(project_key))
}

#[cfg(unix)]
fn listen_state_path(config_path: &Path, repo_root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    Ok(listen_project_store_dir(config_path, repo_root, None)?.join("session.json"))
}

#[cfg(unix)]
fn listen_log_path(
    config_path: &Path,
    repo_root: &Path,
    issue_identifier: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    Ok(listen_project_store_dir(config_path, repo_root, None)?
        .join("logs")
        .join(format!("{issue_identifier}.log")))
}

#[cfg(unix)]
fn listen_project_scope_key(
    project_selector: Option<&str>,
    repo_root: &Path,
) -> Result<String, Box<dyn Error>> {
    let selector = match project_selector
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
    {
        Some(selector) => Some(selector),
        None => {
            let meta = fs::read_to_string(repo_root.join(".metastack/meta.json"))?;
            serde_json::from_str::<serde_json::Value>(&meta)?
                .get("linear")
                .and_then(|value| value.get("project_id"))
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
        }
    };

    Ok(match selector {
        Some(selector) => format!("project:{}", selector.to_ascii_lowercase()),
        None => "project:all".to_string(),
    })
}

#[cfg(unix)]
fn listen_source_root(repo_root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let common_dir = git_stdout(
        repo_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common_dir = PathBuf::from(common_dir);
    if common_dir.file_name().and_then(|value| value.to_str()) == Some(".git")
        && let Some(source_root) = common_dir.parent()
        && source_root.join(".metastack").is_dir()
    {
        return Ok(source_root.canonicalize()?);
    }

    Ok(repo_root.canonicalize()?)
}

#[cfg(unix)]
fn write_minimal_planning_context(
    repo_root: &Path,
    planning_meta: &str,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(repo_root.join(".metastack/codebase"))?;
    fs::write(repo_root.join(".metastack/meta.json"), planning_meta)?;
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
            repo_root.join(".metastack/codebase").join(file),
            format!("{file}\n"),
        )?;
    }
    Ok(())
}

#[cfg(unix)]
fn init_repo_with_origin(repo_root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let remote = repo_root
        .parent()
        .expect("repo should have a parent")
        .join("origin.git");

    let status = ProcessCommand::new("git")
        .args(["init", "--bare", remote.to_string_lossy().as_ref()])
        .status()?;
    assert!(status.success());

    let status = ProcessCommand::new("git")
        .args(["init", "-b", "main", repo_root.to_string_lossy().as_ref()])
        .status()?;
    assert!(status.success());

    configure_git_identity(repo_root)?;

    let repo = repo_root.to_string_lossy();
    for args in [
        vec![
            "-C",
            repo.as_ref(),
            "remote",
            "add",
            "origin",
            remote.to_string_lossy().as_ref(),
        ],
        vec!["-C", repo.as_ref(), "add", "."],
        vec!["-C", repo.as_ref(), "commit", "-m", "Initial commit"],
        vec!["-C", repo.as_ref(), "push", "-u", "origin", "main"],
        vec![
            "-C",
            remote.to_string_lossy().as_ref(),
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
    ] {
        let status = ProcessCommand::new("git").args(args).status()?;
        assert!(status.success());
    }

    Ok(remote)
}

#[cfg(unix)]
fn create_worktree_checkout(
    repo_root: &Path,
    branch: &str,
    worktree_name: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let worktree_root = repo_root
        .parent()
        .expect("repo should have a parent")
        .join(worktree_name);
    let status = ProcessCommand::new("git")
        .args([
            "-C",
            repo_root.to_string_lossy().as_ref(),
            "worktree",
            "add",
            "-b",
            branch,
            worktree_root.to_string_lossy().as_ref(),
            "main",
        ])
        .status()?;
    assert!(status.success());
    Ok(worktree_root)
}

#[cfg(unix)]
fn create_workspace_clone_checkout(
    repo_root: &Path,
    workspace_name: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let workspace_root = repo_root
        .parent()
        .expect("repo should have a parent")
        .join(workspace_name);
    let remote_url = git_stdout(repo_root, &["remote", "get-url", "origin"])?;
    let status = ProcessCommand::new("git")
        .args([
            "clone",
            remote_url.trim(),
            workspace_root.to_string_lossy().as_ref(),
        ])
        .status()?;
    assert!(status.success());
    configure_git_identity(&workspace_root)?;
    Ok(workspace_root)
}

#[cfg(unix)]
fn configure_git_identity(repo_root: &Path) -> Result<(), Box<dyn Error>> {
    let repo = repo_root.to_string_lossy();
    for args in [
        vec![
            "-C",
            repo.as_ref(),
            "config",
            "user.email",
            "test@example.com",
        ],
        vec!["-C", repo.as_ref(), "config", "user.name", "Meta Test"],
    ] {
        let status = ProcessCommand::new("git").args(args).status()?;
        assert!(status.success());
    }

    Ok(())
}

#[cfg(unix)]
fn git_stdout(repo_root: &Path, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()?;
    assert!(output.status.success(), "git {:?} failed", args);
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(unix)]
fn commit_and_push_pull_ref(
    repo_root: &Path,
    branch: &str,
    file: &str,
    contents: &str,
    pull_number: u64,
) -> Result<String, Box<dyn Error>> {
    let repo = repo_root.to_string_lossy();
    let status = ProcessCommand::new("git")
        .args(["-C", repo.as_ref(), "checkout", "-B", branch, "main"])
        .status()?;
    assert!(status.success());

    fs::write(repo_root.join(file), contents)?;

    for args in [
        vec!["-C", repo.as_ref(), "add", file],
        vec![
            "-C",
            repo.as_ref(),
            "commit",
            "-m",
            &format!("Prepare pull request {pull_number}"),
        ],
        vec![
            "-C",
            repo.as_ref(),
            "push",
            "--force",
            "origin",
            &format!("HEAD:refs/pull/{pull_number}/head"),
        ],
    ] {
        let status = ProcessCommand::new("git").args(args).status()?;
        assert!(status.success());
    }

    let output = ProcessCommand::new("git")
        .args(["-C", repo.as_ref(), "rev-parse", "HEAD"])
        .output()?;
    assert!(output.status.success());
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let status = ProcessCommand::new("git")
        .args(["-C", repo.as_ref(), "checkout", "main"])
        .status()?;
    assert!(status.success());

    Ok(sha)
}

#[cfg(unix)]
fn wait_for_path(path: &Path) -> Result<(), Box<dyn Error>> {
    wait_for_path_with_timeout(path, Duration::from_secs(60))
}

#[cfg(unix)]
fn wait_for_path_with_timeout(path: &Path, timeout: Duration) -> Result<(), Box<dyn Error>> {
    let poll_interval = Duration::from_millis(50);
    let attempts = timeout.as_millis() / poll_interval.as_millis();
    for _ in 0..attempts {
        if path.exists() {
            return Ok(());
        }
        thread::sleep(poll_interval);
    }

    Err(format!(
        "timed out waiting for `{}` after {}s",
        path.display(),
        timeout.as_secs()
    )
    .into())
}

#[cfg(unix)]
fn wait_for_pid_to_stop(pid: u32) -> Result<(), Box<dyn Error>> {
    for _ in 0..40 {
        let output = ProcessCommand::new("ps")
            .args(["-p", &pid.to_string()])
            .output()?;
        if !output.status.success() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }

    Err(format!("timed out waiting for pid {pid} to stop").into())
}

fn issue_node(
    id: &str,
    identifier: &str,
    title: &str,
    description: &str,
    state_id: &str,
    state_name: &str,
) -> serde_json::Value {
    json!({
        "id": id,
        "identifier": identifier,
        "title": title,
        "description": description,
        "url": format!("https://linear.app/issues/{identifier}"),
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
            "id": state_id,
            "name": state_name,
            "type": if state_name == "Todo" { "unstarted" } else { "started" }
        }
    })
}

fn issue_detail_node(
    id: &str,
    identifier: &str,
    title: &str,
    description: &str,
    attachments: Vec<serde_json::Value>,
    parent: Option<serde_json::Value>,
) -> serde_json::Value {
    json!({
        "id": id,
        "identifier": identifier,
        "title": title,
        "description": description,
        "url": format!("https://linear.app/issues/{identifier}"),
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
        "labels": { "nodes": [] },
        "comments": { "nodes": [] },
        "state": {
            "id": "state-1",
            "name": "Todo",
            "type": "unstarted"
        },
        "attachments": { "nodes": attachments },
        "parent": parent,
        "children": { "nodes": [] }
    })
}

#[allow(clippy::too_many_arguments)]
fn listen_issue_detail_node(
    id: &str,
    identifier: &str,
    title: &str,
    description: &str,
    state_id: &str,
    state_name: &str,
    comments: Vec<serde_json::Value>,
    attachments: Vec<serde_json::Value>,
    children: Vec<serde_json::Value>,
) -> serde_json::Value {
    json!({
        "id": id,
        "identifier": identifier,
        "title": title,
        "description": description,
        "url": format!("https://linear.app/issues/{identifier}"),
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
        "assignee": {
            "id": "viewer-1",
            "name": "Kames",
            "email": "sudo@example.com"
        },
        "labels": {
            "nodes": [{
                "id": "label-1",
                "name": "agent"
            }]
        },
        "comments": { "nodes": comments },
        "state": {
            "id": state_id,
            "name": state_name,
            "type": if state_name == "Todo" { "unstarted" } else { "started" }
        },
        "attachments": { "nodes": attachments },
        "parent": null,
        "children": { "nodes": children }
    })
}

fn team_payload() -> serde_json::Value {
    json!({
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
    })
}

#[cfg(unix)]
fn wait_for_file_substring(path: &Path, expected: &str) -> Result<(), Box<dyn Error>> {
    wait_for_file_substring_with_timeout(path, expected, Duration::from_secs(60))
}

#[cfg(unix)]
fn wait_for_file_substring_with_timeout(
    path: &Path,
    expected: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let poll_interval = Duration::from_millis(100);
    let attempts = timeout.as_millis() / poll_interval.as_millis();
    for _ in 0..attempts {
        if let Ok(contents) = fs::read_to_string(path)
            && contents.contains(expected)
        {
            return Ok(());
        }
        thread::sleep(poll_interval);
    }

    Err(format!(
        "timed out waiting for `{}` to contain substring `{expected}` after {}s",
        path.display(),
        timeout.as_secs()
    )
    .into())
}

#[cfg(unix)]
fn wait_for_terminal_session_state(path: &Path) -> Result<(), Box<dyn Error>> {
    for _ in 0..300 {
        if let Ok(contents) = fs::read_to_string(path)
            && !contents.contains("\"phase\": \"claimed\"")
            && !contents.contains("\"phase\": \"brief_ready\"")
            && !contents.contains("\"phase\": \"running\"")
        {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }

    Err(format!(
        "timed out waiting for `{}` to reach a terminal session state",
        path.display()
    )
    .into())
}

#[cfg(unix)]
fn listen_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(unix)]
#[derive(Default)]
struct DynamicLinearState {
    claimed: bool,
    issue_refreshes_after_claim: usize,
    review_transition_applied: bool,
    complete_after_claim_refreshes: usize,
}

#[cfg(unix)]
struct DynamicLinearServer {
    url: String,
    shutdown: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl DynamicLinearServer {
    fn start() -> Result<Self, Box<dyn Error>> {
        Self::start_with_completion_after_refreshes(8)
    }

    fn start_with_completion_after_refreshes(
        complete_after_claim_refreshes: usize,
    ) -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(DynamicLinearState {
            complete_after_claim_refreshes,
            ..DynamicLinearState::default()
        }));
        let thread_shutdown = shutdown.clone();
        let handle = thread::spawn(move || {
            while !thread_shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
                        let _ = handle_dynamic_linear_connection(&mut stream, &state);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            url: format!("http://{address}/graphql"),
            shutdown,
            handle: Some(handle),
        })
    }
}

#[cfg(unix)]
impl Drop for DynamicLinearServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(
            self.url
                .trim_start_matches("http://")
                .trim_end_matches("/graphql"),
        );
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(unix)]
fn handle_dynamic_linear_connection(
    stream: &mut TcpStream,
    state: &Arc<Mutex<DynamicLinearState>>,
) -> Result<(), Box<dyn Error>> {
    let mut pending = Vec::new();
    loop {
        let request = read_http_request(stream, &mut pending)?;
        if request.trim().is_empty() {
            return Ok(());
        }
        let body = request
            .split("\r\n\r\n")
            .nth(1)
            .unwrap_or_default()
            .to_string();
        match dynamic_linear_response(&body, state) {
            Ok(response) => {
                let encoded = serde_json::to_string(&response)?;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: keep-alive\r\n\r\n{}",
                    encoded.len(),
                    encoded
                )?;
                stream.flush()?;
            }
            Err(error) => {
                let encoded = serde_json::to_string(&json!({
                    "errors": [{
                        "message": error.to_string()
                    }]
                }))?;
                write!(
                    stream,
                    "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: keep-alive\r\n\r\n{}",
                    encoded.len(),
                    encoded
                )?;
                stream.flush()?;
            }
        }
    }
}

#[cfg(unix)]
fn read_http_request(stream: &mut TcpStream, pending: &mut Vec<u8>) -> Result<String, Box<dyn Error>> {
    let mut chunk = [0u8; 4096];
    let mut idle_reads_after_data = 0usize;

    loop {
        if let Some(request_len) = complete_http_request_len(pending) {
            let remainder = pending.split_off(request_len);
            let request = std::mem::replace(pending, remainder);
            return Ok(String::from_utf8(request)?);
        }

        let read = match stream.read(&mut chunk) {
            Ok(read) => {
                idle_reads_after_data = 0;
                read
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if pending.is_empty() {
                    return Ok(String::new());
                }
                idle_reads_after_data += 1;
                if idle_reads_after_data >= 20 {
                    return Err(format!(
                        "timed out waiting for a complete HTTP request after receiving {} bytes",
                        pending.len()
                    )
                    .into());
                }
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if read == 0 {
            if pending.is_empty() {
                return Ok(String::new());
            }
            if let Some(request_len) = complete_http_request_len(pending)
            {
                let remainder = pending.split_off(request_len);
                let request = std::mem::replace(pending, remainder);
                return Ok(String::from_utf8(request)?);
            }
            return Err(format!(
                "peer closed the HTTP request before the body completed (received {} bytes)",
                pending.len()
            )
            .into());
        }
        pending.extend_from_slice(&chunk[..read]);
    }
}

#[cfg(unix)]
fn complete_http_request_len(buffer: &[u8]) -> Option<usize> {
    let header_end = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)?;
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let mut content_length = 0usize;
    for line in headers.lines() {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse::<usize>().unwrap_or(0);
            break;
        }
    }
    (buffer.len() >= header_end + content_length).then_some(header_end + content_length)
}

#[cfg(unix)]
fn dynamic_linear_response(
    body: &str,
    state: &Arc<Mutex<DynamicLinearState>>,
) -> Result<serde_json::Value, Box<dyn Error>> {
    if body.contains("query Viewer") {
        return Ok(json!({
            "data": {
                "viewer": {
                    "id": "viewer-1",
                    "name": "Kames",
                    "email": "sudo@example.com"
                }
            }
        }));
    }

    if body.contains("query Teams") {
        return Ok(json!({
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
                                },
                                {
                                    "id": "state-3",
                                    "name": "Human Review",
                                    "type": "started"
                                }
                            ]
                        }
                    }]
                }
            }
        }));
    }

    if body.contains("query Projects") {
        return Ok(json!({
            "data": {
                "projects": {
                    "nodes": [{
                        "id": "project-1",
                        "name": "MetaStack CLI",
                        "description": null,
                        "url": "https://linear.app/projects/project-1",
                        "progress": 0.5,
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
    }

    if body.contains("mutation UpdateIssue") {
        let mut state = state.lock().expect("state mutex should lock");
        state.claimed = true;
        if body.contains(r#""stateId":"state-3""#) {
            state.review_transition_applied = true;
        }
        let (state_id, state_name, state_type) = if state.review_transition_applied {
            ("state-3", "Human Review", "started")
        } else {
            ("state-2", "In Progress", "started")
        };
        return Ok(json!({
            "data": {
                "issueUpdate": {
                    "success": true,
                    "issue": {
                        "id": "issue-32",
                        "identifier": "MET-32",
                        "title": "Continuation loop",
                        "description": "Keep running until the issue leaves active states",
                        "url": "https://linear.app/issues/32",
                        "priority": 2,
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
                            "id": state_id,
                            "name": state_name,
                            "type": state_type
                        }
                    }
                }
            }
        }));
    }

    if body.contains("mutation CreateIssue") {
        return Ok(json!({
            "data": {
                "issueCreate": {
                    "success": true,
                    "issue": {
                        "id": "issue-33",
                        "identifier": "MET-33",
                        "title": "Technical: Continuation loop",
                        "description": "# Technical: Continuation loop\n",
                        "url": "https://linear.app/issues/MET-33",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:03:00Z",
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
                            "id": "state-backlog",
                            "name": "Backlog",
                            "type": "backlog"
                        }
                    }
                }
            }
        }));
    }

    if body.contains("mutation CreateComment") {
        return Ok(json!({
            "data": {
                "commentCreate": {
                    "success": true,
                    "comment": {
                        "id": "comment-32",
                        "body": "## Codex Workpad",
                        "resolvedAt": null
                    }
                }
            }
        }));
    }

    if body.contains("mutation CreateAttachment") {
        return Ok(json!({
            "data": {
                "attachmentCreate": {
                    "success": true,
                    "attachment": {
                        "id": "attachment-32",
                        "title": "GitHub PR #321",
                        "url": "https://github.com/example/repo/pull/321",
                        "sourceType": "custom",
                        "metadata": {
                            "provider": "github",
                            "type": "pull_request"
                        }
                    }
                }
            }
        }));
    }

    if body.contains("query Issue($id: String!)") {
        if body.contains("\"id\":\"issue-33\"") {
            return Ok(json!({
                "data": {
                    "issue": listen_issue_detail_node(
                        "issue-33",
                        "MET-33",
                        "Technical: Continuation loop",
                        "# Technical: Continuation loop\n",
                        "state-backlog",
                        "Backlog",
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                    )
                }
            }));
        }
        let state = state.lock().expect("state mutex should lock");
        let (state_id, state_name) = if state.review_transition_applied {
            ("state-3", "Human Review")
        } else if state.claimed {
            ("state-2", "In Progress")
        } else {
            ("state-1", "Todo")
        };
        return Ok(json!({
            "data": {
                "issue": listen_issue_detail_node(
                    "issue-32",
                    "MET-32",
                    "Continuation loop",
                    "Keep running until the issue leaves active states",
                    state_id,
                    state_name,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )
            }
        }));
    }

    if body.contains("query Issues") {
        let mut state = state.lock().expect("state mutex should lock");
        let issue_state = if state.review_transition_applied {
            ("state-3", "Human Review", "started")
        } else if state.claimed {
            state.issue_refreshes_after_claim += 1;
            let threshold = if state.complete_after_claim_refreshes > 0 {
                state.complete_after_claim_refreshes
            } else {
                6
            };
            if state.issue_refreshes_after_claim >= threshold {
                state.review_transition_applied = true;
                ("state-3", "Human Review", "started")
            } else {
                ("state-2", "In Progress", "started")
            }
        } else {
            ("state-1", "Todo", "unstarted")
        };

        return Ok(json!({
            "data": {
                "issues": {
                    "nodes": [{
                        "id": "issue-32",
                        "identifier": "MET-32",
                        "title": "Continuation loop",
                        "description": "Keep running until the issue leaves active states",
                        "url": "https://linear.app/issues/32",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:00:00Z",
                        "assignee": {
                            "id": "viewer-1",
                            "name": "Kames",
                            "email": "sudo@example.com"
                        },
                        "labels": {
                            "nodes": [{
                                "id": "label-1",
                                "name": "agent"
                            }]
                        },
                        "comments": {
                            "nodes": []
                        },
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
                            "id": issue_state.0,
                            "name": issue_state.1,
                            "type": issue_state.2
                        }
                    }, {
                        "id": "issue-33",
                        "identifier": "MET-33",
                        "title": "Technical: Continuation loop",
                        "description": "# Technical: Continuation loop\n",
                        "url": "https://linear.app/issues/MET-33",
                        "priority": 2,
                        "updatedAt": "2026-03-14T16:01:00Z",
                        "assignee": null,
                        "labels": {
                            "nodes": []
                        },
                        "comments": {
                            "nodes": []
                        },
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
                            "id": "state-backlog",
                            "name": "Backlog",
                            "type": "backlog"
                        }
                    }]
                }
            }
        }));
    }

    Err(format!("unexpected GraphQL payload: {body}").into())
}

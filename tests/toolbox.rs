//! Integration tests for user-defined toolbox tools.
//!
//! These tests drive the real binary against a mocked Responses API and a
//! fixture toolbox executable, covering discovery (`CAKE_TOOLBOX`, `--toolbox`, and
//! project-local `.cake/tools`), describe parsing, system-prompt/tool registration, `tb__*`
//! dispatch through the execute protocol, and the read-only exclusion.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]
#![cfg(unix)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use support::{GIT_AMBIENT_ENV_VARS, TestEnv};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn write_responses_settings(env: &TestEnv, base_url: &str) {
    env.write_project_settings(&format!(
        r#"
default_model = "test"

[[models]]
name = "test"
model = "glm-5.1"
base_url = "{base_url}"
api_key_env = "TOOLBOX_TEST_KEY"
api_type = "responses"
"#
    ));
}

fn write_executable(dir: &Path, name: &str, content: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, content).expect("failed to write fixture tool");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .expect("failed to chmod fixture tool");
    path
}

/// A text-format toolbox tool: describes a `greet` tool with one parameter
/// and, on execute, reads the `who=<value>` line from stdin and greets.
fn write_greet_tool(dir: &Path) {
    write_executable(
        dir,
        "greet",
        "#!/bin/sh\n\
         if [ \"$TOOLBOX_ACTION\" = \"describe\" ]; then\n\
         printf 'name: greet\\ndescription: Greets someone.\\nwho: string Who to greet\\n'\n\
         else\n\
         read -r line\n\
         printf 'Hello, %s!' \"${line#who=}\"\n\
         fi\n",
    );
}

fn write_named_tool(dir: &Path, filename: &str, name: &str, description: &str) {
    write_executable(
        dir,
        filename,
        &format!("#!/bin/sh\nprintf 'name: {name}\\ndescription: {description}\\n'\n"),
    );
}

fn write_counted_describe_tool(dir: &Path) {
    write_executable(
        dir,
        "counted",
        "#!/bin/sh\n\
         if [ \"$TOOLBOX_ACTION\" = \"describe\" ]; then\n\
         count=$(cat \"$TOOLBOX_DESCRIBE_COUNT\" 2>/dev/null || printf 0)\n\
         printf '%s' \"$((count + 1))\" > \"$TOOLBOX_DESCRIBE_COUNT\"\n\
         printf 'name: counted\\ndescription: Counted.\\n'\n\
         fi\n",
    );
}

/// Build a hermetic git command for a fixture repository.
fn git(working_dir: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(working_dir);
    for var in GIT_AMBIENT_ENV_VARS {
        cmd.env_remove(var);
    }
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
    cmd.env("GIT_CONFIG_NOSYSTEM", "1");
    cmd.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "user.name=Cake Test",
        "-c",
        "user.email=cake-test@example.invalid",
    ]);
    cmd
}

/// Initialize a fixture repository with the current project files committed.
fn init_git_repo(dir: &Path) {
    let init = git(dir)
        .args(["init"])
        .output()
        .expect("failed to initialize git repository");
    assert!(
        init.status.success(),
        "git init should succeed: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let add = git(dir)
        .args(["add", "."])
        .output()
        .expect("failed to stage fixture repository");
    assert!(
        add.status.success(),
        "git add should succeed: {}",
        String::from_utf8_lossy(&add.stderr)
    );
    let commit = git(dir)
        .args(["commit", "-m", "initial"])
        .output()
        .expect("failed to commit fixture repository");
    assert!(
        commit.status.success(),
        "git commit should succeed: {}",
        String::from_utf8_lossy(&commit.stderr)
    );
}

fn tool_call_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-1",
        "output": [
            {
                "type": "function_call",
                "id": "fc-1",
                "call_id": "call-1",
                "name": "tb__greet",
                "arguments": "{\"who\": \"World\"}"
            }
        ],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
    })
}

fn final_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-2",
        "output": [
            {
                "type": "message",
                "id": "msg-1",
                "status": "completed",
                "content": [
                    { "type": "output_text", "text": "The tool said hello." }
                ]
            }
        ],
        "usage": { "input_tokens": 20, "output_tokens": 5, "total_tokens": 25 }
    })
}

fn session_file_contents(env: &TestEnv) -> String {
    let sessions_dir = env.data_dir.join("sessions");
    let entries: Vec<_> = fs::read_dir(&sessions_dir)
        .expect("sessions directory should exist")
        .collect::<Result<Vec<_>, _>>()
        .expect("sessions directory should be readable");
    assert_eq!(entries.len(), 1, "expected exactly one session file");
    fs::read_to_string(entries[0].path()).expect("session file should be readable")
}

#[tokio::test]
async fn toolbox_tool_discovered_registered_and_dispatched() {
    let env = TestEnv::new("cake-toolbox-e2e-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    let toolbox_dir = env.workspace_dir.join("toolbox");
    fs::create_dir_all(&toolbox_dir).expect("failed to create toolbox dir");
    write_greet_tool(&toolbox_dir);

    // First turn: the model calls tb__greet. Second turn: final message.
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(tool_call_response()))
        .up_to_n_times(1)
        .expect(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("greet the world")
        .env("TOOLBOX_TEST_KEY", "test-token")
        .env("CAKE_TOOLBOX", &toolbox_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let session = session_file_contents(&env);
    assert!(
        session.contains("tb__greet"),
        "session should register the toolbox tool: {session}"
    );
    assert!(
        session.contains("- **tb__greet**: Greets someone."),
        "system prompt should list the toolbox tool: {session}"
    );
    assert!(
        session.contains("Hello, World!"),
        "session should record the toolbox tool's output: {session}"
    );
}

#[tokio::test]
async fn project_local_toolbox_is_discovered_without_configuration() {
    let env = TestEnv::new("cake-project-toolbox-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    let toolbox_dir = env.workspace_dir.join(".cake").join("tools");
    fs::create_dir_all(&toolbox_dir).expect("failed to create project toolbox dir");
    write_greet_tool(&toolbox_dir);

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("test prompt")
        .env("TOOLBOX_TEST_KEY", "test-token")
        .env_remove("CAKE_TOOLBOX")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let session = session_file_contents(&env);
    assert!(
        session.contains("tb__greet"),
        "project-local toolbox should be discovered without configuration: {session}"
    );
}

#[tokio::test]
async fn explicit_project_toolbox_path_is_described_once() {
    let env = TestEnv::new("cake-project-toolbox-dedup-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    let toolbox_dir = env.workspace_dir.join(".cake").join("tools");
    fs::create_dir_all(&toolbox_dir).expect("failed to create project toolbox dir");
    write_counted_describe_tool(&toolbox_dir);
    fs::create_dir_all(&env.data_dir).expect("failed to create data directory");
    let count_file = env.data_dir.join("describe-count");
    fs::write(&count_file, "0").expect("failed to initialize describe counter");

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("test prompt")
        .env("TOOLBOX_TEST_KEY", "test-token")
        // This was the documented way to activate a project toolbox before
        // automatic project-local discovery was added.
        .env("CAKE_TOOLBOX", ".cake/tools")
        .env("TOOLBOX_DESCRIBE_COUNT", &count_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(count_file).expect("failed to read describe counter"),
        "1",
        "an explicitly configured project toolbox should not be described twice"
    );
}

#[tokio::test]
async fn worktree_uses_active_project_local_toolbox() {
    let env = TestEnv::new("cake-worktree-toolbox-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    let toolbox_dir = env.workspace_dir.join(".cake").join("tools");
    fs::create_dir_all(&toolbox_dir).expect("failed to create project toolbox dir");
    write_named_tool(
        &toolbox_dir,
        "selected",
        "worktree",
        "Active worktree toolbox.",
    );
    init_git_repo(&env.workspace_dir);
    // Leave a different uncommitted version in the invocation directory. A
    // resolver that runs before worktree setup would discover this one,
    // whereas the active worktree contains the committed version above.
    write_named_tool(
        &toolbox_dir,
        "selected",
        "invocation",
        "Invocation toolbox.",
    );

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("--worktree=toolbox-selection")
        .arg("test prompt")
        .env("TOOLBOX_TEST_KEY", "test-token")
        .env_remove("CAKE_TOOLBOX")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let session = session_file_contents(&env);
    assert!(
        session.contains("tb__worktree"),
        "the active worktree's project toolbox should be discovered: {session}"
    );
    assert!(
        session.contains("Active worktree toolbox."),
        "the active worktree's describe output should win: {session}"
    );
    assert!(
        !session.contains("tb__invocation"),
        "the original invocation directory's toolbox must not be used with --worktree: {session}"
    );
}

#[tokio::test]
async fn toolbox_flag_adds_directory_without_env() {
    let env = TestEnv::new("cake-toolbox-flag-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    let toolbox_dir = env.workspace_dir.join("toolbox");
    fs::create_dir_all(&toolbox_dir).expect("failed to create toolbox dir");
    write_greet_tool(&toolbox_dir);
    let project_toolbox_dir = env.workspace_dir.join(".cake").join("tools");
    fs::create_dir_all(&project_toolbox_dir).expect("failed to create project toolbox dir");
    write_named_tool(
        &project_toolbox_dir,
        "greet",
        "greet",
        "Project toolbox should lose.",
    );

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("--toolbox")
        .arg(&toolbox_dir)
        .arg("test prompt")
        .env("TOOLBOX_TEST_KEY", "test-token")
        .env_remove("CAKE_TOOLBOX")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let session = session_file_contents(&env);
    assert!(
        session.contains("- **tb__greet**: Greets someone."),
        "explicit --toolbox entries must take precedence over project-local tools: {session}"
    );
    assert!(
        !session.contains("Project toolbox should lose."),
        "project-local duplicate should not replace an explicit toolbox entry: {session}"
    );
}

#[tokio::test]
async fn read_only_sandbox_excludes_toolbox_tools() {
    let env = TestEnv::new("cake-toolbox-readonly-test");
    let mock_server = MockServer::start().await;
    write_responses_settings(&env, &mock_server.uri());

    let toolbox_dir = env.workspace_dir.join("toolbox");
    fs::create_dir_all(&toolbox_dir).expect("failed to create toolbox dir");
    // This tool's describe action has a side effect. Under read-only,
    // cake must not run toolbox executables at all — even describe runs
    // outside the OS sandbox and could mutate the workspace.
    let marker = env.workspace_dir.join("describe-ran");
    write_executable(
        &toolbox_dir,
        "greet",
        &format!(
            "#!/bin/sh\ntouch '{}'\nprintf 'name: greet\\ndescription: Greets someone.\\n'\n",
            marker.display()
        ),
    );

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_response()))
        .expect(1)
        .mount(&mock_server)
        .await;

    let output = env
        .command()
        .arg("--sandbox")
        .arg("read-only")
        .arg("test prompt")
        .env("TOOLBOX_TEST_KEY", "test-token")
        .env("CAKE_TOOLBOX", &toolbox_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake");

    assert!(
        output.status.success(),
        "cake should succeed. stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let session = session_file_contents(&env);
    assert!(
        !session.contains("tb__greet"),
        "read-only sessions must not register unsandboxed toolbox tools: {session}"
    );
    assert!(
        !marker.exists(),
        "read-only sessions must not execute toolbox describe actions"
    );
}

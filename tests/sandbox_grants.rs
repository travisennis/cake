//! Configured grants are provisioned only during writable agent runs.

#[expect(
    clippy::expect_used,
    reason = "shared test fixtures use expect for setup"
)]
mod support;

use std::fs;
use support::TestEnv;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn sandbox_grants_bootstrap_tilde_and_relative_paths_only_for_writable_runs() {
    let env = TestEnv::new("cake-sandbox-grants");
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "resp-grants",
            "output": [{"type": "message", "id": "msg-grants", "status": "completed",
                "content": [{"type": "output_text", "text": "Done."}]}],
            "usage": {"input_tokens": 1, "output_tokens": 0, "total_tokens": 1}
        })))
        .expect(2)
        .mount(&server)
        .await;
    let settings = format!(
        r#"default_model = "test"
directories = ["../legacy/parent/state"]
[[models]]
name = "test"
model = "test"
base_url = "{}"
api_key_env = "GRANTS_TEST_KEY"
api_type = "responses"
[sandbox]
writable = ["~/.cache/cake", "~/.local/share/cake"]
read_only = ["../absent-read-only"]
"#,
        server.uri()
    );
    env.write_project_settings(&settings);
    for args in [
        vec!["--help"],
        vec!["debug", "models", "--json"],
        vec!["--sandbox", "read-only", "inspect"],
    ] {
        let output = env
            .command()
            .args(args)
            .env("GRANTS_TEST_KEY", "test")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(!env.workspace_dir.join("../legacy").exists());
        assert!(!env.workspace_dir.join("../home/.cache/cake").exists());
        assert!(!env.workspace_dir.join("../home/.local/share/cake").exists());
    }
    let output = env
        .command()
        .args(["--sandbox", "workspace-write", "inspect"])
        .env("GRANTS_TEST_KEY", "test")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(env.workspace_dir.join("../legacy/parent/state").is_dir());
    assert!(env.workspace_dir.join("../home/.cache/cake").is_dir());
    assert!(env.workspace_dir.join("../home/.local/share/cake").is_dir());
    assert!(!env.workspace_dir.join("../absent-read-only").exists());
    let logs: String = fs::read_dir(&env.data_dir)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .map(|entry| fs::read_to_string(entry.path()).unwrap())
        .collect();
    assert!(
        logs.contains("created it and any missing parents"),
        "{logs}"
    );
    assert!(
        logs.contains("[sandbox].read_only") && logs.contains("ignoring"),
        "{logs}"
    );

    let link = env.workspace_dir.join("dangling");
    #[cfg(unix)]
    std::os::unix::fs::symlink(env.workspace_dir.join("absent"), &link).unwrap();
    #[cfg(not(unix))]
    fs::write(&link, "file").unwrap();
    env.write_project_settings(&settings.replace(
        "writable = [\"~/.cache/cake\", \"~/.local/share/cake\"]",
        "writable = [\"dangling/state\"]",
    ));
    let output = env
        .command()
        .args(["--sandbox", "workspace-write", "inspect"])
        .env("GRANTS_TEST_KEY", "test")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("[sandbox].writable"), "{stderr}");
    assert!(stderr.contains("dangling/state"), "{stderr}");
    assert!(!env.workspace_dir.join("absent").exists());
    server.verify().await;
}

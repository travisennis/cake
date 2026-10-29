//! Runtime gate resolution: precedence, bypass, credentials, and lifecycle.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::*;
use crate::config::gates::{GateConfiguration, GateSettings};
use crate::config::model::{ApiType, ModelConfig};
use crate::config::settings::{JudgeSettings, ModelDefinition};
use crate::gates::composer::{GatePipelineError, GateRequest, PipelineAuthorization};
use crate::gates::protocol::{BashInput, GateDecision, GateErrorClass};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gates/fake-runtime-gate.sh")
}

fn write_mode(dir: &Path, name: &str, mode: &str) -> PathBuf {
    let path = dir.join(format!("{name}.mode"));
    std::fs::write(&path, mode).unwrap_or_else(|error| panic!("write fake gate mode: {error}"));
    path
}

fn gate_settings(mode_file: &Path, capture: Option<&Path>) -> GateSettings {
    let mut args = vec![mode_file.display().to_string()];
    if let Some(capture) = capture {
        args.push(capture.display().to_string());
    }
    GateSettings {
        command: Some(fixture().display().to_string()),
        args: Some(args),
        timeout_secs: Some(2),
        startup_timeout_secs: Some(2),
        ..GateSettings::default()
    }
}

fn configuration(settings: GateSettings) -> GateConfiguration {
    GateConfiguration {
        definitions: BTreeMap::from([("safety".to_string(), settings)]),
        selection: Some(vec!["safety".to_string()]),
        legacy_present: false,
    }
}

fn model_definition(name: &str, base_url: &str, api_key_env: &str) -> ModelDefinition {
    ModelDefinition {
        name: name.to_string(),
        model: "runtime-model".to_string(),
        base_url: base_url.to_string(),
        api_key_env: api_key_env.to_string(),
        provider: None,
        provider_headers: None,
        api_type: ApiType::Responses,
        temperature: None,
        top_p: None,
        top_k: None,
        max_output_tokens: None,
        context_window: None,
        reasoning_effort: None,
        reasoning_summary: None,
        reasoning_max_tokens: None,
        providers: Vec::new(),
        supports_images: false,
    }
}

fn model_config(base_url: &str, api_key_env: &str) -> ModelConfig {
    ModelConfig {
        supports_images: false,
        model: "runtime-model".to_string(),
        api_type: ApiType::Responses,
        base_url: base_url.to_string(),
        api_key_env: api_key_env.to_string(),
        provider: None,
        provider_headers: None,
        temperature: None,
        top_p: None,
        top_k: None,
        max_output_tokens: None,
        context_window: None,
        reasoning_effort: None,
        reasoning_summary: None,
        reasoning_max_tokens: None,
        providers: Vec::new(),
    }
}

fn request(id: &str) -> GateRequest {
    GateRequest::new(BashInput::new(
        "git status --short".to_string(),
        "/repo".to_string(),
    ))
    .with_session(crate::gates::protocol::CheckSession {
        session_id: Some(id.to_string()),
        ..crate::gates::protocol::CheckSession::default()
    })
}

fn init_frames(capture: &Path) -> String {
    std::fs::read_to_string(capture.join("init.jsonl")).unwrap_or_default()
}

fn captured_environment(capture: &Path) -> String {
    let mut text = String::new();
    for entry in
        std::fs::read_dir(capture).unwrap_or_else(|error| panic!("read capture dir: {error}"))
    {
        let path = entry
            .unwrap_or_else(|error| panic!("read capture entry: {error}"))
            .path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("env."))
        {
            text.push_str(
                &std::fs::read_to_string(path)
                    .unwrap_or_else(|error| panic!("read captured env: {error}")),
            );
        }
    }
    text
}

fn start_count(capture: &Path) -> usize {
    std::fs::read_to_string(capture.join("starts")).map_or(0, |text| text.lines().count())
}

#[tokio::test]
async fn selected_enforce_gate_starts_and_allows() {
    let dir = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "allow", "allow");
    let configuration = configuration(gate_settings(&mode, None));
    let pipeline = start(
        &configuration,
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect("enforce gate starts");

    assert!(!pipeline.is_bypass());
    let records = pipeline.startup_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].outcome, "ready");

    let outcome = pipeline.check(request("check-1")).await.expect("allow");
    assert_eq!(outcome.authorization, PipelineAuthorization::Run);
    assert_eq!(outcome.verdict, Some(GateDecision::Allow));
    assert_eq!(outcome.stages.len(), 1);
    pipeline.shutdown().await;
}

#[tokio::test]
async fn empty_off_and_shadow_only_selections_are_not_enforce_gates() {
    let dir = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "allow", "allow");

    let empty = GateConfiguration {
        definitions: BTreeMap::new(),
        selection: Some(Vec::new()),
        legacy_present: false,
    };
    let error = start(
        &empty,
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect_err("an empty selection has no enforce gate");
    assert!(matches!(error, GatePipelineError::NoEnforceGate));

    for (name, mode_value) in [("off", "off"), ("shadow", "shadow")] {
        let mut settings = gate_settings(&mode, None);
        settings.mode = Some(mode_value.to_string());
        let configuration = configuration(settings);
        assert!(!configuration.bypassed(&JudgeSettings::default(), false));
        let error = start(
            &configuration,
            &JudgeSettings::default(),
            &HashMap::new(),
            None,
            dir.path(),
            false,
        )
        .await
        .expect_err("no enforce gate remains");
        assert!(
            matches!(error, GatePipelineError::NoEnforceGate),
            "{name}: {error}"
        );
    }
}

#[tokio::test]
async fn explicit_bypass_and_disabled_legacy_judge_skip_gate_startup() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = gate_settings(Path::new("/does/not/exist"), None);
    settings.models = Some(vec!["missing-model".to_string()]);
    let configuration = configuration(settings);
    let pipeline = start(
        &configuration,
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        true,
    )
    .await
    .expect("explicit bypass wins before the command or models resolve");
    assert!(pipeline.is_bypass());
    assert!(pipeline.startup_records().is_empty());

    let legacy = JudgeSettings {
        enabled: false,
        ..JudgeSettings::default()
    };
    let pipeline = start(
        &GateConfiguration::default(),
        &legacy,
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect("legacy disablement is an explicit bypass");
    assert!(pipeline.is_bypass());
}

#[tokio::test]
async fn explicit_selection_ignores_a_disabled_legacy_judge() {
    let dir = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "allow", "allow");
    let configuration = configuration(gate_settings(&mode, None));
    let legacy = JudgeSettings {
        enabled: false,
        ..JudgeSettings::default()
    };

    let pipeline = start(
        &configuration,
        &legacy,
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect("selection owns policy");
    assert!(!pipeline.is_bypass());
    let outcome = pipeline.check(request("check-1")).await.expect("allow");
    assert_eq!(outcome.verdict, Some(GateDecision::Allow));
    pipeline.shutdown().await;
}

#[tokio::test]
async fn enforce_startup_failure_is_pipeline_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "init-error", "init-error");
    let configuration = configuration(gate_settings(&mode, None));
    let error = start(
        &configuration,
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect_err("init failure must be fatal");

    match error {
        GatePipelineError::Startup { gate, startup, .. } => {
            assert_eq!(gate, "safety");
            assert_eq!(startup.outcome, "failed");
            assert_eq!(startup.failure_class.as_deref(), Some("config"));
        },
        other => panic!("expected a startup failure, got {other:?}"),
    }
}

#[tokio::test]
async fn shadow_startup_failure_is_recorded_and_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let enforce_mode = write_mode(dir.path(), "enforce", "allow");
    let shadow_mode = write_mode(dir.path(), "shadow", "init-error");
    let mut shadow = gate_settings(&shadow_mode, None);
    shadow.mode = Some("shadow".to_string());
    let configuration = GateConfiguration {
        definitions: BTreeMap::from([
            ("enforce".to_string(), gate_settings(&enforce_mode, None)),
            ("shadow".to_string(), shadow),
        ]),
        selection: Some(vec!["enforce".to_string(), "shadow".to_string()]),
        legacy_present: false,
    };

    let pipeline = start(
        &configuration,
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect("the enforce gate still starts");

    let records = pipeline.startup_records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].gate, "enforce");
    assert_eq!(records[0].outcome, "ready");
    assert_eq!(records[1].gate, "shadow");
    assert_eq!(records[1].outcome, "failed");

    let outcome = pipeline.check(request("check-1")).await.expect("allow");
    assert_eq!(outcome.verdict, Some(GateDecision::Allow));
    assert_eq!(outcome.stages.len(), 1, "the failed shadow gate is dropped");
    pipeline.shutdown().await;
}

#[tokio::test]
async fn descriptors_carry_credential_names_and_only_declared_environment() {
    let dir = tempfile::tempdir().unwrap();
    let capture = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "allow", "allow");
    let mut settings = gate_settings(&mode, Some(capture.path()));
    settings.models = Some(vec!["judge".to_string()]);
    settings.env = Some(vec!["CAKE_RUNTIME_DECLARED".to_string()]);
    let configuration = configuration(settings);
    let models = HashMap::from([(
        "judge".to_string(),
        model_definition(
            "judge",
            "https://example.invalid/v1",
            "CAKE_RUNTIME_TEST_KEY",
        ),
    )]);

    temp_env::async_with_vars(
        [
            ("CAKE_RUNTIME_TEST_KEY", Some("sk-runtime-secret")),
            ("CAKE_RUNTIME_DECLARED", Some("declared-value")),
            ("CAKE_RUNTIME_UNDECLARED", Some("undeclared-secret")),
        ],
        async {
            let pipeline = start(
                &configuration,
                &JudgeSettings::default(),
                &models,
                None,
                dir.path(),
                false,
            )
            .await
            .expect("gate with an environment-backed model starts");

            let init = init_frames(capture.path());
            assert!(
                init.contains(r#""api_key_env":"CAKE_RUNTIME_TEST_KEY""#),
                "{init}"
            );
            assert!(
                !init.contains("sk-runtime-secret"),
                "credential leaked: {init}"
            );
            assert!(init.contains(r#""CAKE_RUNTIME_DECLARED""#), "{init}");
            assert!(init.contains(r#""CAKE_RUNTIME_TEST_KEY""#), "{init}");
            assert!(init.contains(r#""models":{"judge""#), "{init}");

            let environment = captured_environment(capture.path());
            assert!(environment.contains("CAKE_RUNTIME_TEST_KEY=sk-runtime-secret"));
            assert!(environment.contains("CAKE_RUNTIME_DECLARED=declared-value"));
            assert!(
                !environment.contains("CAKE_RUNTIME_UNDECLARED=undeclared-secret"),
                "an undeclared variable crossed the process boundary: {environment}"
            );
            pipeline.shutdown().await;
        },
    )
    .await;
}

#[tokio::test]
async fn missing_credentials_and_unsupported_models_are_config_errors() {
    let dir = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "allow", "allow");
    let mut settings = gate_settings(&mode, None);
    settings.models = Some(vec!["judge".to_string()]);
    let configuration = configuration(settings);
    let models = HashMap::from([(
        "judge".to_string(),
        model_definition(
            "judge",
            "https://example.invalid/v1",
            "CAKE_RUNTIME_MISSING_KEY",
        ),
    )]);

    temp_env::async_with_vars([("CAKE_RUNTIME_MISSING_KEY", None::<&str>)], async {
        let error = start(
            &configuration,
            &JudgeSettings::default(),
            &models,
            None,
            dir.path(),
            false,
        )
        .await
        .expect_err("a missing credential must fail closed");
        assert_eq!(error.class(), GateErrorClass::Config);
        assert!(
            error.to_string().contains("CAKE_RUNTIME_MISSING_KEY"),
            "{error}"
        );
    })
    .await;

    let codex = HashMap::from([(
        "judge".to_string(),
        model_definition(
            "judge",
            "https://chatgpt.com/backend-api/codex",
            "CAKE_RUNTIME_CODEX_KEY",
        ),
    )]);
    let error = start(
        &configuration,
        &JudgeSettings::default(),
        &codex,
        None,
        dir.path(),
        false,
    )
    .await
    .expect_err("codex subscription credentials never cross the boundary");
    assert!(error.to_string().contains("environment-backed"), "{error}");

    let unknown = HashMap::new();
    let error = start(
        &configuration,
        &JudgeSettings::default(),
        &unknown,
        None,
        dir.path(),
        false,
    )
    .await
    .expect_err("an unknown explicit model is a config error");
    assert!(error.to_string().contains("unknown gate model"), "{error}");
}

#[tokio::test]
async fn file_backed_default_model_is_omitted_from_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    let capture = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "allow", "allow");
    let mut settings = gate_settings(&mode, Some(capture.path()));
    settings.models = Some(vec!["default".to_string()]);
    let configuration = configuration(settings);
    let default = model_config("https://chatgpt.com/backend-api/codex", "CODEX_KEY");

    let pipeline = start(
        &configuration,
        &JudgeSettings::default(),
        &HashMap::new(),
        Some(&default),
        dir.path(),
        false,
    )
    .await
    .expect("a gate can run without a model descriptor");
    let init = init_frames(capture.path());
    assert!(init.contains(r#""models":{}"#), "{init}");
    assert!(!init.contains("chatgpt.com/backend-api/codex"), "{init}");
    pipeline.shutdown().await;
}

#[tokio::test]
async fn models_without_the_resolved_models_capability_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "no-models", "no-models");
    let mut settings = gate_settings(&mode, None);
    settings.models = Some(vec!["judge".to_string()]);
    let configuration = configuration(settings);
    let models = HashMap::from([(
        "judge".to_string(),
        model_definition("judge", "https://example.invalid/v1", "PATH"),
    )]);

    let error = start(
        &configuration,
        &JudgeSettings::default(),
        &models,
        None,
        dir.path(),
        false,
    )
    .await
    .expect_err("models require the resolved-models capability");
    assert!(
        error.to_string().contains("resolved-models capability"),
        "{error}"
    );
}

#[tokio::test]
async fn unsupported_descriptor_fields_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "allow", "allow");
    let mut settings = gate_settings(&mode, None);
    settings.models = Some(vec!["judge".to_string()]);
    let configuration = configuration(settings);

    let mut top_k = model_definition("judge", "https://example.invalid/v1", "PATH");
    top_k.top_k = Some(40);
    let models = HashMap::from([("judge".to_string(), top_k)]);
    let error = start(
        &configuration,
        &JudgeSettings::default(),
        &models,
        None,
        dir.path(),
        false,
    )
    .await
    .expect_err("top_k cannot cross the descriptor");
    assert!(error.to_string().contains("top_k"), "{error}");

    let mut routed = model_definition("judge", "https://example.invalid/v1", "PATH");
    routed.providers = vec!["openrouter".to_string()];
    let models = HashMap::from([("judge".to_string(), routed)]);
    let error = start(
        &configuration,
        &JudgeSettings::default(),
        &models,
        None,
        dir.path(),
        false,
    )
    .await
    .expect_err("provider routing cannot cross the descriptor");
    assert!(error.to_string().contains("provider routing"), "{error}");
}

#[tokio::test]
async fn oneshot_and_stdio_lifecycles_match_the_transport() {
    let dir = tempfile::tempdir().unwrap();

    let stdio_capture = tempfile::tempdir().unwrap();
    let stdio_mode = write_mode(dir.path(), "stdio", "allow");
    let stdio_configuration = configuration(gate_settings(&stdio_mode, Some(stdio_capture.path())));
    let pipeline = start(
        &stdio_configuration,
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect("stdio starts");
    for id in ["check-1", "check-2"] {
        let outcome = pipeline.check(request(id)).await.expect("allow");
        assert_eq!(outcome.verdict, Some(GateDecision::Allow));
    }
    pipeline.shutdown().await;
    assert_eq!(
        start_count(stdio_capture.path()),
        1,
        "stdio reuses one process"
    );

    let oneshot_capture = tempfile::tempdir().unwrap();
    let oneshot_mode = write_mode(dir.path(), "oneshot", "oneshot-allow");
    let mut settings = gate_settings(&oneshot_mode, Some(oneshot_capture.path()));
    settings.transport = Some("oneshot".to_string());
    let oneshot_configuration = configuration(settings);
    let pipeline = start(
        &oneshot_configuration,
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect("oneshot starts");
    for id in ["check-1", "check-2"] {
        let outcome = pipeline.check(request(id)).await.expect("allow");
        assert_eq!(outcome.verdict, Some(GateDecision::Allow));
    }
    pipeline.shutdown().await;
    assert_eq!(
        start_count(oneshot_capture.path()),
        2,
        "oneshot starts a fresh process per check"
    );

    let unsupported_mode = write_mode(dir.path(), "unsupported", "allow");
    let mut settings = gate_settings(&unsupported_mode, None);
    settings.transport = Some("oneshot".to_string());
    let error = start(
        &configuration(settings),
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect_err("the gate must declare oneshot");
    assert!(
        error.to_string().contains("does not support oneshot"),
        "{error}"
    );
}

#[tokio::test]
async fn runtime_guards_invalid_mode_and_missing_command() {
    let dir = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "allow", "allow");
    let mut invalid = gate_settings(&mode, None);
    invalid.mode = Some("sometimes".to_string());
    let error = start(
        &configuration(invalid),
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect_err("invalid mode is a startup configuration error");
    assert!(error.to_string().contains("gate mode must be"), "{error}");

    let missing = GateSettings {
        command: None,
        ..GateSettings::default()
    };
    let error = start(
        &configuration(missing),
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect_err("a selected gate needs a command");
    assert!(error.to_string().contains("no command"), "{error}");
}

#[tokio::test]
async fn fail_open_configuration_is_applied_to_check_failures() {
    let dir = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "check-error", "check-error");

    let mut open = gate_settings(&mode, None);
    open.fail = Some("open".to_string());
    let pipeline = start(
        &configuration(open),
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect("open gate starts");
    let outcome = pipeline
        .check(request("check-1"))
        .await
        .expect("open failure continues");
    assert_eq!(outcome.authorization, PipelineAuthorization::Run);
    assert!(outcome.stages[0].failed_open);
    assert!(outcome.stages[0].failure.is_some());
    pipeline.shutdown().await;

    let error = start(
        &configuration(gate_settings(&mode, None)),
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect("closed gate starts")
    .check(request("check-2"))
    .await
    .expect_err("closed failure is fatal");
    assert_eq!(error.class(), GateErrorClass::Config);
    assert!(matches!(error, GatePipelineError::Check { .. }));
}

#[tokio::test]
async fn a_shadow_gate_without_the_capability_is_recorded_and_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let enforce_mode = write_mode(dir.path(), "enforce", "allow");
    let shadow_mode = write_mode(dir.path(), "no-shadow", "no-shadow");
    let mut shadow = gate_settings(&shadow_mode, None);
    shadow.mode = Some("shadow".to_string());
    let configuration = GateConfiguration {
        definitions: BTreeMap::from([
            ("enforce".to_string(), gate_settings(&enforce_mode, None)),
            ("shadow".to_string(), shadow),
        ]),
        selection: Some(vec!["enforce".to_string(), "shadow".to_string()]),
        legacy_present: false,
    };

    let pipeline = start(
        &configuration,
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect("the enforce gate starts");
    let shadow = pipeline
        .startup_records()
        .into_iter()
        .find(|record| record.gate == "shadow")
        .expect("shadow startup record");
    assert_eq!(shadow.outcome, "failed");
    assert_eq!(shadow.failure_class.as_deref(), Some("config"));
    pipeline.shutdown().await;
}

#[tokio::test]
async fn environment_backed_default_model_is_described() {
    let dir = tempfile::tempdir().unwrap();
    let capture = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "allow", "allow");
    let mut settings = gate_settings(&mode, Some(capture.path()));
    settings.models = Some(vec!["default".to_string()]);
    let default = model_config("https://example.invalid/v1", "PATH");

    let pipeline = start(
        &configuration(settings),
        &JudgeSettings::default(),
        &HashMap::new(),
        Some(&default),
        dir.path(),
        false,
    )
    .await
    .expect("environment-backed default model starts");
    let init = init_frames(capture.path());
    assert!(init.contains(r#""models":{"default""#), "{init}");
    assert!(init.contains(r#""api_key_env":"PATH""#), "{init}");
    pipeline.shutdown().await;
}

#[tokio::test]
async fn response_id_mismatch_fails_the_check_closed() {
    let dir = tempfile::tempdir().unwrap();
    let mode = write_mode(dir.path(), "wrong-id", "wrong-id");
    let pipeline = start(
        &configuration(gate_settings(&mode, None)),
        &JudgeSettings::default(),
        &HashMap::new(),
        None,
        dir.path(),
        false,
    )
    .await
    .expect("gate starts");
    let error = pipeline
        .check(request("check-1"))
        .await
        .expect_err("a mismatched response id is a protocol failure");
    assert_eq!(error.class(), GateErrorClass::Protocol);
    assert!(matches!(error, GatePipelineError::Check { .. }));
    pipeline.shutdown().await;
}

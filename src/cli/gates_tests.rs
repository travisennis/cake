use clap::CommandFactory;
use serde_json::Value;

use super::*;
use crate::config::gates::{GateConfiguration, GateSettings};

#[test]
fn parses_gate_list_and_doctor_commands() {
    for (arguments, expected) in [
        (vec!["cake", "gates", "list"], "list"),
        (vec!["cake", "gates", "doctor", "--json"], "doctor"),
    ] {
        let parsed = crate::CodingAssistant::parse_from(arguments);
        match parsed.command {
            Some(crate::cli::Commands::Gates(command)) => match (expected, command.command) {
                ("list", GatesSubcommand::List { json: false })
                | ("doctor", GatesSubcommand::Doctor { json: true }) => {},
                (kind, other) => panic!("unexpected {kind} command {other:?}"),
            },
            other => panic!("expected gates command, got {other:?}"),
        }
    }
}

#[test]
fn gates_help_explains_list_is_non_executing() {
    let help = GatesCommand::command().render_help().to_string();
    assert!(help.contains("without starting a gate"), "{help}");
    assert!(help.contains("doctor"), "{help}");
}

#[test]
fn list_projection_contains_configuration_names_but_no_credential_values() {
    let listing = GateListing::new(
        "safety".to_string(),
        GateSettings {
            command: Some("cake-safety-judge".to_string()),
            args: Some(vec!["serve".to_string()]),
            mode: Some("enforce".to_string()),
            env: Some(vec!["SAFETY_KEY".to_string()]),
            models: Some(vec!["default".to_string()]),
            ..GateSettings::default()
        },
    );
    let value: Value = serde_json::to_value(listing).unwrap();
    assert_eq!(value["name"], "safety");
    assert_eq!(value["command"], "cake-safety-judge");
    assert_eq!(value["env"][0], "SAFETY_KEY");
    assert!(value.get("credential").is_none());
}

#[test]
fn list_reports_disabled_legacy_configuration_without_starting_a_gate() {
    let loaded = minimal_settings(GateConfiguration::default());
    let mut loaded = loaded;
    loaded.judge.enabled = false;
    assert!(loaded.gates.bypassed(&loaded.judge, false));
}

fn minimal_settings(gates: GateConfiguration) -> LoadedSettings {
    LoadedSettings {
        gates,
        models: std::collections::HashMap::default(),
        default_model: None,
        directories: Vec::new(),
        sandbox: crate::config::settings::SandboxSettings::default(),
        skills: crate::config::settings::SkillSettings::default(),
        tools_enabled: None,
        system_prompt: None,
        judge: crate::config::settings::JudgeSettings::default(),
        limits: crate::config::settings::ResolvedLimits::default(),
        warnings: Vec::new(),
    }
}

fn selected_configuration() -> GateConfiguration {
    GateConfiguration {
        definitions: std::collections::BTreeMap::from([(
            "safety".to_string(),
            GateSettings {
                command: Some("cake-safety-judge".to_string()),
                mode: Some("enforce".to_string()),
                transport: Some("stdio".to_string()),
                ..GateSettings::default()
            },
        )]),
        selection: Some(vec!["safety".to_string()]),
        legacy_present: false,
    }
}

#[test]
fn list_gates_reports_the_selection_and_honors_bypass() {
    let loaded = minimal_settings(selected_configuration());
    let (bypassed, gates) = list_gates(&loaded, false);
    assert!(!bypassed);
    assert_eq!(gates.len(), 1);
    assert_eq!(gates[0].name, "safety");
    assert_eq!(gates[0].command.as_deref(), Some("cake-safety-judge"));

    let (bypassed, gates) = list_gates(&loaded, true);
    assert!(bypassed);
    assert!(gates.is_empty());
}

#[test]
fn list_rendering_reports_bypass_and_gate_projection() {
    let loaded = minimal_settings(selected_configuration());
    let (_, gates) = list_gates(&loaded, false);
    let text = render_list_text(false, &gates);
    assert!(text.contains("safety: command=cake-safety-judge"));
    assert!(render_list_text(true, &gates).contains("bypassed"));

    let document: Value =
        serde_json::from_str(&render_list_json(&loaded, false, &gates).unwrap()).unwrap();
    assert_eq!(document["summary"]["count"], 1);
    assert_eq!(document["data"]["gates"][0]["name"], "safety");
}

fn startup(
    gate: &str,
    outcome: &str,
    failure_class: Option<&str>,
) -> crate::session_telemetry::GateStartupTelemetry {
    crate::session_telemetry::GateStartupTelemetry {
        gate: gate.to_string(),
        version: Some("1.0.0".to_string()),
        mode: "enforce".to_string(),
        phases: vec!["pre_tool".to_string()],
        actions: vec!["bash.exec".to_string()],
        outcome: outcome.to_string(),
        failure_class: failure_class.map(str::to_string),
        latency_ms: 3,
    }
}

#[test]
fn doctor_rendering_reports_ready_failed_bypass_and_empty() {
    let records = [
        startup("safety", "ready", None),
        startup("broken", "failed", Some("config")),
    ];
    let text = render_doctor_text(false, &records);
    assert!(text.contains("safety: ready"));
    assert!(text.contains("broken: failed"));
    assert!(render_doctor_text(true, &records).contains("bypassed"));
    assert!(render_doctor_text(false, &[]).contains("No configured gates"));

    let loaded = minimal_settings(GateConfiguration::default());
    let document: Value =
        serde_json::from_str(&render_doctor_json(&loaded, false, &records).unwrap()).unwrap();
    assert_eq!(document["summary"]["count"], 2);
    assert_eq!(document["data"]["gates"][1]["failure_class"], "config");
}

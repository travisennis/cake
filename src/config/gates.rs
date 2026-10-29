//! Trusted gate configuration and legacy settings migration. No policy lives here.
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::gates::protocol::{MAX_ENV_NAMES, MAX_INIT_CONFIG_BYTES};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GateSettings {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub transport: Option<String>,
    pub mode: Option<String>,
    pub timeout_secs: Option<u64>,
    pub startup_timeout_secs: Option<u64>,
    pub fail: Option<String>,
    pub short_circuit_on_allow: Option<bool>,
    pub allowlist: Option<Vec<String>>,
    pub env: Option<Vec<String>>,
    pub models: Option<Vec<String>>,
    pub config: Option<serde_json::Value>,
}

impl GateSettings {
    fn overlay(&mut self, other: Self) {
        macro_rules! overlay {
            ($($field:ident),*) => { $(if other.$field.is_some() { self.$field = other.$field; })* };
        }
        overlay!(
            command,
            args,
            transport,
            mode,
            timeout_secs,
            startup_timeout_secs,
            fail,
            short_circuit_on_allow,
            allowlist,
            env,
            models
        );
        if let Some(config) = other.config {
            merge_config(
                self.config.get_or_insert_with(|| serde_json::json!({})),
                config,
            );
        }
    }
}

fn merge_config(target: &mut serde_json::Value, incoming: serde_json::Value) {
    if let (Some(target), Some(incoming)) = (target.as_object_mut(), incoming.as_object()) {
        for (key, value) in incoming {
            merge_config(
                target.entry(key.clone()).or_insert(serde_json::Value::Null),
                value.clone(),
            );
        }
    } else {
        *target = incoming;
    }
}

#[derive(Debug, Clone, Default)]
pub struct GateConfiguration {
    pub definitions: BTreeMap<String, GateSettings>,
    pub selection: Option<Vec<String>>,
    pub legacy_present: bool,
}

impl GateConfiguration {
    /// Validate the explicit selection before any gate is started.
    ///
    /// An absent selection is resolved from the legacy judge settings or the
    /// auto-detected safety gate, so it has no table to validate here. Tables
    /// that are configured but not selected are ignored by design.
    pub fn validate(&self) -> Result<(), String> {
        let Some(names) = &self.selection else {
            return Ok(());
        };
        let mut seen = BTreeSet::new();
        for name in names {
            if name.trim().is_empty() {
                return Err("gate selection contains an empty name".to_string());
            }
            if name == "builtin-safety" {
                return Err(
                    "builtin-safety was removed; select an external gate such as cake-safety-judge"
                        .to_string(),
                );
            }
            if !seen.insert(name) {
                return Err(format!("gate '{name}' is selected more than once"));
            }
            let settings = self
                .definitions
                .get(name)
                .ok_or_else(|| format!("selected gate '{name}' has no [gates.{name}] table"))?;
            validate_gate(name, settings)?;
        }
        Ok(())
    }

    pub fn merge(&mut self, definitions: BTreeMap<String, GateSettings>) {
        for (name, settings) in definitions {
            self.definitions.entry(name).or_default().overlay(settings);
        }
    }

    pub fn bypassed(&self, legacy: &super::settings::JudgeSettings, no_gate: bool) -> bool {
        no_gate || environment_bypass() || (self.selection.is_none() && !legacy.enabled)
    }

    pub fn selected(&self, legacy: &super::settings::JudgeSettings) -> Vec<(String, GateSettings)> {
        if let Some(names) = &self.selection {
            return names
                .iter()
                .map(|name| {
                    (
                        name.clone(),
                        self.definitions.get(name).cloned().unwrap_or_default(),
                    )
                })
                .collect();
        }
        // Keep explicit legacy tuning, including allowlists, while moving its
        // execution to the plugin. No credential or policy is copied into Cake.
        let mut settings = GateSettings {
            command: Some("cake-safety-judge".into()),
            models: Some(vec![
                legacy.model.clone().unwrap_or_else(|| "default".into()),
            ]),
            timeout_secs: Some(legacy.timeout_secs.saturating_add(legacy.retry_budget_secs)),
            ..GateSettings::default()
        };
        if self.legacy_present {
            settings.allowlist = Some(legacy.allowlist.clone());
            settings.env = Some(vec!["TYPESAFE_AI_API_KEY".into()]);
            settings.config = Some(serde_json::json!({
                "model": legacy.model.as_deref().unwrap_or("default"),
                "timeout_secs": legacy.timeout_secs,
                "retry_budget_secs": legacy.retry_budget_secs,
                "rubric_file": legacy.rubric_file,
                "typesafe": {"mode": legacy.typesafe.mode, "model": legacy.typesafe.model, "timeout_ms": legacy.typesafe.timeout_ms}
            }));
        }
        vec![("safety".into(), settings)]
    }
}

pub fn environment_bypass() -> bool {
    std::env::var("CAKE_GATE")
        .or_else(|_| std::env::var("CAKE_JUDGE"))
        .as_deref()
        == Ok("off")
}

fn validate_gate(name: &str, settings: &GateSettings) -> Result<(), String> {
    if settings.command.as_deref().is_none_or(str::is_empty) {
        return Err(format!("gate '{name}' has no command"));
    }
    validate_gate_enums(name, settings)?;
    validate_gate_timeouts(name, settings)?;
    validate_gate_env(name, settings)?;
    validate_gate_config(name, settings)
}

fn validate_gate_enums(name: &str, settings: &GateSettings) -> Result<(), String> {
    if let Some(transport) = settings.transport.as_deref()
        && !matches!(transport, "stdio" | "oneshot")
    {
        return Err(format!(
            "gate '{name}' has unknown transport '{transport}'; use stdio or oneshot"
        ));
    }
    if let Some(mode) = settings.mode.as_deref()
        && !matches!(mode, "enforce" | "shadow" | "off")
    {
        return Err(format!(
            "gate '{name}' has unknown mode '{mode}'; use enforce, shadow, or off"
        ));
    }
    if let Some(fail) = settings.fail.as_deref()
        && !matches!(fail, "closed" | "open")
    {
        return Err(format!(
            "gate '{name}' has unknown failure mode '{fail}'; use closed or open"
        ));
    }
    Ok(())
}

fn validate_gate_timeouts(name: &str, settings: &GateSettings) -> Result<(), String> {
    for (field, value) in [
        ("timeout_secs", settings.timeout_secs),
        ("startup_timeout_secs", settings.startup_timeout_secs),
    ] {
        if value == Some(0) {
            return Err(format!("gate '{name}' {field} must be at least 1"));
        }
    }
    Ok(())
}

fn validate_gate_env(name: &str, settings: &GateSettings) -> Result<(), String> {
    let Some(env) = &settings.env else {
        return Ok(());
    };
    if env.len() > MAX_ENV_NAMES {
        return Err(format!(
            "gate '{name}' env has more than {MAX_ENV_NAMES} names"
        ));
    }
    let mut seen = BTreeSet::new();
    for variable in env {
        if !is_environment_name(variable) {
            return Err(format!(
                "gate '{name}' env contains invalid variable name '{variable}'"
            ));
        }
        if !seen.insert(variable) {
            return Err(format!(
                "gate '{name}' env lists '{variable}' more than once"
            ));
        }
    }
    Ok(())
}

fn validate_gate_config(name: &str, settings: &GateSettings) -> Result<(), String> {
    let Some(config) = &settings.config else {
        return Ok(());
    };
    let bytes = serde_json::to_vec(config).map_or(usize::MAX, |config| config.len());
    if bytes > MAX_INIT_CONFIG_BYTES {
        return Err(format!(
            "gate '{name}' config exceeds {MAX_INIT_CONFIG_BYTES} bytes"
        ));
    }
    Ok(())
}

fn is_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(settings: GateSettings) -> GateConfiguration {
        GateConfiguration {
            definitions: BTreeMap::from([("safety".to_string(), settings)]),
            selection: Some(vec!["safety".to_string()]),
            legacy_present: false,
        }
    }

    #[test]
    fn config_merges_recursively_and_lists_replace() {
        let mut configuration = GateConfiguration::default();
        configuration.merge(BTreeMap::from([(
            "safety".to_string(),
            GateSettings {
                allowlist: Some(vec!["global".to_string()]),
                env: Some(vec!["GLOBAL".to_string()]),
                config: Some(serde_json::json!({"a": 1, "nested": {"b": 2, "c": 3}})),
                ..GateSettings::default()
            },
        )]));
        configuration.merge(BTreeMap::from([(
            "safety".to_string(),
            GateSettings {
                allowlist: Some(vec!["project".to_string()]),
                env: Some(vec!["PROJECT".to_string()]),
                config: Some(serde_json::json!({"nested": {"b": 4, "d": 5}})),
                ..GateSettings::default()
            },
        )]));

        let settings = configuration.definitions.get("safety").expect("merged");
        assert_eq!(
            settings.allowlist.as_deref(),
            Some(["project".to_string()].as_slice())
        );
        assert_eq!(
            settings.env.as_deref(),
            Some(["PROJECT".to_string()].as_slice())
        );
        assert_eq!(
            settings.config,
            Some(serde_json::json!({"a": 1, "nested": {"b": 4, "c": 3, "d": 5}}))
        );
    }

    #[test]
    fn validate_rejects_too_many_environment_names() {
        let settings = selected(GateSettings {
            command: Some("cake-safety-judge".to_string()),
            env: Some(
                (0..=MAX_ENV_NAMES)
                    .map(|index| format!("VAR_{index}"))
                    .collect(),
            ),
            ..GateSettings::default()
        });
        let error = settings.validate().expect_err("env bound");
        assert!(error.contains("more than"), "{error}");
    }

    #[test]
    fn validate_rejects_oversized_gate_config() {
        let settings = selected(GateSettings {
            command: Some("cake-safety-judge".to_string()),
            config: Some(serde_json::json!({"blob": "x".repeat(MAX_INIT_CONFIG_BYTES)})),
            ..GateSettings::default()
        });
        let error = settings.validate().expect_err("config bound");
        assert!(error.contains("config exceeds"), "{error}");
    }
}

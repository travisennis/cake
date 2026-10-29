//! Resolve and start external gates before exposing Bash for a run.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::gates::{GateConfiguration, GateSettings};
use crate::config::settings::JudgeSettings;
use crate::config::{ModelConfig, ModelDefinition};
use crate::gates::composer::{
    FailMode, GateFailure, GateMode, GatePipeline, GatePipelineError, GateStageConfig, GateStartup,
};
use crate::gates::protocol::{InitFrame, InitSession, ModelDescriptor};
use crate::gates::runner::{GateProcess, GateProcessSpec, ProcessGate};

pub async fn start(
    configuration: &GateConfiguration,
    legacy: &JudgeSettings,
    models: &std::collections::HashMap<String, ModelDefinition>,
    default_model: Option<&ModelConfig>,
    cwd: &Path,
    no_gate: bool,
) -> Result<GatePipeline, GatePipelineError> {
    if configuration.bypassed(legacy, no_gate) {
        return Ok(GatePipeline::bypass());
    }
    let mut startups = Vec::new();
    for (name, settings) in configuration.selected(legacy) {
        if settings.mode.as_deref() == Some("off") {
            continue;
        }
        let started = Instant::now();
        let config =
            stage_config(&name, &settings).map_err(|failure| startup_error(&name, failure))?;
        let gate = start_gate(&settings, models, default_model, cwd).await;
        startups.push(GateStartup {
            config,
            gate: gate.map(|gate| Arc::new(gate) as Arc<dyn crate::gates::composer::Gate>),
            latency: started.elapsed(),
        });
    }
    GatePipeline::start(startups)
}

fn startup_error(name: &str, failure: GateFailure) -> GatePipelineError {
    GatePipelineError::Startup {
        gate: name.into(),
        startup: Box::new(crate::session_telemetry::GateStartupTelemetry {
            gate: name.into(),
            version: None,
            mode: "enforce".into(),
            outcome: "failed".into(),
            phases: vec![],
            actions: vec![],
            failure_class: Some(failure.class.to_string()),
            latency_ms: 0,
        }),
        failure,
    }
}

fn stage_config(name: &str, settings: &GateSettings) -> Result<GateStageConfig, GateFailure> {
    if name == "builtin-safety" {
        return Err(GateFailure::config(
            "builtin-safety was removed; configure cake-safety-judge",
        ));
    }
    let mode = match settings.mode.as_deref().unwrap_or("enforce") {
        "enforce" => GateMode::Enforce,
        "shadow" => GateMode::Shadow,
        _ => {
            return Err(GateFailure::config(
                "gate mode must be enforce, shadow, or off",
            ));
        },
    };
    let fail = match settings.fail.as_deref().unwrap_or("closed") {
        "closed" => FailMode::Closed,
        "open" => FailMode::Open,
        _ => return Err(GateFailure::config("gate fail must be closed or open")),
    };
    Ok(GateStageConfig {
        name: name.into(),
        mode,
        fail,
        short_circuit_on_allow: settings.short_circuit_on_allow.unwrap_or(false),
        allowlist: settings.allowlist.clone().unwrap_or_default(),
    })
}

async fn start_gate(
    settings: &GateSettings,
    models: &std::collections::HashMap<String, ModelDefinition>,
    default_model: Option<&ModelConfig>,
    cwd: &Path,
) -> Result<ProcessGate, GateFailure> {
    let command = settings
        .command
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GateFailure::config("selected gate has no command"))?;
    let oneshot = match settings.transport.as_deref().unwrap_or("stdio") {
        "stdio" => false,
        "oneshot" => true,
        _ => {
            return Err(GateFailure::config(
                "gate transport must be stdio or oneshot",
            ));
        },
    };
    let mut init = InitFrame::new(InitSession {
        session_id: None,
        task_id: None,
        invocation_id: None,
        cwd: cwd.to_string_lossy().into_owned(),
    });
    init.config.clone_from(&settings.config);
    init.env = settings.env.clone().unwrap_or_default();
    let descriptors = resolve_models(settings, models, default_model)?;
    init.env.extend(
        descriptors
            .values()
            .map(|model| model.api_key_env.clone())
            .filter(|name| !name.is_empty()),
    );
    init.env.sort();
    init.env.dedup();
    let environment = init
        .env
        .iter()
        .filter_map(|name| std::env::var(name).ok().map(|value| (name.clone(), value)))
        .collect();
    init.models = Some(descriptors);
    let spec = GateProcessSpec::new(command)
        .with_cwd(cwd.into())
        .with_args(settings.args.clone().unwrap_or_default())
        .with_environment(environment)
        .with_timeouts(
            deadline(settings.startup_timeout_secs, 10)?,
            deadline(settings.timeout_secs, 30)?,
        );
    let gate = ProcessGate::start(init, GateProcess::new(spec)).await?;
    if oneshot {
        gate.with_oneshot()
    } else {
        Ok(gate)
    }
}

fn deadline(seconds: Option<u64>, default: u64) -> Result<Duration, GateFailure> {
    let seconds = seconds.unwrap_or(default);
    if seconds == 0 {
        return Err(GateFailure::config("gate deadlines must be positive"));
    }
    Ok(Duration::from_secs(seconds))
}

fn resolve_models(
    settings: &GateSettings,
    models: &std::collections::HashMap<String, ModelDefinition>,
    default: Option<&ModelConfig>,
) -> Result<BTreeMap<String, ModelDescriptor>, GateFailure> {
    let mut descriptors = BTreeMap::new();
    for name in settings.models.as_deref().unwrap_or_default() {
        let Some(model) = selected_model(name, models, default)? else {
            continue;
        };
        descriptors.insert(name.clone(), descriptor(name, &model)?);
    }
    Ok(descriptors)
}

fn selected_model(
    name: &str,
    models: &std::collections::HashMap<String, ModelDefinition>,
    default: Option<&ModelConfig>,
) -> Result<Option<ModelConfig>, GateFailure> {
    if name != "default" {
        return Ok(Some(
            models
                .get(name)
                .ok_or_else(|| GateFailure::config(format!("unknown gate model '{name}'")))?
                .to_model_config(),
        ));
    }
    let Some(model) = default else {
        return Ok(None);
    };
    // Subscription credentials never cross the process boundary.
    if crate::auth::is_chatgpt_codex_backend(&model.base_url) {
        return Ok(None);
    }
    Ok(Some(model.clone()))
}

fn descriptor(name: &str, model: &ModelConfig) -> Result<ModelDescriptor, GateFailure> {
    if crate::auth::is_chatgpt_codex_backend(&model.base_url) {
        return Err(GateFailure::config(
            "gate models must use environment-backed credentials",
        ));
    }
    if model.top_k.is_some() {
        return Err(GateFailure::config(format!(
            "gate model '{name}' sets top_k, which cake-gate/1 descriptors cannot carry"
        )));
    }
    if !model.providers.is_empty() {
        return Err(GateFailure::config(format!(
            "gate model '{name}' sets provider routing, which cake-gate/1 descriptors cannot carry"
        )));
    }
    if std::env::var(&model.api_key_env)
        .unwrap_or_default()
        .is_empty()
    {
        return Err(GateFailure::config(format!(
            "missing gate credential environment variable '{}'",
            model.api_key_env
        )));
    }
    let value =
        serde_json::to_value(model).map_err(|error| GateFailure::config(error.to_string()))?;
    serde_json::from_value(value)
        .map_err(|error| GateFailure::config(format!("invalid model descriptor: {error}")))
}

#[cfg(all(test, unix))]
#[path = "runtime_tests.rs"]
mod tests;

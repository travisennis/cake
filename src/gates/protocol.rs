//! `cake-gate/1` wire types and bounds.
//!
//! This module mirrors `docs/schemas/cake-gate-1.schema.json`. Unknown fields
//! are tolerated, capabilities and error classes are open strings, and the
//! closed enums and cardinality bounds are the version's known set. The byte and
//! character limits JSON Schema cannot express are enforced here and by the
//! runner: one frame is at most 1 MiB, model-visible gate text is sanitized and
//! capped at 1,000 characters, and evidence is bounded before telemetry.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only protocol version Cake speaks.
pub const PROTOCOL_VERSION: &str = "cake-gate/1";

/// Maximum bytes in one newline-delimited frame, excluding the newline.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Maximum characters of gate text that may reach the model.
pub const MAX_MODEL_TEXT_CHARS: usize = 1_000;

/// Maximum characters retained for a gate error message.
pub const MAX_ERROR_MESSAGE_CHARS: usize = 1_024;

/// Maximum `ready.warnings` entries.
pub const MAX_READY_WARNINGS: usize = 8;

/// Maximum model-visible annotations accepted from one decision.
pub const MAX_ANNOTATIONS: usize = 8;

/// Maximum evidence items accepted from one decision.
pub const MAX_EVIDENCE_ITEMS: usize = 8;

/// Maximum serialized bytes of one evidence payload.
pub const MAX_EVIDENCE_ITEM_BYTES: usize = 8 * 1024;

/// Maximum serialized bytes of all evidence from one decision.
pub const MAX_EVIDENCE_TOTAL_BYTES: usize = 32 * 1024;

/// Maximum serialized bytes of the gate-owned `init.config` table.
pub const MAX_INIT_CONFIG_BYTES: usize = 64 * 1024;

/// Maximum environment names passed to a gate.
pub const MAX_ENV_NAMES: usize = 32;

/// Maximum characters in an identifier or gate code.
pub const MAX_IDENTIFIER_CHARS: usize = 64;

/// Maximum characters of a script-evidence payload.
pub const MAX_SCRIPT_EVIDENCE_CHARS: usize = 32_768;

/// Phases a gate can declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatePhase {
    PreTool,
    PostTool,
}

impl GatePhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreTool => "pre_tool",
            Self::PostTool => "post_tool",
        }
    }
}

/// Actions a gate can declare. `bash.exec` is the only configured action in
/// this release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GateAction {
    #[serde(rename = "bash.exec")]
    BashExec,
}

impl GateAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BashExec => "bash.exec",
        }
    }
}

/// Transports a gate can declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateTransport {
    Stdio,
    Oneshot,
}

/// A gate decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateDecision {
    Allow,
    Warn,
    Block,
    Abstain,
}

impl GateDecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Warn => "warn",
            Self::Block => "block",
            Self::Abstain => "abstain",
        }
    }
}

/// Frame discriminator carried as the JSON `type` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameType {
    Init,
    Ready,
    Check,
    Decision,
    Error,
    Shutdown,
}

/// A gate's `describe` object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Describe {
    pub protocol: String,
    pub name: String,
    pub version: String,
    pub phases: Vec<GatePhase>,
    pub actions: Vec<GateAction>,
    pub transports: Vec<GateTransport>,
    pub capabilities: Vec<String>,
}

impl Describe {
    /// Validate the protocol, naming rules, required non-empty lists, and the
    /// `stdio` transport requirement. Unknown capabilities are accepted.
    pub fn validate(&self) -> Result<(), String> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(format!(
                "gate '{}' speaks '{}', expected '{PROTOCOL_VERSION}'",
                self.name, self.protocol
            ));
        }
        if !is_valid_gate_name(&self.name) {
            return Err(format!("invalid gate name '{}'", self.name));
        }
        if !is_valid_gate_version(&self.version) {
            return Err(format!(
                "gate '{}' has invalid version '{}'",
                self.name, self.version
            ));
        }
        self.validate_lists()
    }

    fn validate_lists(&self) -> Result<(), String> {
        if self.phases.is_empty() {
            return Err("describe.phases must not be empty".to_string());
        }
        if self.actions.is_empty() {
            return Err("describe.actions must not be empty".to_string());
        }
        if self.transports.is_empty() {
            return Err("describe.transports must not be empty".to_string());
        }
        if !self.transports.contains(&GateTransport::Stdio) {
            return Err("describe.transports must include 'stdio'".to_string());
        }
        self.validate_unique_lists()?;
        for capability in &self.capabilities {
            if !is_valid_capability(capability) {
                return Err(format!("invalid capability '{capability}'"));
            }
        }
        Ok(())
    }

    fn validate_unique_lists(&self) -> Result<(), String> {
        if has_duplicates(&self.phases)
            || has_duplicates(&self.actions)
            || has_duplicates(&self.transports)
            || has_duplicates(&self.capabilities)
        {
            return Err("describe lists must not contain duplicates".to_string());
        }
        Ok(())
    }

    /// Whether the gate declares the phase/action pair Cake configures.
    pub fn covers(&self, phase: GatePhase, action: GateAction) -> bool {
        self.phases.contains(&phase) && self.actions.contains(&action)
    }
}

/// Gate session identifiers carried by `init`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitSession {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<String>,
    pub cwd: String,
}

/// A resolved model descriptor. It carries the credential environment variable
/// name, never its value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub base_url: String,
    pub api_type: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_headers: Option<ProviderHeaders>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_max_tokens: Option<u32>,
    pub api_key_env: String,
}

/// The only provider attribution headers a descriptor may carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderHeaders {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_referer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_title: Option<String>,
}

/// The `init` frame sent once after a gate process starts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitFrame {
    pub protocol: String,
    #[serde(rename = "type")]
    pub frame_type: FrameType,
    pub id: String,
    pub session: InitSession,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<std::collections::BTreeMap<String, ModelDescriptor>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
}

impl InitFrame {
    pub const INIT_ID: &'static str = "init";

    pub fn new(session: InitSession) -> Self {
        Self {
            protocol: PROTOCOL_VERSION.to_string(),
            frame_type: FrameType::Init,
            id: Self::INIT_ID.to_string(),
            session,
            config: None,
            models: None,
            env: Vec::new(),
        }
    }

    pub fn with_config(mut self, config: Value) -> Self {
        self.config = Some(config);
        self
    }

    pub fn with_models(
        mut self,
        models: std::collections::BTreeMap<String, ModelDescriptor>,
    ) -> Self {
        self.models = Some(models);
        self
    }

    pub fn with_env(mut self, env: Vec<String>) -> Self {
        self.env = env;
        self
    }
}

/// The `ready` reply to `init`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadyFrame {
    pub protocol: String,
    #[serde(rename = "type")]
    pub frame_type: FrameType,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl ReadyFrame {
    pub fn new(name: String, version: String) -> Self {
        Self {
            protocol: PROTOCOL_VERSION.to_string(),
            frame_type: FrameType::Ready,
            id: InitFrame::INIT_ID.to_string(),
            name,
            version,
            warnings: Vec::new(),
        }
    }
}

/// One collected script's untrusted contents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptEvidence {
    pub path: String,
    pub contents: String,
}

/// The Bash action input carried by a check frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BashInput {
    pub command: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_evidence: Vec<ScriptEvidence>,
}

impl BashInput {
    pub const fn new(command: String, cwd: String) -> Self {
        Self {
            command,
            cwd,
            reason: None,
            repo_digest: None,
            script_evidence: Vec::new(),
        }
    }
}

/// Correlation context for a check. None of these identifiers authorize an
/// action.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CheckSession {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u32>,
}

/// Options attached to a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CheckOptions {
    pub diagnostic: bool,
}

/// A `pre_tool` `bash.exec` check frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckFrame {
    pub protocol: String,
    #[serde(rename = "type")]
    pub frame_type: FrameType,
    pub id: String,
    pub phase: GatePhase,
    pub action: GateAction,
    pub input: BashInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<CheckSession>,
    pub options: CheckOptions,
}

impl CheckFrame {
    pub fn new(id: String, input: BashInput) -> Self {
        Self {
            protocol: PROTOCOL_VERSION.to_string(),
            frame_type: FrameType::Check,
            id,
            phase: GatePhase::PreTool,
            action: GateAction::BashExec,
            input,
            session: None,
            options: CheckOptions::default(),
        }
    }

    pub fn with_session(mut self, session: CheckSession) -> Self {
        self.session = Some(session);
        self
    }

    pub const fn with_diagnostic(mut self, diagnostic: bool) -> Self {
        self.options = CheckOptions { diagnostic };
        self
    }
}

/// Placement for a model-visible annotation. Only `prepend` exists in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationPlacement {
    Prepend,
}

/// One model-visible line supplied by a gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateAnnotation {
    pub placement: AnnotationPlacement,
    pub text: String,
}

impl GateAnnotation {
    pub const fn prepend(text: String) -> Self {
        Self {
            placement: AnnotationPlacement::Prepend,
            text,
        }
    }
}

/// One gate-declared evidence item. Evidence is the only gate-controlled data
/// that reaches telemetry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateEvidence {
    pub kind: String,
    pub data: Value,
}

/// A gate's decision reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionFrame {
    pub protocol: String,
    #[serde(rename = "type")]
    pub frame_type: FrameType,
    pub id: String,
    pub decision: GateDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub annotations: Vec<GateAnnotation>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub short_circuit: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<GateEvidence>,
}

impl DecisionFrame {
    pub fn new(id: String, decision: GateDecision) -> Self {
        Self {
            protocol: PROTOCOL_VERSION.to_string(),
            frame_type: FrameType::Decision,
            id,
            decision,
            code: None,
            message: String::new(),
            confidence: None,
            annotations: Vec::new(),
            short_circuit: false,
            evidence: Vec::new(),
        }
    }

    pub fn with_code(mut self, code: String) -> Self {
        self.code = Some(code);
        self
    }

    pub fn with_message(mut self, message: String) -> Self {
        self.message = message;
        self
    }

    pub const fn with_confidence(mut self, confidence: f32) -> Self {
        self.confidence = Some(confidence);
        self
    }

    pub fn with_annotations(mut self, annotations: Vec<GateAnnotation>) -> Self {
        self.annotations = annotations;
        self
    }

    pub const fn with_short_circuit(mut self, short_circuit: bool) -> Self {
        self.short_circuit = short_circuit;
        self
    }

    pub fn with_evidence(mut self, evidence: Vec<GateEvidence>) -> Self {
        self.evidence = evidence;
        self
    }

    /// Validate protocol, id matching, the block/warn requirements, code shape,
    /// and confidence range. Unknown fields were already ignored by serde.
    pub fn validate(&self, expected_id: &str) -> Result<(), String> {
        self.validate_header(expected_id)?;
        self.validate_decision_fields()?;
        self.validate_confidence()
    }

    fn validate_header(&self, expected_id: &str) -> Result<(), String> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(format!(
                "decision speaks '{}', expected '{PROTOCOL_VERSION}'",
                self.protocol
            ));
        }
        if self.frame_type != FrameType::Decision {
            return Err("decision frame has the wrong type".to_string());
        }
        if self.id != expected_id {
            return Err(format!(
                "decision id '{}' does not match request '{expected_id}'",
                self.id
            ));
        }
        if let Some(code) = &self.code
            && !is_valid_code(code)
        {
            return Err(format!("invalid decision code '{code}'"));
        }
        Ok(())
    }

    fn validate_decision_fields(&self) -> Result<(), String> {
        if !matches!(self.decision, GateDecision::Block | GateDecision::Warn) {
            return Ok(());
        }
        if self.code.as_deref().is_none_or(str::is_empty) {
            return Err(format!(
                "a '{}' decision requires a code",
                self.decision.as_str()
            ));
        }
        if self.message.is_empty() {
            return Err(format!(
                "a '{}' decision requires a message",
                self.decision.as_str()
            ));
        }
        Ok(())
    }

    fn validate_confidence(&self) -> Result<(), String> {
        if let Some(confidence) = self.confidence
            && (!confidence.is_finite() || !(0.0..=1.0).contains(&confidence))
        {
            return Err("decision.confidence must be a finite number in 0.0..=1.0".to_string());
        }
        Ok(())
    }
}

/// A gate-declared failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorFrame {
    pub protocol: String,
    #[serde(rename = "type")]
    pub frame_type: FrameType,
    pub id: String,
    pub class: GateErrorClass,
    pub message: String,
}

impl ErrorFrame {
    pub fn new(id: String, class: GateErrorClass, message: String) -> Self {
        Self {
            protocol: PROTOCOL_VERSION.to_string(),
            frame_type: FrameType::Error,
            id,
            class,
            message,
        }
    }
}

/// The frame sent at session end. Shutdown is best effort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShutdownFrame {
    pub protocol: String,
    #[serde(rename = "type")]
    pub frame_type: FrameType,
    pub id: String,
}

impl ShutdownFrame {
    pub const SHUTDOWN_ID: &'static str = "shutdown";

    pub fn new() -> Self {
        Self {
            protocol: PROTOCOL_VERSION.to_string(),
            frame_type: FrameType::Shutdown,
            id: Self::SHUTDOWN_ID.to_string(),
        }
    }
}

impl Default for ShutdownFrame {
    fn default() -> Self {
        Self::new()
    }
}

/// A bounded gate failure class.
///
/// The known values are closed in the contract; `Other` keeps the wire value so
/// diagnostics do not lose it, and [`Self::normalized`] maps it to `protocol`
/// before composition or telemetry. `Crash` is assigned by Cake when a process
/// exits; a gate never sends it in an error frame.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GateErrorClass {
    Timeout,
    Network,
    Auth,
    RateLimit,
    Transport,
    Malformed,
    Refusal,
    Config,
    Unsupported,
    Internal,
    Protocol,
    Crash,
    Other(String),
}

impl GateErrorClass {
    pub fn from_wire(value: &str) -> Self {
        KNOWN_ERROR_CLASSES
            .iter()
            .find(|(wire, _)| *wire == value)
            .map_or_else(
                || Self::Other(value.to_string()),
                |(_, class)| class.clone(),
            )
    }

    /// Treat an unknown class as `protocol`, as the contract requires.
    #[must_use]
    pub fn normalized(self) -> Self {
        match self {
            Self::Other(_) => Self::Protocol,
            known => known,
        }
    }

    pub fn as_str(&self) -> &str {
        if let Self::Other(value) = self {
            return value;
        }
        KNOWN_ERROR_CLASSES
            .iter()
            .find(|(_, class)| class == self)
            .map_or("protocol", |(wire, _)| *wire)
    }
}

impl std::fmt::Display for GateErrorClass {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

const KNOWN_ERROR_CLASSES: [(&str, GateErrorClass); 12] = [
    ("timeout", GateErrorClass::Timeout),
    ("network", GateErrorClass::Network),
    ("auth", GateErrorClass::Auth),
    ("rate_limit", GateErrorClass::RateLimit),
    ("transport", GateErrorClass::Transport),
    ("malformed", GateErrorClass::Malformed),
    ("refusal", GateErrorClass::Refusal),
    ("config", GateErrorClass::Config),
    ("unsupported", GateErrorClass::Unsupported),
    ("internal", GateErrorClass::Internal),
    ("protocol", GateErrorClass::Protocol),
    ("crash", GateErrorClass::Crash),
];

impl Serialize for GateErrorClass {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GateErrorClass {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_wire(&value))
    }
}

/// Sanitize gate text that may become model-visible.
///
/// Control characters except newline, carriage return, and tab are stripped,
/// and the result is capped at [`MAX_MODEL_TEXT_CHARS`].
#[must_use]
pub fn sanitize_model_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .take(MAX_MODEL_TEXT_CHARS)
        .collect()
}

/// Truncate text on a character boundary.
#[must_use]
pub fn bound_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Bound and sanitize a gate error message for diagnostics.
#[must_use]
pub fn bound_error_message(text: &str) -> String {
    bound_chars(&sanitize_control_text(text), MAX_ERROR_MESSAGE_CHARS)
}

/// Strip control characters, keeping the whitespace characters the protocol
/// allows. This is for diagnostic text that is not model-visible.
#[must_use]
pub fn sanitize_control_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\r' | '\t'))
        .collect()
}

/// Serialized byte length of a JSON value, or `usize::MAX` when it cannot be
/// serialized.
#[must_use]
pub fn json_byte_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

/// A gate product name: `[a-z0-9][a-z0-9-]{0,63}`.
#[must_use]
pub fn is_valid_gate_name(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    is_lower_alphanumeric(first)
        && value.chars().count() <= MAX_IDENTIFIER_CHARS
        && characters.all(|character| is_lower_alphanumeric(character) || character == '-')
}

/// A `MAJOR.MINOR.PATCH` version with an optional prerelease suffix.
#[must_use]
pub fn is_valid_gate_version(value: &str) -> bool {
    if value.is_empty() || value.chars().count() > MAX_IDENTIFIER_CHARS {
        return false;
    }
    let (core, prerelease) = value
        .split_once('-')
        .map_or((value, None), |(core, suffix)| (core, Some(suffix)));
    let mut parts = core.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if ![major, minor, patch]
        .iter()
        .all(|part| !part.is_empty() && part.chars().all(|character| character.is_ascii_digit()))
    {
        return false;
    }
    prerelease.is_none_or(|suffix| {
        !suffix.is_empty()
            && suffix.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-')
            })
    })
}

/// A decision code: `[a-z0-9][a-z0-9-]{0,63}`.
#[must_use]
pub fn is_valid_code(value: &str) -> bool {
    is_valid_gate_name(value)
}

/// A capability label. Unknown labels are accepted; malformed ones are not.
#[must_use]
pub fn is_valid_capability(value: &str) -> bool {
    is_valid_gate_name(value)
}

/// An evidence kind: `[a-z0-9][a-z0-9_-]{0,63}`.
#[must_use]
pub fn is_valid_evidence_kind(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    is_lower_alphanumeric(first)
        && value.chars().count() <= MAX_IDENTIFIER_CHARS
        && characters
            .all(|character| is_lower_alphanumeric(character) || matches!(character, '-' | '_'))
}

/// Whether an id fits the schema's `1..=64` character identifier rule.
#[must_use]
pub fn is_valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= MAX_IDENTIFIER_CHARS
}

const fn is_lower_alphanumeric(character: char) -> bool {
    character.is_ascii_lowercase() || character.is_ascii_digit()
}

fn has_duplicates<T: Eq + std::hash::Hash>(items: &[T]) -> bool {
    let mut seen = std::collections::HashSet::with_capacity(items.len());
    items.iter().any(|item| !seen.insert(item))
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;

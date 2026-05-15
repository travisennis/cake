use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Default)]
pub struct LoadedHooks {
    pub groups: Vec<HookGroup>,
}

impl LoadedHooks {
    pub const fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    pub fn matching_groups<'a>(
        &'a self,
        event: HookEvent,
        source: &HookSource,
    ) -> Vec<&'a HookGroup> {
        self.groups
            .iter()
            .filter(|group| group.event == event && group.matcher.matches(source))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct HookGroup {
    pub event: HookEvent,
    pub matcher: HookMatcher,
    pub hooks: Vec<HookCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    Stop,
    ErrorOccurred,
}

impl HookEvent {
    pub const fn has_source(self) -> bool {
        matches!(
            self,
            Self::SessionStart | Self::PreToolUse | Self::PostToolUse | Self::PostToolUseFailure
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::Stop => "Stop",
            Self::ErrorOccurred => "ErrorOccurred",
        }
    }
}

impl fmt::Display for HookEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for HookEvent {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "SessionStart" => Ok(Self::SessionStart),
            "UserPromptSubmit" => Ok(Self::UserPromptSubmit),
            "PreToolUse" => Ok(Self::PreToolUse),
            "PostToolUse" => Ok(Self::PostToolUse),
            "PostToolUseFailure" => Ok(Self::PostToolUseFailure),
            "Stop" => Ok(Self::Stop),
            "ErrorOccurred" => Ok(Self::ErrorOccurred),
            _ => Err(()),
        }
    }
}

/// Typed source for hook matcher dispatch.
///
/// Replaces the previous free-form `Option<&str>` source so that callers and
/// configuration files carry a strongly-typed discriminator. Implements
/// [`Serialize`] and [`Deserialize`] so typed sources can round-trip through
/// JSON in tests and future configuration formats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HookSource {
    /// No source context (e.g. `Stop`, `UserPromptSubmit` events).
    None,
    /// A tool name for `PreToolUse`, `PostToolUse`, and
    /// `PostToolUseFailure` events.
    Tool(String),
    /// A session-start reason for `SessionStart` events.
    SessionStart(String),
}

impl HookSource {
    /// Return the inner string value for logging and session records.
    pub const fn as_display_str(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Tool(name) => Some(name.as_str()),
            Self::SessionStart(reason) => Some(reason.as_str()),
        }
    }

    /// Parse a matcher component string into a `HookSource` based on the event
    /// type. Returns `None` if the value does not parse as a valid source for
    /// the given event.
    fn parse_for_event(value: &str, event: HookEvent) -> Option<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }
        match event {
            HookEvent::SessionStart => Some(Self::SessionStart(trimmed.to_owned())),
            _ => Some(Self::Tool(trimmed.to_owned())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookMatcher {
    All,
    Exact(Vec<HookSource>),
}

impl HookMatcher {
    /// Parse a matcher value from hook configuration.
    ///
    /// `value` is the `"matcher"` field from the config (e.g. `"Bash|Write"`).
    /// `event` provides the context needed to type each component as a
    /// `HookSource` variant. Empty or `"*"` matchers return `All`.
    pub fn parse(value: Option<&str>, event: HookEvent) -> Self {
        let Some(value) = value else {
            return Self::All;
        };
        if value.trim() == "*" {
            return Self::All;
        }

        let sources: Vec<HookSource> = value
            .split('|')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .filter_map(|part| HookSource::parse_for_event(part, event))
            .collect();
        if sources.is_empty() {
            Self::All
        } else {
            Self::Exact(sources)
        }
    }

    pub fn matches(&self, source: &HookSource) -> bool {
        match self {
            Self::All => true,
            Self::Exact(values) => values.iter().any(|v| v == source),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HookCommand {
    pub command: String,
    pub timeout: Duration,
    pub fail_closed: bool,
    pub status_message: Option<String>,
    pub source_path: PathBuf,
}

#[derive(Debug, Error)]
pub enum HooksError {
    #[error("Failed to read hooks file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Failed to parse hooks file {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("Unsupported hooks version in {path}: expected version 1, got {version}")]
    UnsupportedVersion { path: PathBuf, version: u32 },
    #[error("Unknown hook event in {path}: {event}")]
    UnknownEvent { path: PathBuf, event: String },
    #[error("Hook matcher is not supported for event {event} in {path}")]
    MatcherNotSupported { path: PathBuf, event: HookEvent },
    #[error("Unsupported hook type in {path}: expected command, got {hook_type}")]
    UnsupportedHookType { path: PathBuf, hook_type: String },
}

pub struct HooksLoader;

impl HooksLoader {
    pub fn load(project_dir: &Path) -> Result<LoadedHooks, HooksError> {
        let global_path =
            dirs::home_dir().map(|home| home.join(".config").join("cake").join("hooks.json"));
        let paths = [
            global_path,
            Some(project_dir.join(".cake").join("hooks.json")),
            Some(project_dir.join(".cake").join("hooks.local.json")),
        ];
        Self::load_from_paths(paths.iter().flatten().map(PathBuf::as_path))
    }

    pub fn load_from_paths(
        paths: impl IntoIterator<Item = impl AsRef<Path>>,
    ) -> Result<LoadedHooks, HooksError> {
        let mut loaded = LoadedHooks::default();

        for path in paths {
            let path = path.as_ref();
            if !path.exists() {
                continue;
            }

            let content = fs::read_to_string(path).map_err(|source| HooksError::Read {
                path: path.to_path_buf(),
                source,
            })?;
            let hook_file: HookFile =
                serde_json::from_str(&content).map_err(|source| HooksError::Parse {
                    path: path.to_path_buf(),
                    source,
                })?;

            if hook_file.version != 1 {
                return Err(HooksError::UnsupportedVersion {
                    path: path.to_path_buf(),
                    version: hook_file.version,
                });
            }

            for (event_name, entries) in hook_file.hooks {
                let event =
                    event_name
                        .parse::<HookEvent>()
                        .map_err(|()| HooksError::UnknownEvent {
                            path: path.to_path_buf(),
                            event: event_name.clone(),
                        })?;

                for entry in entries {
                    if entry.matcher.is_some() && !event.has_source() {
                        return Err(HooksError::MatcherNotSupported {
                            path: path.to_path_buf(),
                            event,
                        });
                    }

                    let hooks = entry
                        .hooks
                        .into_iter()
                        .map(|hook| {
                            if hook.hook_type != "command" {
                                return Err(HooksError::UnsupportedHookType {
                                    path: path.to_path_buf(),
                                    hook_type: hook.hook_type,
                                });
                            }

                            let timeout = hook.timeout.unwrap_or(60).clamp(1, 600);
                            Ok(HookCommand {
                                command: hook.command,
                                timeout: Duration::from_secs(timeout),
                                fail_closed: hook.fail_closed.unwrap_or(false),
                                status_message: hook.status_message,
                                source_path: path.to_path_buf(),
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;

                    loaded.groups.push(HookGroup {
                        event,
                        matcher: HookMatcher::parse(entry.matcher.as_deref(), event),
                        hooks,
                    });
                }
            }
        }

        Ok(loaded)
    }
}

#[derive(Debug, Deserialize)]
struct HookFile {
    version: u32,
    hooks: BTreeMap<String, Vec<HookMatcherConfig>>,
}

#[derive(Debug, Deserialize)]
struct HookMatcherConfig {
    matcher: Option<String>,
    hooks: Vec<HookCommandConfig>,
}

#[derive(Debug, Deserialize)]
struct HookCommandConfig {
    #[serde(rename = "type")]
    hook_type: String,
    command: String,
    timeout: Option<u64>,
    fail_closed: Option<bool>,
    status_message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn missing_hook_files_load_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let loaded = HooksLoader::load_from_paths([tmp.path().join("missing.json")].iter())
            .expect("missing files should be ignored");

        assert!(loaded.is_empty());
    }

    #[test]
    fn loads_global_project_and_local_in_order() {
        let tmp = tempfile::TempDir::new().unwrap();
        let global = tmp.path().join("global.json");
        let project = tmp.path().join("project.json");
        let local = tmp.path().join("local.json");
        let body = |command: &str| {
            format!(
                r#"{{
                  "version": 1,
                  "hooks": {{
                    "PreToolUse": [{{"matcher": "Bash", "hooks": [{{"type": "command", "command": "{command}"}}]}}]
                  }}
                }}"#
            )
        };
        write(&global, &body("global"));
        write(&project, &body("project"));
        write(&local, &body("local"));

        let loaded = HooksLoader::load_from_paths([&global, &project, &local]).unwrap();

        let commands = loaded
            .groups
            .iter()
            .flat_map(|group| group.hooks.iter().map(|hook| hook.command.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(commands, ["global", "project", "local"]);
    }

    #[test]
    fn matcher_pipe_syntax_matches_exact_names() {
        let matcher = HookMatcher::parse(Some("Bash| Write "), HookEvent::PreToolUse);

        assert!(matcher.matches(&HookSource::Tool("Bash".to_owned())));
        assert!(matcher.matches(&HookSource::Tool("Write".to_owned())));
        assert!(!matcher.matches(&HookSource::Tool("Read".to_owned())));
    }

    #[test]
    fn rejects_matcher_on_non_source_events() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        write(
            &path,
            r#"{"version":1,"hooks":{"Stop":[{"matcher":"*","hooks":[{"type":"command","command":"true"}]}]}}"#,
        );

        let error = HooksLoader::load_from_paths([path.as_path()]).unwrap_err();
        assert!(matches!(error, HooksError::MatcherNotSupported { .. }));
    }

    #[test]
    fn rejects_unknown_event_names() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        write(
            &path,
            r#"{"version":1,"hooks":{"PreToolUSe":[{"hooks":[{"type":"command","command":"true"}]}]}}"#,
        );

        let error = HooksLoader::load_from_paths([path.as_path()]).unwrap_err();
        assert!(matches!(error, HooksError::UnknownEvent { .. }));
    }

    #[test]
    fn rejects_version_other_than_1() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("hooks.json");
        write(&path, r#"{"version":2,"hooks":{}}"#);

        let error = HooksLoader::load_from_paths([path.as_path()]).unwrap_err();
        assert!(matches!(error, HooksError::UnsupportedVersion { .. }));
    }

    #[test]
    fn matcher_all_with_empty_and_edge_cases() {
        // Empty value or "*" or whitespace-only all produce All.
        assert!(matches!(
            HookMatcher::parse(None, HookEvent::PreToolUse),
            HookMatcher::All
        ));
        assert!(matches!(
            HookMatcher::parse(Some("*"), HookEvent::SessionStart),
            HookMatcher::All
        ));
        assert!(matches!(
            HookMatcher::parse(Some("  |  "), HookEvent::PreToolUse),
            HookMatcher::All
        ));
        assert!(matches!(
            HookMatcher::parse(Some("||"), HookEvent::PreToolUse),
            HookMatcher::All
        ));
    }

    #[test]
    fn matcher_session_start_sources_match_exact() {
        let matcher = HookMatcher::parse(Some("fork|resume"), HookEvent::SessionStart);

        assert!(matcher.matches(&HookSource::SessionStart("fork".to_owned())));
        assert!(matcher.matches(&HookSource::SessionStart("resume".to_owned())));
        assert!(!matcher.matches(&HookSource::SessionStart("startup".to_owned())));
        // A Tool source should not match a SessionStart matcher.
        assert!(!matcher.matches(&HookSource::Tool("fork".to_owned())));
    }

    #[test]
    fn hook_source_serde_round_trips() {
        let cases = vec![
            HookSource::None,
            HookSource::Tool("Bash".to_owned()),
            HookSource::Tool("Read".to_owned()),
            HookSource::SessionStart("fork".to_owned()),
            HookSource::SessionStart("resume".to_owned()),
        ];
        for source in cases {
            let json = serde_json::to_string(&source).unwrap();
            let round_tripped: HookSource = serde_json::from_str(&json).unwrap();
            assert_eq!(source, round_tripped, "round-trip failed for {json}");
        }
    }

    #[test]
    fn hook_source_none_separates_correctly() {
        let source = HookSource::None;
        assert_eq!(source.as_display_str(), None);
        // None should not equal Tool("") or SessionStart("").
        assert_ne!(source, HookSource::Tool(String::new()));
        assert_ne!(source, HookSource::SessionStart(String::new()));
    }
}

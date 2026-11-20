//! Loading and compilation of `--output-schema` JSON Schema files.
//!
//! The schema constrains only the final response of a run (the `result` of
//! the `task_complete` record). Schemas are JSON Schema draft 2020-12 and
//! must be self-contained: the `jsonschema` crate is built without its
//! `resolve-http`/`resolve-file` features, so external `$ref` resolution
//! fails at compile time.

use std::path::{Path, PathBuf};

/// Maximum number of validation errors included in a failure detail string.
const MAX_REPORTED_ERRORS: usize = 8;

/// Maximum length of a single reported validation error message.
const MAX_ERROR_MESSAGE_LEN: usize = 500;

/// A compiled JSON Schema that constrains the final response of a run.
#[derive(Debug, Clone)]
pub struct OutputSchema {
    /// Name for the provider `json_schema` payload, derived from the schema
    /// file stem and sanitized to `[a-zA-Z0-9_-]`.
    pub name: String,
    /// The schema document as parsed from the file, passed through unmodified
    /// to providers that support native structured output.
    pub raw: serde_json::Value,
    /// The compiled draft 2020-12 validator; local validation is authoritative.
    pub validator: jsonschema::Validator,
}

/// Errors from loading, compiling, or satisfying an output schema.
#[derive(Debug, thiserror::Error)]
pub enum OutputSchemaError {
    #[error("Failed to read output schema file '{path}': {source}")]
    Unreadable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Output schema file '{path}' is not valid JSON: {source}")]
    InvalidJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("Output schema file '{path}' is not a valid JSON Schema (draft 2020-12): {detail}")]
    InvalidSchema { path: PathBuf, detail: String },
    #[error(
        "Final response did not conform to the output schema after {attempts} attempt(s): {detail}"
    )]
    Unsatisfied { attempts: u32, detail: String },
}

impl OutputSchema {
    /// Read, parse, and compile a JSON Schema file.
    ///
    /// # Errors
    ///
    /// Returns [`OutputSchemaError::Unreadable`] if the file cannot be read,
    /// [`OutputSchemaError::InvalidJson`] if it is not valid JSON, and
    /// [`OutputSchemaError::InvalidSchema`] if it does not compile as a
    /// draft 2020-12 schema (including schemas with external `$ref`s).
    pub fn load(path: &Path) -> Result<Self, OutputSchemaError> {
        let contents =
            std::fs::read_to_string(path).map_err(|source| OutputSchemaError::Unreadable {
                path: path.to_path_buf(),
                source,
            })?;
        let raw: serde_json::Value =
            serde_json::from_str(&contents).map_err(|source| OutputSchemaError::InvalidJson {
                path: path.to_path_buf(),
                source,
            })?;
        let validator = jsonschema::draft202012::new(&raw).map_err(|error| {
            OutputSchemaError::InvalidSchema {
                path: path.to_path_buf(),
                detail: error.to_string(),
            }
        })?;

        Ok(Self {
            name: schema_name_from_path(path),
            raw,
            validator,
        })
    }

    /// Collect validation errors for an instance into a bounded, human-readable
    /// detail string, or `None` when the instance validates.
    pub fn validation_detail(&self, instance: &serde_json::Value) -> Option<String> {
        let mut messages: Vec<String> = Vec::new();
        let mut total = 0usize;
        for error in self.validator.iter_errors(instance) {
            total += 1;
            if messages.len() < MAX_REPORTED_ERRORS {
                let location = error.instance_path();
                let mut message = if location.as_str().is_empty() {
                    error.to_string()
                } else {
                    format!("at {location}: {error}")
                };
                if message.len() > MAX_ERROR_MESSAGE_LEN {
                    let mut end = MAX_ERROR_MESSAGE_LEN;
                    while !message.is_char_boundary(end) {
                        end -= 1;
                    }
                    message.truncate(end);
                    message.push_str("...");
                }
                messages.push(message);
            }
        }
        if total == 0 {
            return None;
        }
        if total > messages.len() {
            messages.push(format!("... and {} more error(s)", total - messages.len()));
        }
        Some(messages.join("; "))
    }
}

/// Derive the provider-facing schema name from the schema file stem,
/// keeping only `[a-zA-Z0-9_-]` and falling back to `"final_output"`.
fn schema_name_from_path(path: &Path) -> String {
    let sanitized: String = path
        .file_stem()
        .map(|stem| stem.to_string_lossy())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if sanitized.is_empty() {
        "final_output".to_string()
    } else {
        sanitized
    }
}

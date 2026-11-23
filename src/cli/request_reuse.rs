//! Reusable provider-request captures.
//!
//! A capture is written after a successful main-agent task and lives beside
//! telemetry, not in the session transcript. It records the exact request body
//! for the next turn plus the identity needed to prove the capture still
//! belongs to this session and provider configuration.

use std::{
    fs::{self, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::{DataDir, owner_only};
use crate::types::ProviderIdentity;

const CAPTURE_VERSION: u32 = 1;
const MAX_CAPTURE_BYTES: u64 = 16 * 1024 * 1024;

/// The on-disk capture for one session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRequestCapture {
    pub version: u32,
    pub session_id: String,
    pub task_id: String,
    pub turn: u32,
    pub profile: Option<String>,
    pub provider_identity: ProviderIdentity,
    pub backend: String,
    pub body: String,
    pub body_sha256: String,
    pub body_len: u64,
    pub session_file_len: u64,
    pub created_at: DateTime<Utc>,
}

impl SessionRequestCapture {
    /// Build a capture and compute its body integrity fields.
    #[expect(
        clippy::too_many_arguments,
        reason = "a capture record carries session, task, turn, provider, body, and file identity"
    )]
    pub fn new(
        session_id: Uuid,
        task_id: Uuid,
        turn: u32,
        profile: Option<String>,
        provider_identity: ProviderIdentity,
        backend: String,
        body: String,
        session_file_len: u64,
    ) -> Self {
        let body_len = body.len() as u64;
        let body_sha256 = sha256_hex(body.as_bytes());
        Self {
            version: CAPTURE_VERSION,
            session_id: session_id.to_string(),
            task_id: task_id.to_string(),
            turn,
            profile,
            provider_identity,
            backend,
            body,
            body_sha256,
            body_len,
            session_file_len,
            created_at: Utc::now(),
        }
    }
}

/// A capture could not be used for this resume.
#[derive(Debug, thiserror::Error)]
pub enum RequestReuseError {
    /// No capture exists for the session.
    #[error("request capture unavailable for session {session_id}")]
    Unavailable {
        /// The requested session.
        session_id: Uuid,
    },
    /// The session changed after the capture was written.
    #[error("request capture stale for session {session_id}: {detail}")]
    Stale {
        /// The requested session.
        session_id: Uuid,
        /// Why the capture is stale.
        detail: String,
    },
    /// The capture belongs to a different provider or session shape.
    #[error("request capture incompatible for session {session_id}: {detail}")]
    Incompatible {
        /// The requested session.
        session_id: Uuid,
        /// Why the capture is incompatible.
        detail: String,
    },
}

/// The path of a session's capture.
pub fn capture_path(data_dir: &DataDir, session_id: Uuid) -> PathBuf {
    data_dir
        .get_cache_dir()
        .join("session-requests")
        .join(format!("{session_id}.json"))
}

/// Remove a session's capture. A missing file is not an error.
pub fn delete_capture(data_dir: &DataDir, session_id: Uuid) -> anyhow::Result<()> {
    let path = capture_path(data_dir, session_id);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Write a capture atomically with owner-only permissions.
pub fn write_capture(
    data_dir: &DataDir,
    session_id: Uuid,
    capture: &SessionRequestCapture,
) -> anyhow::Result<()> {
    let path = capture_path(data_dir, session_id);
    let directory = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("request capture path has no parent"))?;
    ensure_capture_directory(directory)?;
    let bytes = serde_json::to_vec(capture)?;
    if bytes.len() as u64 > MAX_CAPTURE_BYTES {
        anyhow::bail!(
            "request capture is {} bytes, above the {} byte limit",
            bytes.len(),
            MAX_CAPTURE_BYTES
        );
    }
    let temporary = directory.join(format!(".{session_id}-{}.tmp", Uuid::new_v4()));
    let result = write_temporary(&temporary, &path, &bytes);
    if result.is_err() {
        drop(fs::remove_file(&temporary));
    }
    result
}

/// Load and verify a capture's own integrity fields.
pub fn load_capture(
    data_dir: &DataDir,
    session_id: Uuid,
) -> Result<SessionRequestCapture, RequestReuseError> {
    let path = capture_path(data_dir, session_id);
    let bytes = read_capture_file(&path, session_id)?;
    let capture: SessionRequestCapture =
        serde_json::from_slice(&bytes).map_err(|error| RequestReuseError::Incompatible {
            session_id,
            detail: format!("capture is not valid JSON: {error}"),
        })?;
    validate_integrity(&capture, session_id)?;
    Ok(capture)
}

/// Verify the capture still describes the current session and provider.
pub fn validate_capture(
    capture: &SessionRequestCapture,
    session_id: Uuid,
    session_file_len: u64,
    current_identity: &ProviderIdentity,
    current_profile: Option<&str>,
) -> Result<(), RequestReuseError> {
    if capture.session_id != session_id.to_string() {
        return Err(RequestReuseError::Incompatible {
            session_id,
            detail: "capture belongs to a different session".to_string(),
        });
    }
    if capture.session_file_len != session_file_len {
        return Err(RequestReuseError::Stale {
            session_id,
            detail: format!(
                "session file is {session_file_len} bytes but the capture was written at {} bytes",
                capture.session_file_len
            ),
        });
    }
    if capture.provider_identity != *current_identity {
        return Err(RequestReuseError::Incompatible {
            session_id,
            detail: "provider identity changed since the capture was written".to_string(),
        });
    }
    if capture.profile.as_deref() != current_profile {
        return Err(RequestReuseError::Incompatible {
            session_id,
            detail: "profile changed since the capture was written".to_string(),
        });
    }
    Ok(())
}

/// Append the invocation prompt as the next user message and preserve the
/// captured tool choice.
pub fn append_user_message(
    capture: &SessionRequestCapture,
    session_id: Uuid,
    instruction: &str,
) -> Result<Vec<u8>, RequestReuseError> {
    let mut body: serde_json::Value =
        serde_json::from_str(&capture.body).map_err(|error| RequestReuseError::Incompatible {
            session_id,
            detail: format!("captured request body is not valid JSON: {error}"),
        })?;
    match capture.backend.as_str() {
        "chat_completions" => append_chat_message(&mut body, instruction, session_id)?,
        "responses" => append_responses_message(&mut body, instruction, session_id)?,
        other => {
            return Err(RequestReuseError::Incompatible {
                session_id,
                detail: format!("unsupported captured backend '{other}'"),
            });
        },
    }
    serde_json::to_vec(&body).map_err(|error| RequestReuseError::Incompatible {
        session_id,
        detail: format!("failed to serialize the reused request: {error}"),
    })
}

fn append_chat_message(
    body: &mut serde_json::Value,
    instruction: &str,
    session_id: Uuid,
) -> Result<(), RequestReuseError> {
    let messages = body
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| RequestReuseError::Incompatible {
            session_id,
            detail: "captured Chat Completions body has no messages array".to_string(),
        })?;
    messages.push(serde_json::json!({
        "role": "user",
        "content": instruction,
    }));
    Ok(())
}

fn append_responses_message(
    body: &mut serde_json::Value,
    instruction: &str,
    session_id: Uuid,
) -> Result<(), RequestReuseError> {
    let input = body
        .get_mut("input")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| RequestReuseError::Incompatible {
            session_id,
            detail: "captured Responses body has no input array".to_string(),
        })?;
    input.push(serde_json::json!({
        "type": "message",
        "role": "user",
        "content": [{
            "type": "input_text",
            "text": instruction,
        }],
    }));
    Ok(())
}

fn read_capture_file(path: &Path, session_id: Uuid) -> Result<Vec<u8>, RequestReuseError> {
    let mut options = OpenOptions::new();
    options.read(true);
    owner_only::apply_session_options(&mut options);
    let mut file = options.open(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => RequestReuseError::Unavailable { session_id },
        _ => RequestReuseError::Incompatible {
            session_id,
            detail: format!("cannot open capture '{}': {error}", path.display()),
        },
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| RequestReuseError::Incompatible {
            session_id,
            detail: format!("cannot stat capture '{}': {error}", path.display()),
        })?;
    if !metadata.is_file() {
        return Err(RequestReuseError::Incompatible {
            session_id,
            detail: format!("capture '{}' is not a regular file", path.display()),
        });
    }
    if metadata.len() > MAX_CAPTURE_BYTES {
        return Err(RequestReuseError::Incompatible {
            session_id,
            detail: format!(
                "capture '{}' is {} bytes, above the {} byte limit",
                path.display(),
                metadata.len(),
                MAX_CAPTURE_BYTES
            ),
        });
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)
        .map_err(|error| RequestReuseError::Incompatible {
            session_id,
            detail: format!("cannot read capture '{}': {error}", path.display()),
        })?;
    Ok(bytes)
}

fn validate_integrity(
    capture: &SessionRequestCapture,
    session_id: Uuid,
) -> Result<(), RequestReuseError> {
    if capture.version != CAPTURE_VERSION {
        return Err(RequestReuseError::Incompatible {
            session_id,
            detail: format!(
                "capture version {} is not supported (expected {CAPTURE_VERSION})",
                capture.version
            ),
        });
    }
    let actual_len = capture.body.len() as u64;
    if actual_len != capture.body_len {
        return Err(RequestReuseError::Incompatible {
            session_id,
            detail: format!(
                "capture body length is {actual_len} bytes but the record says {}",
                capture.body_len
            ),
        });
    }
    let actual_hash = sha256_hex(capture.body.as_bytes());
    if actual_hash != capture.body_sha256 {
        return Err(RequestReuseError::Incompatible {
            session_id,
            detail: "capture body digest does not match the record".to_string(),
        });
    }
    Ok(())
}

fn ensure_capture_directory(directory: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                anyhow::bail!(
                    "request capture directory '{}' must be a real directory",
                    directory.display()
                );
            }
            owner_only::tighten_dir_owner_only(directory)?;
            Ok(())
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            owner_only::create_dir_all_owner_only(directory)?;
            Ok(())
        },
        Err(error) => Err(error.into()),
    }
}

fn write_temporary(temporary: &Path, path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    owner_only::apply_session_options(&mut options);
    let mut file = options.open(temporary)?;
    owner_only::set_file_owner_only(&file)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

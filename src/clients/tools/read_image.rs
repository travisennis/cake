use std::io::Read as _;
use std::path::Path;

use base64::Engine as _;
use serde::Deserialize;

use crate::clients::image_budget::{
    MAX_SOURCE_PIXELS, is_supported_media_type, prepare_image, source_dimensions,
};
use crate::clients::tools::{
    ToolContext, file_access::validate_regular_file, resolve_path_for_read_scheduling,
    validate_path_in_cwd,
};
use crate::types::ImagePart;

// =============================================================================
// ReadImage Tool Definition
// =============================================================================

/// Returns the `ReadImage` tool definition.
pub(super) fn read_image_tool() -> super::Tool {
    super::Tool {
        type_: "function".to_string(),
        name: "ReadImage".to_string(),
        description: include_str!("read-image-description.txt").to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute path to the image file to read (PNG, JPEG, GIF, or WebP)"
                }
            },
            "required": ["path"]
        }),
    }
}

// =============================================================================
// ReadImage Execution
// =============================================================================

/// Arguments for the `ReadImage` tool.
#[derive(Deserialize)]
struct ReadImageArgs {
    path: String,
}

/// Return the validated canonical path this `ReadImage` call would observe.
///
/// Scheduling uses this to serialize the read with same-path mutations in the
/// same turn. Unresolvable arguments or paths return `Err`, leaving the call
/// unscheduled so it can surface its own error.
pub(super) fn path_target(
    context: &ToolContext,
    arguments: &str,
) -> Result<std::path::PathBuf, String> {
    let args: ReadImageArgs = serde_json::from_str(arguments)
        .map_err(|e| format!("Invalid read_image arguments: {e}"))?;
    resolve_path_for_read_scheduling(context, &args.path)
}

/// Execute a `ReadImage` call: validate the path, read the file under the
/// configured size cap, and return its bytes as one inline image part.
///
/// An image wider or taller than the 2048-pixel edge cap is downscaled first,
/// so a large screenshot reaches the provider as a usable picture instead of a
/// request the provider rejects outright.
pub(super) fn execute_read_image(
    context: &ToolContext,
    arguments: &str,
) -> Result<super::ToolResult, String> {
    let args: ReadImageArgs = serde_json::from_str(arguments)
        .map_err(|e| format!("Invalid read_image arguments: {e}"))?;

    let path = validate_path_in_cwd(context, &args.path)?;

    // Reject directories, pipes, sockets, and other objects before the
    // blocking open; a named pipe with no writer never returns from it.
    validate_regular_file(&path)?;

    let bytes = read_bounded(&path, context.limits.read_image_max_bytes)?;

    let Some(media_type) = detect_image_media_type(&bytes) else {
        return Err(unsupported_image_error(
            &path,
            detect_mime_type(&bytes).unwrap_or("an unknown format"),
        ));
    };

    let source_size = checked_source_dimensions(&path, &bytes, media_type)?;

    let prepared = prepare_image(&bytes, media_type, source_size);
    let size_bytes = prepared.bytes.len();
    let resize_note = if prepared.changed {
        match (prepared.source_size, prepared.size) {
            (Some(source), Some(sent)) => format!(
                ", downscaled from {}x{} to {}x{}",
                source.0, source.1, sent.0, sent.1
            ),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let output = format!(
        "ReadImage {} ({size_bytes} bytes, {}{resize_note})",
        path.display(),
        prepared.media_type
    );

    Ok(super::ToolResult {
        output,
        images: vec![ImagePart {
            media_type: prepared.media_type.to_string(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(&prepared.bytes),
            content_id: None,
        }],
        compensation_events: Vec::new(),
        permission_denials: Vec::new(),
    })
}

/// Read a file's bytes, enforcing the configured image size cap.
///
/// The read stops at `cap + 1` bytes, so memory stays bounded no matter how
/// large the file is; a file longer than the cap is rejected with a
/// model-visible error instead of being returned. `None` disables the cap.
fn read_bounded(path: &Path, cap: Option<usize>) -> Result<Vec<u8>, String> {
    let Some(cap) = cap else {
        return std::fs::read(path)
            .map_err(|e| format!("Failed to read file '{}': {e}", path.display()));
    };

    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("Failed to read file '{}': {e}", path.display()))?;

    if bytes.len() > cap {
        return Err(format!(
            "Image file is too large: {} exceeds the read_image_max_bytes limit of {cap} bytes",
            path.display()
        ));
    }

    Ok(bytes)
}

/// Probe the source header and reject a decompression-bomb-sized claim.
fn checked_source_dimensions(
    path: &Path,
    bytes: &[u8],
    media_type: &str,
) -> Result<Option<(u32, u32)>, String> {
    let source_size = source_dimensions(bytes, media_type).map_err(|error| {
        format!(
            "Failed to read image dimensions for {}: {error}",
            path.display()
        )
    })?;
    let Some((width, height)) = source_size else {
        return Ok(None);
    };
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_SOURCE_PIXELS {
        return Err(oversized_source_error(path, width, height, pixels));
    }
    Ok(Some((width, height)))
}

/// Model-visible error for a source image whose header claims too many pixels.
fn oversized_source_error(path: &Path, width: u32, height: u32, pixels: u64) -> String {
    format!(
        "Image dimensions are too large: {} is {width}x{height} ({pixels} pixels), above the {MAX_SOURCE_PIXELS}-pixel source limit; crop or resize the image before using ReadImage.",
        path.display()
    )
}

/// Detect the MIME type of raw file bytes using content-based detection.
fn detect_mime_type(data: &[u8]) -> Option<&'static str> {
    infer::get(data).map(|kind| kind.mime_type())
}

/// Return the MIME type only when the bytes are in a supported image format.
fn detect_image_media_type(data: &[u8]) -> Option<&'static str> {
    detect_mime_type(data).filter(|media_type| is_supported_media_type(media_type))
}

/// Model-visible error for a file `ReadImage` will not send.
///
/// A recognized-but-unsupported image format is distinguished from a non-image
/// so the model learns the file is an image of an unsupported type rather than
/// text it should read with `Read`.
fn unsupported_image_error(path: &Path, detected: &str) -> String {
    if detected.starts_with("image/") {
        format!(
            "Unsupported image format: {} (detected {detected}); ReadImage supports PNG, JPEG, GIF, and WebP.",
            path.display()
        )
    } else {
        format!(
            "Not an image file: {} (detected {detected}); use Read to read text files.",
            path.display()
        )
    }
}

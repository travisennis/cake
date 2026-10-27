use std::io::Read as _;
use std::path::Path;

use base64::Engine as _;
use serde::Deserialize;

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

    let size_bytes = bytes.len();
    Ok(super::ToolResult {
        output: format!(
            "ReadImage {} ({size_bytes} bytes, {media_type})",
            path.display()
        ),
        images: vec![ImagePart {
            media_type: media_type.to_string(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
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

/// Image formats `ReadImage` sends to the provider.
///
/// `infer` recognizes many more `image/*` types (TIFF, BMP, ICO, AVIF, and
/// others) than the vision-capable backends accept, so the accepted set is an
/// explicit allowlist that matches the tool description rather than an
/// `image/` prefix test.
const SUPPORTED_IMAGE_MEDIA_TYPES: &[&str] =
    &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Detect the MIME type of raw file bytes using content-based detection.
fn detect_mime_type(data: &[u8]) -> Option<&'static str> {
    infer::get(data).map(|kind| kind.mime_type())
}

/// Return the MIME type only when the bytes are in a supported image format.
fn detect_image_media_type(data: &[u8]) -> Option<&'static str> {
    detect_mime_type(data).filter(|media_type| SUPPORTED_IMAGE_MEDIA_TYPES.contains(media_type))
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

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::clients::tools::file_access::process_isolation;
    use crate::config::settings::DEFAULT_READ_IMAGE_MAX_BYTES;
    use tempfile::TempDir;

    /// A valid 1x1 red PNG, generated with Python `zlib`.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xf8,
        0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0xf7, 0x03, 0x41, 0x43, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn write_fixture(name: &str, bytes: &[u8]) -> (TempDir, String) {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join(name);
        std::fs::write(&file_path, bytes).unwrap();
        let path = file_path.to_str().unwrap().to_string();
        (temp_dir, path)
    }

    #[test]
    fn read_image_returns_one_image_part() {
        let (_temp_dir, path) = write_fixture("red.png", TINY_PNG);
        let args = serde_json::json!({ "path": path }).to_string();

        let result = execute_read_image(&ToolContext::from_current_process(), &args).unwrap();

        assert_eq!(result.images.len(), 1);
        let image = &result.images[0];
        assert_eq!(image.media_type, "image/png");
        assert_eq!(
            image.data_base64,
            base64::engine::general_purpose::STANDARD.encode(TINY_PNG)
        );
        assert!(result.output.contains("image/png"), "{}", result.output);
        assert!(
            result.output.contains(&format!("{} bytes", TINY_PNG.len())),
            "{}",
            result.output
        );
    }

    #[test]
    fn read_image_rejects_text_file() {
        let (_temp_dir, path) = write_fixture("notes.txt", b"plain text, not an image\n");
        let args = serde_json::json!({ "path": path }).to_string();

        let err = execute_read_image(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("Not an image file"), "{err}");
        assert!(err.contains("use Read"), "{err}");
    }

    #[test]
    fn read_image_rejects_undetected_format() {
        let (_temp_dir, path) = write_fixture("blob.bin", &[0x01, 0x02, 0x03, 0x04]);
        let args = serde_json::json!({ "path": path }).to_string();

        let err = execute_read_image(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("an unknown format"), "{err}");
        assert!(err.contains("use Read"), "{err}");
    }

    #[test]
    fn read_image_rejects_unsupported_image_format() {
        // BMP is a real image format that `infer` detects but that the vision
        // backends, and therefore this tool, do not support.
        let (_temp_dir, path) = write_fixture("scan.bmp", b"BM\x00\x00\x00\x00");
        let args = serde_json::json!({ "path": path }).to_string();

        let err = execute_read_image(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("Unsupported image format"), "{err}");
        assert!(err.contains("detected image/bmp"), "{err}");
        assert!(err.contains("PNG, JPEG, GIF, and WebP"), "{err}");
    }

    #[test]
    fn read_image_rejects_file_over_limit() {
        let (_temp_dir, path) = write_fixture("big.png", TINY_PNG);
        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_image_max_bytes = Some(TINY_PNG.len() - 1);
        context.limits = limits;
        let args = serde_json::json!({ "path": path }).to_string();

        let err = execute_read_image(&context, &args).unwrap_err();

        assert!(err.contains("too large"), "{err}");
        assert!(err.contains("read_image_max_bytes"), "{err}");
    }

    #[test]
    fn read_image_accepts_file_at_limit() {
        let (_temp_dir, path) = write_fixture("exact.png", TINY_PNG);
        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_image_max_bytes = Some(TINY_PNG.len());
        context.limits = limits;
        let args = serde_json::json!({ "path": path }).to_string();

        let result = execute_read_image(&context, &args).unwrap();

        assert_eq!(result.images.len(), 1);
    }

    #[test]
    fn read_image_rejects_directory() {
        let temp_dir = TempDir::new().unwrap();
        let args = serde_json::json!({ "path": temp_dir.path().to_str().unwrap() }).to_string();

        let err = execute_read_image(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("is a directory"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn read_image_rejects_fifo_without_a_writer() {
        // A regression in the object-type precheck would make the blocking
        // open wait forever, so the body runs in a child process with an
        // external deadline.
        if !process_isolation::in_child() {
            process_isolation::run_isolated(
                "read_image_rejects_fifo_without_a_writer",
                std::time::Duration::from_secs(10),
            );
            return;
        }

        let temp_dir = TempDir::new().unwrap();
        let fifo = temp_dir.path().join("pipe.png");
        create_fifo(&fifo);

        let args = serde_json::json!({ "path": fifo.to_str().unwrap() }).to_string();
        let err = execute_read_image(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("is not a regular file"), "{err}");
        assert!(err.contains("pipe.png"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn read_image_rejects_unix_socket() {
        let temp_dir = TempDir::new().unwrap();
        let socket = temp_dir.path().join("agent.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();

        let args = serde_json::json!({ "path": socket.to_str().unwrap() }).to_string();
        let err = execute_read_image(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("is not a regular file"), "{err}");
        assert!(err.contains("agent.sock"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn read_image_follows_symlink_to_regular_file() {
        let temp_dir = TempDir::new().unwrap();
        let target = temp_dir.path().join("red.png");
        std::fs::write(&target, TINY_PNG).unwrap();
        let link = temp_dir.path().join("link.png");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let args = serde_json::json!({ "path": link.to_str().unwrap() }).to_string();
        let result = execute_read_image(&ToolContext::from_current_process(), &args).unwrap();

        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].media_type, "image/png");
    }

    /// Create a named pipe with no writer, which blocks `File::open` until the
    /// object-type precheck rejects it.
    #[cfg(unix)]
    fn create_fifo(path: &std::path::Path) {
        let status = std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success(), "mkfifo failed for {}", path.display());
    }

    #[test]
    fn read_image_arguments_require_path() {
        let err = execute_read_image(&ToolContext::from_current_process(), "{}").unwrap_err();
        assert!(err.contains("Invalid read_image arguments"), "{err}");
    }

    #[test]
    fn default_limit_matches_compiled_constant() {
        assert_eq!(
            crate::config::settings::ToolLimits::defaults().read_image_max_bytes,
            Some(DEFAULT_READ_IMAGE_MAX_BYTES as usize)
        );
    }
}

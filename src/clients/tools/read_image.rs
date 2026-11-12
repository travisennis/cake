use std::io::Cursor;
use std::io::Read as _;
use std::path::Path;

use base64::Engine as _;
use image::imageops::FilterType;
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
///
/// An image wider or taller than [`MAX_IMAGE_DIMENSION`] is downscaled first,
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

    let prepared = prepare_image(&bytes, media_type);
    let size_bytes = prepared.bytes.len();
    let resize_note = match (prepared.source_size, prepared.size) {
        (Some(source), Some(sent)) if source != sent => format!(
            ", downscaled from {}x{} to {}x{}",
            source.0, source.1, sent.0, sent.1
        ),
        _ => String::new(),
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

// =============================================================================
// Downscaling
// =============================================================================

/// Longest edge `ReadImage` forwards to a provider, in pixels.
///
/// Vision models resample anything larger anyway, but a provider that cannot
/// read an image rejects the entire request, stranding a task over a file that
/// is perfectly valid. Shrinking here keeps a large screenshot or photo usable
/// instead of poisoning the conversation that carries it.
const MAX_IMAGE_DIMENSION: u32 = 2048;

/// Ceiling on decoded image memory, so a small compressed file that expands to
/// gigabytes is refused instead of exhausting the process.
const MAX_DECODED_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

/// JPEG quality for a downscaled re-encode: high enough that small text stays
/// legible after resizing.
const DOWNSCALE_JPEG_QUALITY: u8 = 85;

/// The bytes `ReadImage` sends, plus the dimensions the model-visible output
/// reports.
struct PreparedImage {
    bytes: Vec<u8>,
    media_type: &'static str,
    /// Dimensions of the bytes actually sent, when known.
    size: Option<(u32, u32)>,
    /// Dimensions of the file on disk, when known.
    source_size: Option<(u32, u32)>,
}

/// A re-encoded image: the bytes to send and what they are.
struct Downscaled {
    bytes: Vec<u8>,
    media_type: &'static str,
    size: (u32, u32),
}

/// Shrink `bytes` when either edge exceeds [`MAX_IMAGE_DIMENSION`].
///
/// An image inside the cap is forwarded byte for byte, which preserves
/// animation, the source encoder's settings, and every pixel a vision model
/// would consume. Downscaling is best effort: a decode or encode failure
/// returns the original bytes, because a file `infer` recognizes but this
/// decoder rejects may still be readable upstream, and the agent loop's
/// image-rejection recovery is the backstop for anything that slips through.
fn prepare_image(bytes: &[u8], media_type: &'static str) -> PreparedImage {
    let unchanged = |size| PreparedImage {
        bytes: bytes.to_vec(),
        media_type,
        size,
        source_size: size,
    };

    let Some(format) = image_format(media_type) else {
        return unchanged(None);
    };
    let Ok((width, height)) = image_dimensions(bytes, format) else {
        return unchanged(None);
    };
    if width <= MAX_IMAGE_DIMENSION && height <= MAX_IMAGE_DIMENSION {
        return unchanged(Some((width, height)));
    }

    match downscale(bytes, format) {
        Ok(downscaled) => PreparedImage {
            bytes: downscaled.bytes,
            media_type: downscaled.media_type,
            size: Some(downscaled.size),
            source_size: Some((width, height)),
        },
        Err(error) => {
            tracing::warn!(
                target: "cake",
                %error,
                "Could not downscale an oversized image; sending it unchanged"
            );
            unchanged(Some((width, height)))
        },
    }
}

/// Map a supported media type to the codec that decodes and re-encodes it.
fn image_format(media_type: &str) -> Option<image::ImageFormat> {
    Some(match media_type {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/gif" => image::ImageFormat::Gif,
        "image/webp" => image::ImageFormat::WebP,
        _ => return None,
    })
}

/// Read the dimensions from the image header without decoding the pixels.
fn image_dimensions(bytes: &[u8], format: image::ImageFormat) -> Result<(u32, u32), String> {
    image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|e| format!("Failed to read the image dimensions: {e}"))
}

/// Decode, shrink to the cap, and re-encode `bytes`.
fn downscale(bytes: &[u8], format: image::ImageFormat) -> Result<Downscaled, String> {
    let image = decode_image(bytes, format)?;
    let resized = image.resize(
        MAX_IMAGE_DIMENSION,
        MAX_IMAGE_DIMENSION,
        FilterType::Lanczos3,
    );
    encode_downscaled(&resized, format)
}

/// Decode the whole image, refusing a file whose pixels would not fit in memory.
fn decode_image(bytes: &[u8], format: image::ImageFormat) -> Result<image::DynamicImage, String> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(decode_limits());
    reader
        .decode()
        .map_err(|e| format!("Failed to decode the image: {e}"))
}

/// Allocation ceiling for one decode.
///
/// `image::Limits` is non-exhaustive outside its crate, so struct-update syntax
/// is unavailable and the one field is assigned directly.
fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODED_IMAGE_BYTES);
    limits
}

/// Encode a downscaled image, keeping the source format when it can represent
/// the pixels.
///
/// PNG sources stay PNG so screenshots and fine text survive the resize
/// losslessly. Everything else becomes JPEG, which is compact for the
/// photographic images that reach this size. Transparency forces PNG because
/// JPEG cannot carry it, and the WebP and GIF encoders are either absent or
/// lossy-only here, so those sources take the same rule.
fn encode_downscaled(
    image: &image::DynamicImage,
    source: image::ImageFormat,
) -> Result<Downscaled, String> {
    let size = (image.width(), image.height());
    if source == image::ImageFormat::Png || has_transparency(image) {
        let mut bytes = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
            .map_err(|e| format!("Failed to encode the downscaled PNG: {e}"))?;
        return Ok(Downscaled {
            bytes,
            media_type: "image/png",
            size,
        });
    }

    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, DOWNSCALE_JPEG_QUALITY)
        .encode_image(&image.to_rgb8())
        .map_err(|e| format!("Failed to encode the downscaled JPEG: {e}"))?;
    Ok(Downscaled {
        bytes,
        media_type: "image/jpeg",
        size,
    })
}

/// Whether the image has transparency that a JPEG encode would discard.
fn has_transparency(image: &image::DynamicImage) -> bool {
    if !image.color().has_alpha() {
        return false;
    }
    image.to_rgba8().pixels().any(|pixel| pixel.0[3] < u8::MAX)
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

    /// The bytes `ReadImage` actually hands to the provider.
    fn sent_bytes(image: &ImagePart) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(&image.data_base64)
            .unwrap()
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
    fn read_image_downscales_an_image_over_the_cap() {
        // 3000x40 is 75:1 and only a few kilobytes, but its width is well past
        // the 2048px cap, which is the shape that costs a provider request.
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("wide.png");
        image::RgbImage::from_fn(3000, 40, |x, y| {
            image::Rgb([
                u8::try_from(x % 256).unwrap(),
                u8::try_from(y % 256).unwrap(),
                0,
            ])
        })
        .save(&source)
        .unwrap();

        let args = serde_json::json!({ "path": source.to_str().unwrap() }).to_string();
        let result = execute_read_image(&ToolContext::from_current_process(), &args).unwrap();

        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].media_type, "image/png");
        assert!(
            result.output.contains("downscaled from 3000x40 to 2048x"),
            "{}",
            result.output
        );

        let sent = image::load_from_memory(&sent_bytes(&result.images[0])).unwrap();
        assert_eq!(sent.width(), MAX_IMAGE_DIMENSION);
        assert!(sent.height() <= 40, "{}", sent.height());
    }

    #[test]
    fn read_image_downscales_a_jpeg_and_keeps_it_a_jpeg() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("tall.jpg");
        image::RgbImage::from_fn(200, 2600, |x, y| {
            image::Rgb([
                u8::try_from(x % 256).unwrap(),
                u8::try_from(y % 256).unwrap(),
                u8::try_from((x + y) % 256).unwrap(),
            ])
        })
        .save(&source)
        .unwrap();

        let args = serde_json::json!({ "path": source.to_str().unwrap() }).to_string();
        let result = execute_read_image(&ToolContext::from_current_process(), &args).unwrap();

        assert_eq!(result.images[0].media_type, "image/jpeg");
        assert!(
            result.output.contains("downscaled from 200x2600 to "),
            "{}",
            result.output
        );

        let bytes = sent_bytes(&result.images[0]);
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Jpeg
        );
        let sent = image::load_from_memory(&bytes).unwrap();
        assert_eq!(sent.height(), MAX_IMAGE_DIMENSION);
        assert!(sent.width() <= 200, "{}", sent.width());
    }

    #[test]
    fn transparency_forces_png_for_a_photographic_source() {
        let opaque = image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 4));
        assert!(!has_transparency(&opaque));

        let mut translucent = image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 255]));
        assert!(!has_transparency(&image::DynamicImage::ImageRgba8(
            translucent.clone()
        )));

        translucent.put_pixel(2, 2, image::Rgba([0, 0, 0, 0]));
        assert!(has_transparency(&image::DynamicImage::ImageRgba8(
            translucent
        )));
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

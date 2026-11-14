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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::image_budget::{MAX_IMAGE_DIMENSION, has_transparency};
    #[cfg(unix)]
    use crate::clients::tools::file_access::process_isolation;
    use crate::config::settings::DEFAULT_READ_IMAGE_MAX_BYTES;
    use sha2::{Digest as _, Sha256};
    use std::io::Write as _;
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

    /// Write a gradient image whose width is past the 2048-pixel edge cap.
    fn write_gradient_image(
        path: &std::path::Path,
        format: image::ImageFormat,
        width: u32,
        height: u32,
    ) {
        image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([
                u8::try_from(x % 256).unwrap(),
                u8::try_from(y % 256).unwrap(),
                u8::try_from((x + y) % 256).unwrap(),
            ])
        })
        .save_with_format(path, format)
        .unwrap();
    }

    fn file_digest(path: &std::path::Path) -> Vec<u8> {
        Sha256::digest(std::fs::read(path).unwrap()).to_vec()
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
    fn read_image_downscales_every_supported_codec_and_leaves_the_source_untouched() {
        struct Case {
            name: &'static str,
            source: image::ImageFormat,
            emitted_media_type: &'static str,
            emitted: image::ImageFormat,
        }

        let cases = [
            Case {
                name: "wide.png",
                source: image::ImageFormat::Png,
                emitted_media_type: "image/png",
                emitted: image::ImageFormat::Png,
            },
            Case {
                name: "wide.jpg",
                source: image::ImageFormat::Jpeg,
                emitted_media_type: "image/jpeg",
                emitted: image::ImageFormat::Jpeg,
            },
            Case {
                name: "wide.gif",
                source: image::ImageFormat::Gif,
                emitted_media_type: "image/jpeg",
                emitted: image::ImageFormat::Jpeg,
            },
            Case {
                name: "wide.webp",
                source: image::ImageFormat::WebP,
                emitted_media_type: "image/jpeg",
                emitted: image::ImageFormat::Jpeg,
            },
        ];

        for case in cases {
            let temp_dir = TempDir::new().unwrap();
            let source = temp_dir.path().join(case.name);
            write_gradient_image(&source, case.source, 2400, 600);
            let before = file_digest(&source);

            let args = serde_json::json!({ "path": source.to_str().unwrap() }).to_string();
            let result = execute_read_image(&ToolContext::from_current_process(), &args)
                .unwrap_or_else(|error| panic!("{}: {error}", case.name));

            assert_eq!(result.images.len(), 1, "{}", case.name);
            assert_eq!(
                result.images[0].media_type, case.emitted_media_type,
                "{}",
                case.name
            );
            assert!(
                result
                    .output
                    .contains("downscaled from 2400x600 to 2048x512"),
                "{}: {}",
                case.name,
                result.output
            );

            let bytes = sent_bytes(&result.images[0]);
            assert_eq!(
                image::guess_format(&bytes).unwrap(),
                case.emitted,
                "{}",
                case.name
            );
            let sent = image::load_from_memory(&bytes).unwrap();
            assert_eq!((sent.width(), sent.height()), (2048, 512), "{}", case.name);
            let aspect = f64::from(sent.width()) / f64::from(sent.height());
            assert!(
                (aspect - 4.0).abs() < 0.01,
                "{}: aspect ratio {aspect}",
                case.name
            );

            assert_eq!(
                before,
                file_digest(&source),
                "{}: the source file must stay byte-identical",
                case.name
            );
        }
    }

    #[test]
    fn read_image_keeps_a_small_image_byte_identical() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("small.png");
        write_gradient_image(&source, image::ImageFormat::Png, 10, 10);
        let original = std::fs::read(&source).unwrap();

        let args = serde_json::json!({ "path": source.to_str().unwrap() }).to_string();
        let result = execute_read_image(&ToolContext::from_current_process(), &args).unwrap();

        assert_eq!(sent_bytes(&result.images[0]), original);
        assert!(!result.output.contains("downscaled"), "{}", result.output);
        let sent = image::load_from_memory(&sent_bytes(&result.images[0])).unwrap();
        assert_eq!((sent.width(), sent.height()), (10, 10));
    }

    #[test]
    fn read_image_rejects_a_decompression_bomb_before_decoding() {
        // A GIF logical screen of 65535x65535 claims 4,294,836,225 pixels.
        // The 1x1 frame keeps the container valid for the header probe, while
        // a full decode of the logical screen would need more than 16 GiB.
        let header = [
            b'G', b'I', b'F', b'8', b'9', b'a', // signature
            0xff, 0xff, // logical screen width 65535
            0xff, 0xff, // logical screen height 65535
            0x00, 0x00, 0x00, // no global palette, background, aspect
            0x2c, // image separator
            0x00, 0x00, 0x00, 0x00, // frame origin
            0x01, 0x00, 0x01, 0x00, // frame size 1x1
            0x00, // no local palette
            0x02, // LZW minimum code size
            0x02, 0x44, 0x01, 0x00, // one-pixel LZW data and terminator
            0x3b, // trailer
        ];
        let (_temp_dir, path) = write_fixture("bomb.gif", &header);
        let args = serde_json::json!({ "path": path }).to_string();

        let err = execute_read_image(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("Image dimensions are too large"), "{err}");
        assert!(err.contains("65535x65535"), "{err}");
        assert!(err.contains("4294836225"), "{err}");
        assert!(err.contains("100000000-pixel source limit"), "{err}");
        assert!(err.contains("crop or resize"), "{err}");
    }

    #[test]
    fn read_image_reports_a_header_probe_failure_without_panicking() {
        let (_temp_dir, path) = write_fixture("truncated.gif", b"GIF89a");
        let args = serde_json::json!({ "path": path }).to_string();

        let err = execute_read_image(&ToolContext::from_current_process(), &args).unwrap_err();

        assert!(err.contains("Failed to read image dimensions"), "{err}");
    }

    #[test]
    fn read_image_rejects_a_large_source_with_a_bounded_read() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("huge.png");
        let mut file = std::fs::File::create(&source).unwrap();
        file.write_all(TINY_PNG).unwrap();
        file.set_len(64 * 1024 * 1024).unwrap();
        drop(file);

        let mut context = ToolContext::from_current_process();
        let mut limits = crate::config::settings::ToolLimits::defaults();
        limits.read_image_max_bytes = Some(4096);
        context.limits = limits;
        let args = serde_json::json!({ "path": source.to_str().unwrap() }).to_string();

        let err = execute_read_image(&context, &args).unwrap_err();

        assert!(err.contains("too large"), "{err}");
        assert!(err.contains("4096"), "{err}");
    }

    #[test]
    fn read_image_keeps_oversized_bytes_when_the_downscale_decode_fails() {
        let temp_dir = TempDir::new().unwrap();
        let source = temp_dir.path().join("broken.png");
        write_gradient_image(&source, image::ImageFormat::Png, 3000, 40);
        let mut bytes = std::fs::read(&source).unwrap();
        let idat = bytes
            .windows(4)
            .position(|window| window == b"IDAT")
            .unwrap();
        bytes[idat + 8] ^= 0xff;
        std::fs::write(&source, &bytes).unwrap();

        let args = serde_json::json!({ "path": source.to_str().unwrap() }).to_string();
        let result = execute_read_image(&ToolContext::from_current_process(), &args).unwrap();

        assert_eq!(sent_bytes(&result.images[0]), bytes);
        assert!(!result.output.contains("downscaled"), "{}", result.output);
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

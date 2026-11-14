//! Shared image codec helpers for `ReadImage` and request image budgets.
//!
//! Source acceptance and request sizing are separate concerns. `ReadImage`
//! uses the attachment helpers here to probe a source from its header, reject a
//! decompression bomb, and downscale an oversized image in memory. The request
//! planner will reuse the same codecs so there is one encoding path.

use std::io::Cursor;

use image::imageops::FilterType;

/// Longest edge `ReadImage` forwards to a provider, in pixels.
///
/// Vision models resample anything larger anyway, but a provider that cannot
/// read an image rejects the entire request, stranding a task over a file that
/// is perfectly valid. Shrinking here keeps a large screenshot or photo usable
/// instead of poisoning the conversation that carries it.
pub const MAX_IMAGE_DIMENSION: u32 = crate::config::settings::MAX_IMAGE_EDGE;

/// Ceiling on decoded image memory, so a small compressed file that expands to
/// gigabytes is refused instead of exhausting the process.
pub const MAX_DECODED_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

/// Ceiling on decoded pixels for one source image.
///
/// A header can claim billions of pixels in a few bytes. The product is
/// checked in `u64` before a full decode, so a hostile width and height cannot
/// overflow or force the decoder to allocate the claimed buffer.
pub const MAX_SOURCE_PIXELS: u64 = 100_000_000;

/// JPEG quality for a downscaled re-encode: high enough that small text stays
/// legible after resizing.
const DOWNSCALE_JPEG_QUALITY: u8 = 85;

/// Image formats `ReadImage` sends to the provider.
///
/// `infer` recognizes many more `image/*` types (TIFF, BMP, ICO, AVIF, and
/// others) than the vision-capable backends accept, so the accepted set is an
/// explicit allowlist that matches the tool description rather than an
/// `image/` prefix test.
const SUPPORTED_IMAGE_MEDIA_TYPES: &[&str] =
    &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// The bytes `ReadImage` sends, plus the dimensions the model-visible output
/// reports.
pub struct PreparedImage {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
    /// Dimensions of the bytes actually sent, when known.
    pub size: Option<(u32, u32)>,
    /// Dimensions of the file on disk, when known.
    pub source_size: Option<(u32, u32)>,
    /// Whether the emitted bytes were decoded and re-encoded.
    pub changed: bool,
}

/// A re-encoded image: the bytes to send and what they are.
pub struct Downscaled {
    pub bytes: Vec<u8>,
    pub media_type: &'static str,
    pub size: (u32, u32),
}

/// Whether a media type is one of the four supported codecs.
pub fn is_supported_media_type(media_type: &str) -> bool {
    SUPPORTED_IMAGE_MEDIA_TYPES.contains(&media_type)
}

/// Map a supported media type to the codec that decodes and re-encodes it.
pub fn image_format(media_type: &str) -> Option<image::ImageFormat> {
    Some(match media_type {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/gif" => image::ImageFormat::Gif,
        "image/webp" => image::ImageFormat::WebP,
        _ => return None,
    })
}

/// Read the dimensions from the image header without decoding the pixels.
pub fn image_dimensions(bytes: &[u8], format: image::ImageFormat) -> Result<(u32, u32), String> {
    image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|e| format!("Failed to read the image dimensions: {e}"))
}

/// Probe the source dimensions for a supported media type.
///
/// An opaque media type has no codec here, so its dimensions stay unknown.
pub fn source_dimensions(bytes: &[u8], media_type: &str) -> Result<Option<(u32, u32)>, String> {
    let Some(format) = image_format(media_type) else {
        return Ok(None);
    };
    image_dimensions(bytes, format).map(Some)
}

/// Shrink `bytes` when either edge exceeds [`MAX_IMAGE_DIMENSION`].
///
/// An image inside the cap is forwarded byte for byte, which preserves
/// animation, the source encoder's settings, and every pixel a vision model
/// would consume. Downscaling is best effort: a decode or encode failure
/// returns the original bytes, because a file `infer` recognizes but this
/// decoder rejects may still be readable upstream, and the agent loop's
/// image-rejection recovery is the backstop for anything that slips through.
pub fn prepare_image(
    bytes: &[u8],
    media_type: &'static str,
    source_size: Option<(u32, u32)>,
) -> PreparedImage {
    let unchanged = |size| PreparedImage {
        bytes: bytes.to_vec(),
        media_type,
        size,
        source_size: size,
        changed: false,
    };

    let Some(format) = image_format(media_type) else {
        return unchanged(None);
    };
    let Some((width, height)) = source_size else {
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
            changed: true,
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
pub fn decode_image(
    bytes: &[u8],
    format: image::ImageFormat,
) -> Result<image::DynamicImage, String> {
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
pub fn decode_limits() -> image::Limits {
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
pub fn encode_downscaled(
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
pub fn has_transparency(image: &image::DynamicImage) -> bool {
    if !image.color().has_alpha() {
        return false;
    }
    image.to_rgba8().pixels().any(|pixel| pixel.0[3] < u8::MAX)
}

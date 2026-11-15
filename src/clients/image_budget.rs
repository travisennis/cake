//! Shared image codec helpers and the request image planner.
//!
//! Source acceptance and request sizing are separate concerns. `ReadImage`
//! uses the attachment helpers here to probe a source from its header, reject a
//! decompression bomb, and downscale an oversized image in memory. The request
//! planner reuses the same codecs so there is one encoding path. It verifies
//! image digests, downscales an image that individually exceeds the request
//! limits, and omits an image that would push a request over its budget. An
//! ordinary request admits images in source order and never revises an earlier
//! decision, so each request is a prefix-extension of the one before it;
//! checkpoint preparation may still retire unpinned images. Every omission
//! leaves a model-visible marker in the owning item's text.

use std::borrow::Cow;
use std::fmt;
use std::io::Cursor;

use base64::Engine as _;
use image::imageops::FilterType;

use crate::config::blob_store::ContentId;
use crate::config::settings::ResolvedImagePolicy;
use crate::types::{ConversationItem, ImagePart};

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

// =============================================================================
// Request image planner
// =============================================================================

/// One image in request source order, borrowed from the conversation.
#[derive(Debug)]
pub struct ImageSlot<'a> {
    /// One-based position of this image in the request's image sequence.
    pub ordinal: usize,
    /// The item and image index that own this slot.
    pub owner: ImageOwner,
    /// The image as it currently appears in the request projection.
    pub image: &'a ImagePart,
    /// Whether the planner is forbidden from omitting this image.
    ///
    /// Checkpoint preparation supplies pins; the ordinary request path ignores
    /// this field and never fails because of a budget.
    pub pinned: bool,
}

/// The conversation item and image index a slot belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageOwner {
    /// Zero-based index into the request history.
    pub item: usize,
    /// Zero-based index into that item's image vector.
    pub image: usize,
}

/// What the planner decided for one image slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImagePlanEntry {
    /// Emit the original image byte-for-byte.
    Keep,
    /// Emit a request-only re-encoded image.
    Replace(ImagePart),
    /// Leave the image out and append the marker to its owner.
    Omit {
        /// The budget or per-image condition that caused the omission.
        reason: ImageOmissionReason,
        /// The exact model-visible marker to append.
        marker: String,
    },
}

/// Why the planner omitted an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageOmissionReason {
    /// The request would carry too many images.
    Count,
    /// The request would carry too many decoded pixels.
    Pixels,
    /// The request would carry too many encoded bytes.
    Bytes,
    /// This image could not be reduced below a per-image limit.
    CannotFit,
}

impl fmt::Display for ImageOmissionReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self {
            Self::Count => "count",
            Self::Pixels => "pixels",
            Self::Bytes => "bytes",
            Self::CannotFit => "cannot fit",
        };
        formatter.write_str(reason)
    }
}

/// The aggregate request budget a pinned image set exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBudgetLimit {
    /// Maximum emitted image count.
    Count,
    /// Maximum emitted decoded pixels.
    Pixels,
    /// Maximum emitted encoded bytes.
    Bytes,
}

impl fmt::Display for ImageBudgetLimit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let budget = match self {
            Self::Count => "count",
            Self::Pixels => "pixels",
            Self::Bytes => "bytes",
        };
        formatter.write_str(budget)
    }
}

/// The change the planner made to one image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageBudgetChange {
    /// The original bytes were kept.
    Kept,
    /// The image was re-encoded at smaller dimensions.
    Downscaled,
    /// The image was left out of the request.
    Omitted(ImageOmissionReason),
}

/// One per-image record in an [`ImageBudgetReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBudgetRecord {
    /// One-based position in request source order.
    pub slot: usize,
    /// The image's original media type.
    pub media_type: String,
    /// Source dimensions, when the planner could probe them.
    pub original_size: Option<(u32, u32)>,
    /// Emitted dimensions, when the planner emitted an image.
    pub emitted_size: Option<(u32, u32)>,
    /// Decoded source bytes.
    pub original_bytes: usize,
    /// Encoded emitted bytes, or zero when omitted.
    pub emitted_bytes: usize,
    /// What happened to this image.
    pub change: ImageBudgetChange,
}

/// Totals and per-image changes for one request plan.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageBudgetReport {
    /// Number of images in the source projection.
    pub original_count: usize,
    /// Number of images the planner emits.
    pub emitted_count: usize,
    /// Decoded source pixels across all images the planner could probe.
    pub original_pixels: u64,
    /// Decoded emitted pixels.
    pub emitted_pixels: u64,
    /// Decoded source bytes across all images.
    pub original_bytes: usize,
    /// Encoded emitted bytes.
    pub emitted_bytes: usize,
    /// One record per source image, in source order.
    pub images: Vec<ImageBudgetRecord>,
}

/// A complete request image plan and its diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageBudgetPlan {
    /// One decision per input slot, in source order.
    pub entries: Vec<ImagePlanEntry>,
    /// The aggregate and per-image result.
    pub report: ImageBudgetReport,
}

/// A request image that cannot be planned safely or cannot fit its budgets.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImageBudgetError {
    /// The base64 payload is not valid standard base64.
    #[error("request image {slot} ({media_type}) is not valid base64: {detail}")]
    InvalidBase64 {
        /// One-based position in request source order.
        slot: usize,
        /// The image's media type.
        media_type: String,
        /// The decoder's message; it never contains payload bytes.
        detail: String,
    },
    /// The decoded bytes do not hash to the recorded content id.
    #[error(
        "request image {slot} ({media_type}) has content id {expected} but its bytes hash to {actual}"
    )]
    DigestMismatch {
        /// One-based position in request source order.
        slot: usize,
        /// The image's media type.
        media_type: String,
        /// The recorded digest.
        expected: String,
        /// The digest of the decoded bytes, or why the recorded digest is invalid.
        actual: String,
    },
    /// The image header could not be read.
    #[error("request image {slot} ({media_type}) has unreadable dimensions: {detail}")]
    DimensionProbe {
        /// One-based position in request source order.
        slot: usize,
        /// The image's media type.
        media_type: String,
        /// The probe's message.
        detail: String,
    },
    /// The source claims more pixels than the compiled source limit allows.
    #[error(
        "request image {slot} ({media_type}) has {pixels} pixels, above the {limit}-pixel source limit"
    )]
    DecompressionBomb {
        /// One-based position in request source order.
        slot: usize,
        /// The image's media type.
        media_type: String,
        /// Claimed decoded pixels.
        pixels: u64,
        /// The compiled source pixel limit.
        limit: u64,
    },
    /// A checkpoint's pinned images alone exceed an image budget.
    #[error(
        "pinned request images exceed the image {budget} budget: {used} used, {limit} allowed across {pinned_images} pinned image(s)"
    )]
    PinnedExceedsBudget {
        /// The budget that overflowed.
        budget: ImageBudgetLimit,
        /// The pinned total that overflowed.
        used: u64,
        /// The configured limit.
        limit: u64,
        /// Number of pinned images in the request.
        pinned_images: usize,
    },
    /// A checkpoint image that is pinned cannot be reduced below a per-image limit.
    #[error("request image {slot} ({media_type}) cannot fit the request image budget: {detail}")]
    PinnedCannotFit {
        /// One-based position in request source order.
        slot: usize,
        /// The image's media type.
        media_type: String,
        /// Why the image could not be reduced.
        detail: String,
    },
}

/// Which caller is planning, and therefore what an omission may say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanScope {
    /// An ordinary provider request. The emitted prefix is fixed, so a budget
    /// cannot be freed before the invocation ends.
    Request,
    /// Checkpoint preparation, where an unpinned image may be retired.
    Checkpoint,
}

/// Plan one checkpoint's retained image set.
///
/// Slots are processed in source order. Each image is decoded, digest-checked
/// when it carries a content id, and probed before the planner decides to keep
/// it, re-encode it, or omit it. Aggregate budgets are then enforced by
/// omitting the oldest unpinned image one at a time. A pinned image is never
/// omitted; if pinned images alone exceed a budget the plan fails instead.
///
/// Only checkpoint preparation uses this policy, because it commits a new
/// boundary. Ordinary requests use [`plan_admitted_images`].
///
/// # Errors
///
/// Returns an [`ImageBudgetError`] for invalid base64, a digest mismatch, an
/// unreadable header, a decompression bomb, or a pinned image that can never
/// fit. A corrupt or unsafe image is never silently omitted.
pub fn plan_image_budget(
    slots: &[ImageSlot<'_>],
    policy: &ResolvedImagePolicy,
) -> Result<ImageBudgetPlan, ImageBudgetError> {
    let mut entries = Vec::with_capacity(slots.len());
    let mut records = Vec::with_capacity(slots.len());
    let mut emitted_by_slot = Vec::with_capacity(slots.len());
    let mut totals = BudgetTotals::default();

    for slot in slots {
        let outcome = prepare_slot(slot, policy, PlanScope::Checkpoint)?;
        totals.original_count = totals.original_count.saturating_add(1);
        totals.original_pixels = totals
            .original_pixels
            .saturating_add(outcome.original_pixels);
        totals.original_bytes = totals
            .original_bytes
            .saturating_add(outcome.record.original_bytes);
        if let Some(emitted) = outcome.emitted {
            totals.emitted_count = totals.emitted_count.saturating_add(1);
            totals.emitted_pixels = totals.emitted_pixels.saturating_add(emitted.pixels);
            totals.emitted_bytes = totals.emitted_bytes.saturating_add(emitted.bytes);
        }
        emitted_by_slot.push(outcome.emitted);
        entries.push(outcome.entry);
        records.push(outcome.record);
    }

    enforce_aggregate_budgets(
        slots,
        policy,
        &mut entries,
        &mut records,
        &mut emitted_by_slot,
        &mut totals,
    )?;

    let report = ImageBudgetReport {
        original_count: totals.original_count,
        emitted_count: totals.emitted_count,
        original_pixels: totals.original_pixels,
        emitted_pixels: totals.emitted_pixels,
        original_bytes: totals.original_bytes,
        emitted_bytes: totals.emitted_bytes,
        images: records,
    };
    if !slots.is_empty() {
        tracing::debug!(target: "cake", ?report, "Planned checkpoint image budget");
    }
    Ok(ImageBudgetPlan { entries, report })
}

/// Plan one provider request's image set by admission only.
///
/// Slots are decided in source order from the slots before them. Each image is
/// decoded, digest-checked when it carries a content id, and probed; an image
/// that individually exceeds a per-image limit is downscaled, and an image that
/// would push the emitted totals over a budget is omitted. No earlier decision
/// is revisited, so planning a history and planning that history plus a suffix
/// agree on every original item. That is what keeps each provider request an
/// exact prefix-extension of the one before it.
///
/// [`ImageSlot::pinned`] is a checkpoint concern and is ignored here: an
/// ordinary request never fails because of image budgets.
///
/// # Errors
///
/// Returns an [`ImageBudgetError`] for invalid base64, a digest mismatch, an
/// unreadable header, or a decompression bomb.
pub fn plan_admitted_images(
    slots: &[ImageSlot<'_>],
    policy: &ResolvedImagePolicy,
) -> Result<ImageBudgetPlan, ImageBudgetError> {
    let mut entries = Vec::with_capacity(slots.len());
    let mut records = Vec::with_capacity(slots.len());
    let mut totals = BudgetTotals::default();

    for slot in slots {
        let mut outcome = prepare_slot(slot, policy, PlanScope::Request)?;
        totals.original_count = totals.original_count.saturating_add(1);
        totals.original_pixels = totals
            .original_pixels
            .saturating_add(outcome.original_pixels);
        totals.original_bytes = totals
            .original_bytes
            .saturating_add(outcome.record.original_bytes);
        if let Some(emitted) = &outcome.emitted
            && let Some(limit) = admission_limit(&totals, emitted, policy)
        {
            outcome = reject_admitted(slot, outcome, limit);
        }
        if let Some(emitted) = &outcome.emitted {
            totals.emitted_count = totals.emitted_count.saturating_add(1);
            totals.emitted_pixels = totals.emitted_pixels.saturating_add(emitted.pixels);
            totals.emitted_bytes = totals.emitted_bytes.saturating_add(emitted.bytes);
        }
        entries.push(outcome.entry);
        records.push(outcome.record);
    }

    let report = ImageBudgetReport {
        original_count: totals.original_count,
        emitted_count: totals.emitted_count,
        original_pixels: totals.original_pixels,
        emitted_pixels: totals.emitted_pixels,
        original_bytes: totals.original_bytes,
        emitted_bytes: totals.emitted_bytes,
        images: records,
    };
    if !slots.is_empty() {
        tracing::debug!(target: "cake", ?report, "Admitted request image budget");
    }
    Ok(ImageBudgetPlan { entries, report })
}

/// A pruned request projection together with the plan that produced it.
///
/// `owners` and `plan.entries` align with the image slots found in the
/// unpruned history. Add [`Self::fully_emitted_items`] to map an emitted image
/// back to its request item.
#[derive(Debug)]
pub struct RequestImagePlan<'a> {
    /// The request projection after admission.
    pub history: Cow<'a, [ConversationItem]>,
    /// The admission decisions for every source image.
    pub plan: ImageBudgetPlan,
    /// The item and image index for each plan entry.
    pub owners: Vec<ImageOwner>,
}

impl RequestImagePlan<'_> {
    /// Request items whose every image was emitted.
    ///
    /// A record with any omitted image is excluded: partial delivery is not
    /// consumption. Items without images have no plan entries and are absent.
    pub fn fully_emitted_items(&self) -> Vec<usize> {
        let mut emitted = vec![false; self.history.len()];
        let mut omitted = vec![false; self.history.len()];
        for (owner, entry) in self.owners.iter().zip(&self.plan.entries) {
            let Some(was_emitted) = emitted.get_mut(owner.item) else {
                continue;
            };
            match entry {
                ImagePlanEntry::Keep | ImagePlanEntry::Replace(_) => *was_emitted = true,
                ImagePlanEntry::Omit { .. } => {
                    if let Some(was_omitted) = omitted.get_mut(owner.item) {
                        *was_omitted = true;
                    }
                },
            }
        }
        emitted
            .into_iter()
            .zip(omitted)
            .enumerate()
            .filter_map(|(item, (emitted, omitted))| (emitted && !omitted).then_some(item))
            .collect()
    }
}

/// Prune a request projection and return the plan that produced it.
///
/// The history follows the same admission rules as [`prune_request`]. The plan
/// and owner list let an accepted response record which image-bearing source
/// records it actually sent.
///
/// # Errors
///
/// Returns an error when an image cannot be planned safely, including a corrupt
/// image or a digest mismatch. A budget never fails a request.
pub fn prune_request_with_plan<'a>(
    history: Cow<'a, [ConversationItem]>,
    policy: &ResolvedImagePolicy,
) -> anyhow::Result<RequestImagePlan<'a>> {
    let slots = collect_image_slots(&history);
    if slots.is_empty() {
        return Ok(RequestImagePlan {
            history,
            plan: ImageBudgetPlan::default(),
            owners: Vec::new(),
        });
    }
    let owners: Vec<ImageOwner> = slots.iter().map(|slot| slot.owner).collect();
    let plan = plan_admitted_images(&slots, policy)?;
    let every_image_fits = plan
        .entries
        .iter()
        .all(|entry| matches!(entry, ImagePlanEntry::Keep));
    drop(slots);
    if every_image_fits {
        return Ok(RequestImagePlan {
            history,
            plan,
            owners,
        });
    }
    let mut owned = history.into_owned();
    apply_plan_to_history(&mut owned, &owners, plan.clone())?;
    Ok(RequestImagePlan {
        history: Cow::Owned(owned),
        plan,
        owners,
    })
}

/// Prune a request projection to the configured image policy.
///
/// Images are admitted in source order and an image that would exceed a budget
/// is omitted, so the result never rewrites an image an earlier request already
/// projected. Returns the borrowed slice when every image is admitted, so an
/// image-free or under-budget request pays no copy. The original conversation
/// and session records are never modified.
///
/// # Errors
///
/// Returns an error when an image cannot be planned safely, including a corrupt
/// image or a digest mismatch. A budget never fails a request.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the agent uses the plan-bearing variant; this keeps the simple signature"
    )
)]
pub fn prune_request<'a>(
    history: Cow<'a, [ConversationItem]>,
    policy: &ResolvedImagePolicy,
) -> anyhow::Result<Cow<'a, [ConversationItem]>> {
    Ok(prune_request_with_plan(history, policy)?.history)
}

/// Verify every image digest in a request history without changing it.
///
/// Vision backends call this immediately before serialization. The planner
/// already checked the source projection; this catches a corrupt or altered
/// copy that would otherwise reach the provider.
///
/// # Errors
///
/// Returns an [`ImageBudgetError`] for invalid base64 or a digest mismatch.
pub fn verify_request_images(history: &[ConversationItem]) -> Result<(), ImageBudgetError> {
    let slots = collect_image_slots(history);
    for slot in &slots {
        decode_slot_bytes(slot)?;
    }
    Ok(())
}

/// Apply a plan to a request projection in place.
///
/// `owners` must align with `plan.entries` and come from the same history the
/// plan was computed from. Replacements are assigned first; omissions remove
/// their image and append their marker in source order.
pub fn apply_plan_to_history(
    history: &mut [ConversationItem],
    owners: &[ImageOwner],
    plan: ImageBudgetPlan,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        owners.len() == plan.entries.len(),
        "image plan has {} entries for {} owners",
        plan.entries.len(),
        owners.len()
    );
    let mut markers_by_item: Vec<Vec<String>> = vec![Vec::new(); history.len()];
    let mut omissions_by_item: Vec<Vec<usize>> = vec![Vec::new(); history.len()];

    for (owner, entry) in owners.iter().zip(plan.entries) {
        match entry {
            ImagePlanEntry::Keep => {},
            ImagePlanEntry::Replace(image) => replace_image(history, *owner, image)?,
            ImagePlanEntry::Omit { marker, .. } => record_omission(
                history,
                *owner,
                marker,
                &mut markers_by_item,
                &mut omissions_by_item,
            )?,
        }
    }
    apply_omissions(history, markers_by_item, omissions_by_item)
}

fn replace_image(
    history: &mut [ConversationItem],
    owner: ImageOwner,
    image: ImagePart,
) -> anyhow::Result<()> {
    let images = image_vector_mut(history, owner.item)?;
    let image_slot = images.get_mut(owner.image).ok_or_else(|| {
        anyhow::anyhow!(
            "image plan owner {}:{} is outside the history item",
            owner.item,
            owner.image
        )
    })?;
    *image_slot = image;
    Ok(())
}

fn record_omission(
    history: &[ConversationItem],
    owner: ImageOwner,
    marker: String,
    markers_by_item: &mut [Vec<String>],
    omissions_by_item: &mut [Vec<usize>],
) -> anyhow::Result<()> {
    let item = history.get(owner.item).ok_or_else(|| {
        anyhow::anyhow!(
            "image plan owner {} is outside the request history",
            owner.item
        )
    })?;
    anyhow::ensure!(
        owner.image < item_images(item).len(),
        "image plan owner {}:{} is outside the history item",
        owner.item,
        owner.image
    );
    markers_by_item[owner.item].push(marker);
    omissions_by_item[owner.item].push(owner.image);
    Ok(())
}

fn apply_omissions(
    history: &mut [ConversationItem],
    markers_by_item: Vec<Vec<String>>,
    mut omissions_by_item: Vec<Vec<usize>>,
) -> anyhow::Result<()> {
    for (item_index, markers) in markers_by_item.into_iter().enumerate() {
        if markers.is_empty() {
            continue;
        }
        let images = image_vector_mut(history, item_index)?;
        let mut omissions = std::mem::take(&mut omissions_by_item[item_index]);
        omissions.sort_unstable();
        for image_index in omissions.into_iter().rev() {
            images.remove(image_index);
        }
        let text = item_text_mut(history, item_index)?;
        append_markers(text, &markers);
    }
    Ok(())
}

fn prepare_slot(
    slot: &ImageSlot<'_>,
    policy: &ResolvedImagePolicy,
    scope: PlanScope,
) -> Result<SlotOutcome, ImageBudgetError> {
    let bytes = decode_slot_bytes(slot)?;
    let dimensions = slot_dimensions(slot, &bytes)?;
    let original_bytes = bytes.len();
    let Some(size) = dimensions else {
        if original_bytes > policy.max_bytes {
            return cannot_fit_outcome(
                slot,
                None,
                original_bytes,
                "opaque image is over the byte budget",
                scope,
            );
        }
        return Ok(keep_outcome(slot, None, original_bytes));
    };

    let original_pixels = pixel_count(size);
    if original_pixels > MAX_SOURCE_PIXELS {
        return Err(ImageBudgetError::DecompressionBomb {
            slot: slot.ordinal,
            media_type: slot.image.media_type.clone(),
            pixels: original_pixels,
            limit: MAX_SOURCE_PIXELS,
        });
    }
    if image_fits(size, original_pixels, original_bytes, policy) {
        return Ok(keep_outcome(slot, Some(size), original_bytes));
    }
    downscale_slot(slot, &bytes, size, policy, scope)
}

fn decode_slot_bytes(slot: &ImageSlot<'_>) -> Result<Vec<u8>, ImageBudgetError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&slot.image.data_base64)
        .map_err(|error| ImageBudgetError::InvalidBase64 {
            slot: slot.ordinal,
            media_type: slot.image.media_type.clone(),
            detail: error.to_string(),
        })?;
    let Some(content_id) = slot.image.content_id.as_deref() else {
        return Ok(bytes);
    };
    let Ok(expected) = ContentId::parse(content_id) else {
        return Err(ImageBudgetError::DigestMismatch {
            slot: slot.ordinal,
            media_type: slot.image.media_type.clone(),
            expected: content_id.to_string(),
            actual: "invalid content id".to_string(),
        });
    };
    if !expected.matches(&bytes) {
        return Err(ImageBudgetError::DigestMismatch {
            slot: slot.ordinal,
            media_type: slot.image.media_type.clone(),
            expected: content_id.to_string(),
            actual: ContentId::from_bytes(&bytes).to_string(),
        });
    }
    Ok(bytes)
}

fn slot_dimensions(
    slot: &ImageSlot<'_>,
    bytes: &[u8],
) -> Result<Option<(u32, u32)>, ImageBudgetError> {
    if !is_supported_media_type(&slot.image.media_type) {
        return Ok(None);
    }
    let dimensions = source_dimensions(bytes, &slot.image.media_type).map_err(|detail| {
        ImageBudgetError::DimensionProbe {
            slot: slot.ordinal,
            media_type: slot.image.media_type.clone(),
            detail,
        }
    })?;
    match dimensions {
        Some((width, height)) if width > 0 && height > 0 => Ok(Some((width, height))),
        Some(_) => Err(ImageBudgetError::DimensionProbe {
            slot: slot.ordinal,
            media_type: slot.image.media_type.clone(),
            detail: "the image reports a zero dimension".to_string(),
        }),
        None => Err(ImageBudgetError::DimensionProbe {
            slot: slot.ordinal,
            media_type: slot.image.media_type.clone(),
            detail: "no decoder for a supported media type".to_string(),
        }),
    }
}

fn keep_outcome(
    slot: &ImageSlot<'_>,
    dimensions: Option<(u32, u32)>,
    original_bytes: usize,
) -> SlotOutcome {
    let pixels = dimensions.map_or(0, pixel_count);
    SlotOutcome {
        entry: ImagePlanEntry::Keep,
        record: ImageBudgetRecord {
            slot: slot.ordinal,
            media_type: slot.image.media_type.clone(),
            original_size: dimensions,
            emitted_size: dimensions,
            original_bytes,
            emitted_bytes: original_bytes,
            change: ImageBudgetChange::Kept,
        },
        emitted: Some(EmittedStats {
            pixels,
            bytes: original_bytes,
        }),
        original_pixels: pixels,
    }
}

fn downscale_slot(
    slot: &ImageSlot<'_>,
    bytes: &[u8],
    source_size: (u32, u32),
    policy: &ResolvedImagePolicy,
    scope: PlanScope,
) -> Result<SlotOutcome, ImageBudgetError> {
    match downscale_to_budget(bytes, &slot.image.media_type, source_size, policy) {
        Ok(downscaled) => {
            let emitted_bytes = downscaled.bytes.len();
            let emitted_size = downscaled.size;
            let digest = ContentId::from_bytes(&downscaled.bytes).to_string();
            let image = ImagePart {
                media_type: downscaled.media_type.to_string(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(&downscaled.bytes),
                content_id: Some(digest),
            };
            Ok(SlotOutcome {
                entry: ImagePlanEntry::Replace(image),
                record: ImageBudgetRecord {
                    slot: slot.ordinal,
                    media_type: slot.image.media_type.clone(),
                    original_size: Some(source_size),
                    emitted_size: Some(emitted_size),
                    original_bytes: bytes.len(),
                    emitted_bytes,
                    change: ImageBudgetChange::Downscaled,
                },
                emitted: Some(EmittedStats {
                    pixels: pixel_count(emitted_size),
                    bytes: emitted_bytes,
                }),
                original_pixels: pixel_count(source_size),
            })
        },
        Err(detail) => cannot_fit_outcome(slot, Some(source_size), bytes.len(), &detail, scope),
    }
}

fn cannot_fit_outcome(
    slot: &ImageSlot<'_>,
    dimensions: Option<(u32, u32)>,
    original_bytes: usize,
    detail: &str,
    scope: PlanScope,
) -> Result<SlotOutcome, ImageBudgetError> {
    if slot.pinned {
        return Err(ImageBudgetError::PinnedCannotFit {
            slot: slot.ordinal,
            media_type: slot.image.media_type.clone(),
            detail: detail.to_string(),
        });
    }
    Ok(omitted_outcome(
        slot,
        dimensions,
        original_bytes,
        ImageOmissionReason::CannotFit,
        scope,
    ))
}

fn omitted_outcome(
    slot: &ImageSlot<'_>,
    dimensions: Option<(u32, u32)>,
    original_bytes: usize,
    reason: ImageOmissionReason,
    scope: PlanScope,
) -> SlotOutcome {
    let marker = omission_marker(
        reason,
        &slot.image.media_type,
        dimensions,
        original_bytes,
        scope,
    );
    SlotOutcome {
        entry: ImagePlanEntry::Omit { reason, marker },
        record: ImageBudgetRecord {
            slot: slot.ordinal,
            media_type: slot.image.media_type.clone(),
            original_size: dimensions,
            emitted_size: None,
            original_bytes,
            emitted_bytes: 0,
            change: ImageBudgetChange::Omitted(reason),
        },
        emitted: None,
        original_pixels: dimensions.map_or(0, pixel_count),
    }
}

fn downscale_to_budget(
    bytes: &[u8],
    media_type: &str,
    source_size: (u32, u32),
    policy: &ResolvedImagePolicy,
) -> Result<Downscaled, String> {
    let format =
        image_format(media_type).ok_or_else(|| "no decoder for this media type".to_string())?;
    let image = decode_image(bytes, format)?;
    let (start_width, start_height) =
        bounded_dimensions(source_size.0, source_size.1, policy.max_pixels);
    let mut previous = (start_width, start_height);
    let mut candidate = encode_candidate(&image, start_width, start_height, format)?;
    if candidate.bytes.len() <= policy.max_bytes {
        return Ok(candidate);
    }

    for step in 1..=8_i32 {
        let factor = 0.85_f64.powi(step);
        let next = (
            scaled_dimension(start_width, factor),
            scaled_dimension(start_height, factor),
        );
        if next == previous {
            break;
        }
        previous = next;
        candidate = encode_candidate(&image, next.0, next.1, format)?;
        if candidate.bytes.len() <= policy.max_bytes {
            return Ok(candidate);
        }
    }
    Err(format!(
        "still {} bytes after eight reductions",
        candidate.bytes.len()
    ))
}

fn encode_candidate(
    image: &image::DynamicImage,
    width: u32,
    height: u32,
    format: image::ImageFormat,
) -> Result<Downscaled, String> {
    let resized = image.resize(width, height, FilterType::Lanczos3);
    encode_downscaled(&resized, format)
}

/// Largest dimensions that fit the request's per-image edge and pixel bounds.
///
/// `max_pixels` is at most `u32::MAX`, so every value here is exactly
/// representable in `f64`; `floor_to_u32` clamps and converts the final scale.
#[expect(
    clippy::cast_precision_loss,
    reason = "image dimensions and the pixel budget are u32-range values, exactly representable in f64"
)]
fn bounded_dimensions(width: u32, height: u32, max_pixels: u64) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (width.max(1), height.max(1));
    }
    let pixels = pixel_count((width, height));
    let longest = u64::from(width.max(height));
    let edge_scale = f64::from(MAX_IMAGE_DIMENSION) / longest as f64;
    let pixel_scale = (max_pixels as f64 / pixels as f64).sqrt();
    let scale = edge_scale.min(pixel_scale).min(1.0);
    let mut bounded_width = u64::from(floor_to_u32(f64::from(width) * scale));
    let mut bounded_height = u64::from(floor_to_u32(f64::from(height) * scale));
    while bounded_width.saturating_mul(bounded_height) > max_pixels {
        if bounded_width >= bounded_height && bounded_width > 1 {
            bounded_width -= 1;
        } else if bounded_height > 1 {
            bounded_height -= 1;
        } else {
            break;
        }
    }
    (
        u32::try_from(bounded_width).unwrap_or(u32::MAX),
        u32::try_from(bounded_height).unwrap_or(u32::MAX),
    )
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to the positive u32 range before conversion"
)]
fn floor_to_u32(value: f64) -> u32 {
    value.max(1.0).min(f64::from(u32::MAX)).floor() as u32
}

fn scaled_dimension(value: u32, factor: f64) -> u32 {
    floor_to_u32(f64::from(value) * factor)
}

fn pixel_count(size: (u32, u32)) -> u64 {
    u64::from(size.0).saturating_mul(u64::from(size.1))
}

const fn image_fits(
    size: (u32, u32),
    pixels: u64,
    bytes: usize,
    policy: &ResolvedImagePolicy,
) -> bool {
    size.0 <= MAX_IMAGE_DIMENSION
        && size.1 <= MAX_IMAGE_DIMENSION
        && pixels <= policy.max_pixels
        && bytes <= policy.max_bytes
}

fn omission_marker(
    reason: ImageOmissionReason,
    media_type: &str,
    dimensions: Option<(u32, u32)>,
    bytes: usize,
    scope: PlanScope,
) -> String {
    let mut marker = match dimensions {
        Some((width, height)) => {
            format!("[image omitted: {reason}; {media_type}, {width}x{height}, {bytes} bytes")
        },
        None => format!("[image omitted: {reason}; {media_type}, {bytes} bytes"),
    };
    if scope == PlanScope::Request {
        marker.push_str(request_omission_advice(reason));
    }
    marker.push(']');
    marker
}

/// What a request-side omission tells the model to do instead.
///
/// A request cannot free image budget, so the advice names an action the model
/// can take with the source file or with text it already has.
const fn request_omission_advice(reason: ImageOmissionReason) -> &'static str {
    match reason {
        ImageOmissionReason::CannotFit => "; crop or downscale the source, or continue from text",
        ImageOmissionReason::Count | ImageOmissionReason::Pixels | ImageOmissionReason::Bytes => {
            "; the request image budget is exhausted for this run, so crop or downscale the source, or continue from text"
        },
    }
}

fn enforce_aggregate_budgets(
    slots: &[ImageSlot<'_>],
    policy: &ResolvedImagePolicy,
    entries: &mut [ImagePlanEntry],
    records: &mut [ImageBudgetRecord],
    emitted_by_slot: &mut [Option<EmittedStats>],
    totals: &mut BudgetTotals,
) -> Result<(), ImageBudgetError> {
    loop {
        let Some((budget, used, limit)) = exceeded_budget(totals, policy) else {
            return Ok(());
        };
        let Some(index) = oldest_unpinned_index(slots, entries) else {
            let pinned_images = slots.iter().filter(|slot| slot.pinned).count();
            return Err(ImageBudgetError::PinnedExceedsBudget {
                budget,
                used,
                limit,
                pinned_images,
            });
        };
        omit_planned(
            slots,
            entries,
            records,
            emitted_by_slot,
            totals,
            index,
            budget,
        );
    }
}

fn exceeded_budget(
    totals: &BudgetTotals,
    policy: &ResolvedImagePolicy,
) -> Option<(ImageBudgetLimit, u64, u64)> {
    if totals.emitted_count > policy.max_images {
        return Some((
            ImageBudgetLimit::Count,
            as_u64(totals.emitted_count),
            as_u64(policy.max_images),
        ));
    }
    if totals.emitted_pixels > policy.max_pixels {
        return Some((
            ImageBudgetLimit::Pixels,
            totals.emitted_pixels,
            policy.max_pixels,
        ));
    }
    if totals.emitted_bytes > policy.max_bytes {
        return Some((
            ImageBudgetLimit::Bytes,
            as_u64(totals.emitted_bytes),
            as_u64(policy.max_bytes),
        ));
    }
    None
}

fn oldest_unpinned_index(slots: &[ImageSlot<'_>], entries: &[ImagePlanEntry]) -> Option<usize> {
    slots
        .iter()
        .enumerate()
        .find(|(index, slot)| {
            !slot.pinned
                && !matches!(
                    entries.get(*index),
                    None | Some(ImagePlanEntry::Omit { .. })
                )
        })
        .map(|(index, _)| index)
}

fn omit_planned(
    slots: &[ImageSlot<'_>],
    entries: &mut [ImagePlanEntry],
    records: &mut [ImageBudgetRecord],
    emitted_by_slot: &mut [Option<EmittedStats>],
    totals: &mut BudgetTotals,
    index: usize,
    budget: ImageBudgetLimit,
) {
    let Some(slot) = slots.get(index) else {
        return;
    };
    let Some(record) = records.get_mut(index) else {
        return;
    };
    let reason = omission_reason(budget);
    let marker = omission_marker(
        reason,
        &slot.image.media_type,
        record.original_size,
        record.original_bytes,
        PlanScope::Checkpoint,
    );
    if let Some(entry) = entries.get_mut(index) {
        *entry = ImagePlanEntry::Omit { reason, marker };
    }
    record.change = ImageBudgetChange::Omitted(reason);
    record.emitted_size = None;
    record.emitted_bytes = 0;
    if let Some(emitted) = emitted_by_slot.get_mut(index).and_then(Option::take) {
        totals.emitted_count = totals.emitted_count.saturating_sub(1);
        totals.emitted_pixels = totals.emitted_pixels.saturating_sub(emitted.pixels);
        totals.emitted_bytes = totals.emitted_bytes.saturating_sub(emitted.bytes);
    }
}

const fn omission_reason(budget: ImageBudgetLimit) -> ImageOmissionReason {
    match budget {
        ImageBudgetLimit::Count => ImageOmissionReason::Count,
        ImageBudgetLimit::Pixels => ImageOmissionReason::Pixels,
        ImageBudgetLimit::Bytes => ImageOmissionReason::Bytes,
    }
}

/// The budget that admitting one more image would exceed.
const fn admission_limit(
    totals: &BudgetTotals,
    emitted: &EmittedStats,
    policy: &ResolvedImagePolicy,
) -> Option<ImageBudgetLimit> {
    if totals.emitted_count.saturating_add(1) > policy.max_images {
        return Some(ImageBudgetLimit::Count);
    }
    if totals.emitted_pixels.saturating_add(emitted.pixels) > policy.max_pixels {
        return Some(ImageBudgetLimit::Pixels);
    }
    if totals.emitted_bytes.saturating_add(emitted.bytes) > policy.max_bytes {
        return Some(ImageBudgetLimit::Bytes);
    }
    None
}

/// Turn an admitted outcome into an omission without touching earlier slots.
fn reject_admitted(
    slot: &ImageSlot<'_>,
    mut outcome: SlotOutcome,
    limit: ImageBudgetLimit,
) -> SlotOutcome {
    let reason = omission_reason(limit);
    let marker = omission_marker(
        reason,
        &slot.image.media_type,
        outcome.record.original_size,
        outcome.record.original_bytes,
        PlanScope::Request,
    );
    outcome.entry = ImagePlanEntry::Omit { reason, marker };
    outcome.record.change = ImageBudgetChange::Omitted(reason);
    outcome.record.emitted_size = None;
    outcome.record.emitted_bytes = 0;
    outcome.emitted = None;
    outcome
}

fn collect_image_slots(history: &[ConversationItem]) -> Vec<ImageSlot<'_>> {
    let mut slots = Vec::new();
    for (item_index, item) in history.iter().enumerate() {
        for (image_index, image) in item_images(item).iter().enumerate() {
            slots.push(ImageSlot {
                ordinal: slots.len() + 1,
                owner: ImageOwner {
                    item: item_index,
                    image: image_index,
                },
                image,
                pinned: false,
            });
        }
    }
    slots
}

fn item_images(item: &ConversationItem) -> &[ImagePart] {
    match item {
        ConversationItem::Message { images, .. }
        | ConversationItem::FunctionCallOutput { images, .. } => images,
        ConversationItem::FunctionCall { .. } | ConversationItem::Reasoning { .. } => &[],
    }
}

fn image_vector_mut(
    history: &mut [ConversationItem],
    item_index: usize,
) -> anyhow::Result<&mut Vec<ImagePart>> {
    let item = history.get_mut(item_index).ok_or_else(|| {
        anyhow::anyhow!("image plan owner {item_index} is outside the request history")
    })?;
    match item {
        ConversationItem::Message { images, .. }
        | ConversationItem::FunctionCallOutput { images, .. } => Ok(images),
        ConversationItem::FunctionCall { .. } | ConversationItem::Reasoning { .. } => {
            anyhow::bail!("image plan owner {item_index} has no image vector")
        },
    }
}

fn item_text_mut(
    history: &mut [ConversationItem],
    item_index: usize,
) -> anyhow::Result<&mut String> {
    let item = history.get_mut(item_index).ok_or_else(|| {
        anyhow::anyhow!("image plan owner {item_index} is outside the request history")
    })?;
    match item {
        ConversationItem::Message { content, .. } => Ok(content),
        ConversationItem::FunctionCallOutput { output, .. } => Ok(output),
        ConversationItem::FunctionCall { .. } | ConversationItem::Reasoning { .. } => {
            anyhow::bail!("image plan owner {item_index} has no text")
        },
    }
}

fn append_markers(text: &mut String, markers: &[String]) {
    for marker in markers {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(marker);
    }
}

fn as_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[derive(Debug, Clone, Copy)]
struct EmittedStats {
    pixels: u64,
    bytes: usize,
}

#[derive(Debug, Default)]
struct BudgetTotals {
    original_count: usize,
    emitted_count: usize,
    original_pixels: u64,
    emitted_pixels: u64,
    original_bytes: usize,
    emitted_bytes: usize,
}

struct SlotOutcome {
    entry: ImagePlanEntry,
    record: ImageBudgetRecord,
    emitted: Option<EmittedStats>,
    original_pixels: u64,
}

#[cfg(test)]
#[path = "image_budget_tests.rs"]
mod tests;

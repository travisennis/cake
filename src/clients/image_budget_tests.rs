use std::borrow::Cow;
use std::io::Cursor;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};

use super::*;
use crate::config::blob_store::ContentId;
use crate::config::settings::ResolvedImagePolicy;

fn loose_policy() -> ResolvedImagePolicy {
    ResolvedImagePolicy {
        max_images: 8,
        max_bytes: 64 * 1024 * 1024,
        max_pixels: 100_000_000,
    }
}

fn policy(max_images: usize, max_bytes: usize, max_pixels: u64) -> ResolvedImagePolicy {
    ResolvedImagePolicy {
        max_images,
        max_bytes,
        max_pixels,
    }
}

fn encode_image(width: u32, height: u32, format: ImageFormat) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| {
        Rgb([
            u8::try_from(x % 256).unwrap(),
            u8::try_from(y % 256).unwrap(),
            u8::try_from((x + y) % 256).unwrap(),
        ])
    }));
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), format)
        .unwrap();
    bytes
}

fn image_part(media_type: &str, bytes: Vec<u8>) -> ImagePart {
    ImagePart {
        media_type: media_type.to_string(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        content_id: None,
    }
}

fn png_part(width: u32, height: u32) -> ImagePart {
    image_part("image/png", encode_image(width, height, ImageFormat::Png))
}

fn decoded(part: &ImagePart) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(&part.data_base64)
        .unwrap()
}

fn plan_parts(
    parts: &[ImagePart],
    pinned: &[usize],
    policy: &ResolvedImagePolicy,
) -> Result<ImageBudgetPlan, ImageBudgetError> {
    let slots: Vec<ImageSlot<'_>> = parts
        .iter()
        .enumerate()
        .map(|(index, image)| ImageSlot {
            ordinal: index + 1,
            owner: ImageOwner {
                item: 0,
                image: index,
            },
            image,
            pinned: pinned.contains(&index),
        })
        .collect();
    plan_image_budget(&slots, policy)
}

fn count_only_parts() -> Vec<ImagePart> {
    (0..3).map(|_| png_part(10, 10)).collect()
}

fn user_message(content: &str, images: Vec<ImagePart>) -> ConversationItem {
    ConversationItem::Message {
        role: Role::User,
        content: content.to_string(),
        id: None,
        status: None,
        timestamp: None,
        images,
    }
}

#[test]
fn count_budget_drops_the_oldest_unpinned_image_and_marks_it() {
    let parts = count_only_parts();
    let plan = plan_parts(
        &parts,
        &[2],
        &policy(2, loose_policy().max_bytes, 100_000_000),
    )
    .unwrap();

    assert_eq!(plan.report.original_count, 3);
    assert_eq!(plan.report.emitted_count, 2);
    assert_eq!(
        plan.report.images[0].change,
        ImageBudgetChange::Omitted(ImageOmissionReason::Count)
    );
    assert!(matches!(plan.entries[1], ImagePlanEntry::Keep));
    assert!(matches!(plan.entries[2], ImagePlanEntry::Keep));
    let marker = format!(
        "[image omitted: count; image/png, 10x10, {} bytes]",
        decoded(&parts[0]).len()
    );
    match &plan.entries[0] {
        ImagePlanEntry::Omit {
            reason: ImageOmissionReason::Count,
            marker: actual,
        } => assert_eq!(actual, &marker),
        other => panic!("expected a count omission, got {other:?}"),
    }
}

#[test]
fn pixel_budget_drops_the_oldest_unpinned_image() {
    let parts = vec![png_part(100, 100), png_part(100, 100)];
    let plan = plan_parts(&parts, &[1], &policy(8, loose_policy().max_bytes, 15_000)).unwrap();

    assert_eq!(plan.report.emitted_count, 1);
    assert_eq!(plan.report.emitted_pixels, 10_000);
    assert_eq!(
        plan.report.images[0].change,
        ImageBudgetChange::Omitted(ImageOmissionReason::Pixels)
    );
    assert!(matches!(
        plan.entries[0],
        ImagePlanEntry::Omit {
            reason: ImageOmissionReason::Pixels,
            ..
        }
    ));
}

#[test]
fn byte_budget_drops_the_oldest_unpinned_image() {
    let parts = vec![png_part(100, 100), png_part(100, 100)];
    let each = decoded(&parts[0]).len();
    let plan = plan_parts(&parts, &[1], &policy(8, each + each / 2, 100_000_000)).unwrap();

    assert_eq!(plan.report.emitted_count, 1);
    assert_eq!(plan.report.emitted_bytes, each);
    assert_eq!(
        plan.report.images[0].change,
        ImageBudgetChange::Omitted(ImageOmissionReason::Bytes)
    );
}

#[test]
fn per_image_downscale_preserves_aspect_and_png_format() {
    let parts = vec![png_part(2400, 600)];
    let plan = plan_parts(&parts, &[0], &loose_policy()).unwrap();

    let ImagePlanEntry::Replace(image) = &plan.entries[0] else {
        panic!(
            "expected the image to be downscaled, got {:?}",
            plan.entries[0]
        );
    };
    assert_eq!(image.media_type, "image/png");
    assert!(image.content_id.is_some());
    let bytes = decoded(image);
    let decoded_image = image::load_from_memory(&bytes).unwrap();
    assert_eq!((decoded_image.width(), decoded_image.height()), (2048, 512));
    assert!(
        (f64::from(decoded_image.width()) / f64::from(decoded_image.height()) - 4.0).abs() < 0.01
    );
    assert_eq!(plan.report.images[0].change, ImageBudgetChange::Downscaled);
    assert_eq!(plan.report.images[0].emitted_size, Some((2048, 512)));
    assert_eq!(
        image.content_id.as_deref(),
        Some(ContentId::from_bytes(&bytes).as_str())
    );
}

#[test]
fn downscaled_jpeg_gif_and_webp_become_jpeg() {
    for source in [ImageFormat::Jpeg, ImageFormat::Gif, ImageFormat::WebP] {
        let media_type = match source {
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::WebP => "image/webp",
            _ => unreachable!(),
        };
        let parts = vec![image_part(media_type, encode_image(2400, 600, source))];
        let plan = plan_parts(&parts, &[0], &loose_policy()).unwrap();

        let ImagePlanEntry::Replace(image) = &plan.entries[0] else {
            panic!("{source:?} should be downscaled, got {:?}", plan.entries[0]);
        };
        assert_eq!(image.media_type, "image/jpeg", "{source:?}");
        let bytes = decoded(image);
        assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Jpeg);
        let decoded_image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded_image.width(), decoded_image.height()), (2048, 512));
    }
}

#[test]
fn transparent_gif_downscales_to_png() {
    let mut source = RgbaImage::from_fn(2400, 600, |x, y| {
        Rgba([
            u8::try_from(x % 256).unwrap(),
            u8::try_from(y % 256).unwrap(),
            0,
            255,
        ])
    });
    source.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(source)
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Gif)
        .unwrap();
    let parts = vec![image_part("image/gif", bytes)];
    let plan = plan_parts(&parts, &[0], &loose_policy()).unwrap();

    let ImagePlanEntry::Replace(image) = &plan.entries[0] else {
        panic!("a transparent GIF should be downscaled");
    };
    assert_eq!(image.media_type, "image/png");
    let decoded_image = image::load_from_memory(&decoded(image)).unwrap();
    assert!(has_transparency(&decoded_image));
}

#[test]
fn small_image_is_kept_and_bounded_dimensions_never_upscale() {
    assert_eq!(bounded_dimensions(10, 10, 100_000_000), (10, 10));
    assert_eq!(bounded_dimensions(2400, 600, 100_000_000), (2048, 512));

    let parts = vec![png_part(10, 10)];
    let plan = plan_parts(&parts, &[0], &loose_policy()).unwrap();
    assert!(matches!(plan.entries[0], ImagePlanEntry::Keep));
    assert_eq!(plan.report.emitted_bytes, decoded(&parts[0]).len());
}

#[test]
fn digest_mismatch_is_a_hard_error() {
    let mut part = png_part(10, 10);
    part.content_id = Some(ContentId::from_bytes(b"different bytes").to_string());

    let error = plan_parts(&[part], &[], &loose_policy()).unwrap_err();
    assert!(matches!(
        error,
        ImageBudgetError::DigestMismatch { slot: 1, .. }
    ));
    assert!(error.to_string().contains("image/png"));
}

#[test]
fn invalid_base64_is_a_hard_error() {
    let part = ImagePart {
        media_type: "image/png".to_string(),
        data_base64: "not base64!".to_string(),
        content_id: None,
    };

    let error = plan_parts(&[part], &[], &loose_policy()).unwrap_err();
    assert!(matches!(
        error,
        ImageBudgetError::InvalidBase64 { slot: 1, .. }
    ));
}

#[test]
fn opaque_image_is_kept_under_budget_and_has_no_pixels() {
    let part = image_part("image/tiff", b"opaque bytes".to_vec());
    let plan = plan_parts(&[part], &[], &loose_policy()).unwrap();

    assert!(matches!(plan.entries[0], ImagePlanEntry::Keep));
    assert_eq!(plan.report.emitted_pixels, 0);
    assert_eq!(plan.report.emitted_bytes, b"opaque bytes".len());
}

#[test]
fn opaque_image_over_budget_is_omitted_or_rejected_when_pinned() {
    let part = image_part("image/tiff", b"opaque bytes".to_vec());
    let tight = policy(8, 4, 100_000_000);
    let plan = plan_parts(std::slice::from_ref(&part), &[], &tight).unwrap();
    let marker = "[image omitted: cannot fit; image/tiff, 12 bytes]";
    match &plan.entries[0] {
        ImagePlanEntry::Omit {
            reason: ImageOmissionReason::CannotFit,
            marker: actual,
        } => assert_eq!(actual, marker),
        other => panic!("expected a cannot-fit omission, got {other:?}"),
    }

    let error = plan_parts(&[part], &[0], &tight).unwrap_err();
    assert!(matches!(
        error,
        ImageBudgetError::PinnedCannotFit { slot: 1, .. }
    ));
}

#[test]
fn pinned_images_that_exceed_the_count_budget_error() {
    let parts = vec![png_part(10, 10), png_part(10, 10)];
    let error = plan_parts(
        &parts,
        &[0, 1],
        &policy(1, loose_policy().max_bytes, 100_000_000),
    )
    .unwrap_err();

    match error {
        ImageBudgetError::PinnedExceedsBudget {
            budget: ImageBudgetLimit::Count,
            used: 2,
            limit: 1,
            pinned_images: 2,
        } => {},
        other => panic!("expected a pinned count overflow, got {other:?}"),
    }
}

#[test]
fn decompression_bomb_is_rejected_before_decode() {
    // A GIF logical screen of 65535x65535 with a valid 1x1 frame. The header
    // probe sees the claimed pixels; a full decode would need more than 16 GiB.
    let bomb = [
        b'G', b'I', b'F', b'8', b'9', b'a', 0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x2c, 0x00,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
    ];
    let parts = vec![image_part("image/gif", bomb.to_vec())];
    let error = plan_parts(&parts, &[], &loose_policy()).unwrap_err();

    assert!(matches!(
        error,
        ImageBudgetError::DecompressionBomb {
            pixels: 4_294_836_225,
            ..
        }
    ));
}

#[test]
fn prune_request_returns_the_borrowed_history_when_every_image_fits() {
    let history = vec![user_message("one image", vec![png_part(10, 10)])];
    let pruned = prune_request(Cow::Borrowed(&history), &loose_policy()).unwrap();

    assert!(matches!(pruned, Cow::Borrowed(_)));
    assert_eq!(pruned.as_ref(), history.as_slice());
}

#[test]
fn prune_request_drops_oldest_images_keeps_the_newest_and_leaves_raw_history() {
    let old_image = png_part(10, 10);
    let old_bytes = decoded(&old_image).len();
    let history = vec![
        user_message("old", vec![old_image, png_part(10, 10)]),
        user_message("new", vec![png_part(10, 10)]),
    ];
    let pruned = prune_request(
        Cow::Borrowed(&history),
        &policy(2, loose_policy().max_bytes, 100_000_000),
    )
    .unwrap();

    let Cow::Owned(pruned) = pruned else {
        panic!("a pruned history must be owned");
    };
    let ConversationItem::Message {
        content, images, ..
    } = &pruned[0]
    else {
        panic!("first item should be a message");
    };
    assert_eq!(
        content,
        &format!("old\n[image omitted: count; image/png, 10x10, {old_bytes} bytes]")
    );
    assert_eq!(images.len(), 1);
    let ConversationItem::Message { images, .. } = &pruned[1] else {
        panic!("second item should be a message");
    };
    assert_eq!(images.len(), 1);

    let ConversationItem::Message {
        content, images, ..
    } = &history[0]
    else {
        panic!("raw history should still be a message");
    };
    assert_eq!(content, "old");
    assert_eq!(
        images.len(),
        2,
        "raw history must keep every original image"
    );
}

#[test]
fn verify_request_images_accepts_valid_digests_and_reports_a_mismatch() {
    let valid = png_part(10, 10);
    let mut digested = valid.clone();
    digested.content_id = Some(ContentId::from_bytes(&decoded(&valid)).to_string());
    let history = vec![user_message("valid", vec![digested])];
    verify_request_images(&history).unwrap();

    let mut corrupt = valid;
    corrupt.content_id = Some(ContentId::from_bytes(b"other").to_string());
    let history = vec![user_message("corrupt", vec![corrupt])];
    let error = verify_request_images(&history).unwrap_err();
    assert!(matches!(error, ImageBudgetError::DigestMismatch { .. }));
}

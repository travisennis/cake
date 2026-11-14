---
status: proposed
date: 2026-11-14
decision-makers: Travis Ennis
---

# Bound request images by count, pixels, and bytes

## Context and Problem Statement

Cake currently treats image acceptance and image request size as the same problem. `ReadImage` reads at most `read_image_max_bytes` (default five MiB) and rejects anything larger. It downscales an image only when its longest edge exceeds 2048 pixels. Those two rules bound one image, but not a request.

Conversation history keeps every image as inline base64, and every provider request carries every image in the projected history. A task that reads several screenshots sends all of them on every later turn, even after their exchanges are no longer relevant. Providers may reject the request for size or charge for pixels the task no longer needs. A provider that rejects one unreadable image rejects the whole request, which today triggers a recovery path that drops all images and retries without them.

The present source cap also couples the two concerns in the wrong direction. Raising it would blindly send larger images; keeping it rejects files that could be decoded and downscaled in memory. Session image bytes are already content-addressed and digest-verified when a session is loaded (ADR 061), but nothing verifies the digest again when a request is materialized.

This decision covers image payloads only. Tool-output text externalization is delivered by task 145a. Checkpoint selection and summary accounting belong to ADR 047. Session blob storage belongs to ADR 061.

## Decision Drivers

- Bound each provider request by image count, decoded pixels, and emitted image bytes without changing the provider wire shape.
- Accept a source image larger than the old five-MiB cap when it is within an explicit absolute source limit and can be downscaled in memory.
- Never modify a source file on disk.
- Preserve aspect ratio, never upscale, and keep an image that already fits byte-for-byte so animation, metadata, and the source encoder's choices survive.
- Reuse one deterministic selection and sizing path for ordinary requests, proactive compaction, and pressure recovery.
- Prefer the newest images and keep pinned images exactly.
- Fail before a provider request when pinned images alone cannot fit or when a digest is missing or corrupt.
- Keep session history and checkpoint retained records unchanged by request fitting. A request projection is not persistence.
- Keep the existing `image` and `infer` dependencies; add no codec crate.
- Keep diagnostics useful without logging payload bytes.

## Considered Options

- **Keep the current per-image edge cap and send every image (status quo).** Simple, but a long session can exceed provider limits, and history spends tokens on images the task has moved past.
- **Reject sources above five MiB and reject requests above a byte cap.** Bounds requests but rejects images that could be downscaled, and gives the model no way to continue from the remaining text.
- **Downscale every image to a small fixed size.** Bounds requests, but loses text legibility on screenshots and wastes quality on images that already fit.
- **Drop older images silently.** Bounds requests but leaves the model unable to tell that content is missing and may make it repeat the same read.
- **One request-time planner with count, pixel, and byte budgets, plus a larger source-read cap (chosen).** Separates source acceptance from request size, preserves images that fit, and makes omission explicit and deterministic.
- **Persist the downscaled image into the session or checkpoint.** Avoids re-encoding on every request but rewrites user data, loses the original pixels, and makes a request decision part of the session format. Rejected.
- **Use a new codec crate or an external image service.** Adds dependency, binary-size, network, privacy, and trust costs. The existing `image` crate already compiles the four supported codecs for `ReadImage`.

## Decision Outcome

Chosen: keep one per-image edge downscale at read time, raise the absolute source-read cap, and add one request-time image planner with count, pixel, and byte budgets.

### Source acceptance

`[limits].read_image_max_bytes` remains the source-read cap. Its compiled default rises from five MiB to 32 MiB. `"unlimited"` keeps its existing meaning: no configured cap. The read stops at the cap plus one byte, so a file larger than the cap is rejected with the existing model-visible error and the process does not buffer the whole file.

Before a full decode, `ReadImage` reads the image dimensions from the header and rejects more than 100,000,000 decoded pixels with an actionable error. The existing 512 MiB allocation limit on one decode remains a second bound. These bounds are compiled safety limits, not settings. The source file is never modified, truncated, or replaced.

### Request settings

A new optional `[images]` section carries the per-request budgets:

```
[images]
max_images = 8
max_bytes = 16777216
max_pixels = 24000000
```

`max_images` is the maximum number of emitted images. `max_bytes` is the maximum total emitted encoded image bytes. `max_pixels` is the maximum total decoded pixels. All three are positive integers with no `"unlimited"` sentinel; zero, negative, and non-integer values are rejected at load time. The defaults are eight images, 16 MiB, and 24,000,000 pixels.

The section is low-authority configuration: it can change cost and request size but cannot grant filesystem authority, execute code, or select a provider. It merges per key from global settings, then trusted project settings, then the selected profile's `[profiles.NAME.images]`, with the same precedence and unknown-key warning behavior as `[limits]`.

The byte budget counts the emitted encoded image bytes, which are the decoded length of the inline `data_base64` field. It does not count the data-URL prefix, JSON quoting, or base64 expansion; the wire representation is approximately four thirds of the budget. That choice keeps the setting comparable to source file sizes and to the blob store. The ADR records the approximation rather than pretending the value is an exact HTTP body limit.

The pixel budget counts decoded pixels for formats the planner can probe. The count budget applies to every emitted image, including an opaque format. An opaque format counts toward bytes and count but not toward pixels because its dimensions are not known without a decoder.

### Downscaling

The planner considers a supported image (`image/png`, `image/jpeg`, `image/gif`, `image/webp`) for downscaling when any of these is true:

- either edge exceeds 2048 pixels;
- its decoded pixel count exceeds `max_pixels`;
- its encoded byte count exceeds `max_bytes`.

A supported image inside all three limits is emitted byte-for-byte. A downscaled image preserves aspect ratio, is never upscaled, and is re-encoded in memory. PNG stays PNG; an image with transparency becomes PNG because JPEG cannot carry it; any other image becomes JPEG at quality 85. A downscaled animated GIF or WebP uses the first frame because the existing `image` decoder does. The planner starts at the dimensions allowed by the edge and pixel limits, encodes, and while the result exceeds `max_bytes` shrinks both dimensions by a factor of 0.85 and re-encodes, up to eight times. If the result still exceeds the byte budget, the image cannot fit; an unpinned image is omitted, and a pinned image is a hard error.

An unsupported or opaque media type passes through unchanged when it is within the count and byte budgets. It is never downscaled and does not count toward pixels. An unpinned opaque image over a budget is omitted; a pinned one is a hard error.

### Pins and selection

The planner receives a pin flag for every image slot. For an ordinary request, the images on the most recent user message that carries images are pinned. They are the current input. Other images, including images from tool results in the same invocation, are unpinned and may be omitted oldest first. For proactive compaction, every image in a retained record with a pin reason is pinned, every image in a complete exchange that contains such a record is pinned, and every `unconsumed_image` record is pinned. Pressure recovery uses the ordinary request rule.

The planner first handles per-image downscaling, then applies the count, pixel, and byte budgets to the emitted set. When a total still exceeds a budget, it omits the oldest unpinned image, one at a time, and keeps the newest images. Selection is deterministic by source order. If the remaining pinned images alone exceed any budget, the result is an explicit no-fit error before the provider call; the planner never drops a pinned image to make a request fit.

Every emitted set that drops an image appends one explicit marker to the owning message text or tool output, in source order:

```
[image omitted: <reason>; <media_type>, <width>x<height>, <bytes> bytes]
```

The reason is `count`, `pixels`, or `bytes`; when the dimensions are unknown the marker omits them and keeps the media type and byte count. The marker is text, not an empty replacement for a content part. The planner does not drop function calls, outputs, reasoning, or any non-image content.

### Request-only projection

Ordinary requests plan against the owned request projection `ConversationState::request_context()`. The planner mutates that projection only when it must downscale or omit; when every image fits, the borrowed projection is passed through. Raw conversation history and session records keep their original images. A later request plans again from the original bytes.

Compaction calls the same planner in read-only mode on its retained tail. The plan's emitted count, bytes, and pixels feed the existing estimate instead of the raw image count. The checkpoint retains original `content_id` references and never stores a downscaled image. If pinned images alone cannot fit, preparation returns `PrepareOutcome::NoFit` rather than inventing a smaller pinned image. Proactive compaction (034g) and pressure recovery (034h) call the same planner; there is no second image-sizing path.

The provider wire shape is unchanged. A kept or downscaled image becomes the same inline data URL each backend already builds.

### Digest verification

Before downscaling, the planner decodes each image's base64 and, when `content_id` is present, verifies that the decoded bytes hash to that digest. A mismatch or invalid base64 is a hard error naming the image's position, media type, and digest, never the payload. A missing blob is already a hydration error before the agent starts; request planning never substitutes empty bytes. Legacy inline images without a digest are verified by decoding, and the planner records the digest of the bytes it emits.

The request-only copy of a downscaled image carries the digest of the emitted bytes. Each backend calls the planner's verification function again immediately before serializing its request, so a corrupt or accidentally altered request projection fails before any HTTP request. This second check is a re-verification of the emitted bytes; it does not re-downscale.

### Diagnostics

The planner returns a report with the original and emitted image count, bytes, and pixels, plus one entry per downscaled or omitted image: slot position, media type, original and emitted dimensions, original and emitted bytes, and the omission reason. It logs the report under the `cake` tracing target at debug level and never logs payload bytes. `cake debug config` gains an `images` snapshot with the resolved policy and the source-read cap, in text and JSON forms. No session record or provider field is added.

### Consequences

- Good: a request is bounded by image count, pixels, and emitted bytes while the provider wire shape and session format stay unchanged.
- Good: a source image above the old five-MiB cap is accepted when it is within the absolute source limit and can be downscaled in memory; the source file is untouched.
- Good: one planner and one codec path serve ordinary requests, compaction, and pressure recovery, so the same inputs produce the same emitted projection.
- Good: pinned and current-input images are never silently dropped.
- Good: the existing `image` dependency already compiles all four codecs, so binary size and the dependency graph do not change.
- Bad: raising the source-read default changes behavior. A file that was rejected at five MiB is now read and downscaled; a user who relied on the old rejection must lower `read_image_max_bytes`.
- Bad: older unpinned images are omitted from requests. They remain in the session, and the marker tells the model they existed.
- Bad: every request decodes and hashes its images, and downscaling costs CPU. The budgets bound the work, but the cost is real for large sessions.
- Bad: the byte budget is an emitted-image-byte budget, not an exact HTTP body limit; base64 and JSON add roughly one third.
- Bad: a downscaled animated GIF or WebP becomes a single frame. An image that already fits stays byte-for-byte and keeps its animation.
- Bad: a request whose pinned images alone exceed a budget fails instead of silently dropping user input or compacting more history. That failure is the intended explicit behavior.

## More Information

- Task 147, `Enforce per-request image budgets and downscale in memory`.
- ADR 047, core-native compaction, which owns checkpoint selection and the eight-MiB ceiling.
- ADR 061, content-addressed session images, which owns blob storage and hydration digest verification.
- Task 145 and 145a, which delivered image and large-tool-output content references.
- `docs/dependencies.md`, for the existing `image` pin and the no-new-codec decision.

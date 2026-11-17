---
status: accepted
date: 2026-11-14
decision-makers: Travis Ennis
---

# Admit request images without rewriting sent history

## Context and Problem Statement

ADR 062 bounded provider requests by image count, pixels, and emitted bytes with one request-time planner. That planner re-plans the whole request projection on every turn and, when a budget is exceeded, omits the oldest unpinned image. The omitted image is replaced by a marker inside a message the provider has already accepted.

Cake's architecture states the opposite requirement: within one invocation, each provider request's message sequence is an exact prefix-extension of the previous request's, with no sanctioned exception, because provider prompt caches depend on it. Prefix caches match complete cached prefix units, so a rewrite early in the request can invalidate reuse for every later text token, not only the image. A request that emits fewer image tokens can therefore cost more and take longer than the one it replaced. The effect is conditional on the budget being full and a new image arriving, and its dollar magnitude is unmeasured, but the mechanism is structural rather than incidental.

The pin model is also ambiguous. An ordinary request pins only the most recent user message that carries images, so every tool-result image is unpinned, including one the model has never seen. Compaction pins retained records with a pin reason, every exchange containing one, and every `unconsumed_image` record. Two callers of one planner therefore disagree about the same history, and proactive compaction (034g) would inherit the disagreement. Pinning the most recent user message with images also skips newer text-only user messages, so an earlier task's attachments can stay pinned across later tasks.

The rest of ADR 062 is sound and stays: in-memory codec reuse, the raised source-read cap with a decompression-bomb guard, deterministic in-memory downscaling, digest verification at the request boundary, and one sizing path.

## Decision Drivers

- Preserve the tail-only prefix invariant on the request path without an exception.
- Keep every request bounded by image count, decoded pixels, and emitted bytes.
- Never fail an ordinary request because of image budgets.
- Decide each image once; make retirement of an admitted image a checkpoint-level operation.
- Keep checkpoint preparation free to choose the best retained set, because it commits a new boundary.
- Keep the provider wire shape, session format, and `[images]` settings unchanged.
- Bound planning work, and never re-encode an image whose fate a previous request already fixed.

## Considered Options

- **Keep ADR 062's oldest-first eviction (status quo).** Bounds requests, but rewrites history the provider already accepted and loses cache reuse after the rewritten item.
- **Remove request planning and send every image (status quo ante).** Preserves the prefix, but leaves request size unbounded and is why 147 exists.
- **Trigger compaction whenever the image budget fills.** The correct long-term answer, but it needs checkpoint machinery (034g) that does not exist yet, and it would fire mid-tool-loop.
- **Admission-only request planning (chosen).** Each image is sized and either admitted or omitted when it enters the projection; nothing already emitted changes.
- **Persist the admitted projection into the session or checkpoint.** Avoids re-planning, but makes a request decision part of the session format and rewrites user data. Rejected.

## Decision Outcome

Chosen: keep the ADR 062 settings, source acceptance, downscaling, digest verification, and diagnostics, and replace only its selection and pin policy with admission-only selection on the request path.

### Admission rule

The planner decides each image in source order, using only the images that precede it:

- A supported image inside the per-image limits is emitted byte-for-byte.
- A supported image over an edge, pixel, or byte limit is downscaled in memory with the existing deterministic algorithm, then counted at its emitted size.
- An image that would push the running count, pixel, or byte total over a budget is omitted and replaced by the existing explicit marker. Every earlier decision stands.

There is one pass and no revisit, so an image's fate is a pure function of the images before it and the resolved policy. Planning a history and planning that history plus a suffix therefore reproduce the same projection for the original items; no admitted state has to be recorded anywhere. The planner never fails a request for budget reasons: an image that cannot be admitted is omitted, and decode or digest failures remain hard errors.

When the budget is full, later images in the invocation are omitted until a checkpoint retires earlier ones. That is the deliberate tradeoff: admission is monotone, and the marker is the model's only recovery path within the invocation.

### Omission marker

The marker keeps the ADR 062 shape and reason vocabulary:

```
[image omitted: <reason>; <media_type>, <width>x<height>, <bytes> bytes]
```

Request-side markers add that the request image budget is exhausted and that the model should crop or downscale the source, or continue from text, because a later request will not free the space. There is no separate checkpoint marker shape after the planner removal.

### Checkpoint preparation

Compaction commits a new boundary, so it keeps ADR 062's pin rule: every image in a retained record with a pin reason is pinned, every image in a complete exchange that contains such a record is pinned, and every `unconsumed_image` record is pinned. Checkpoint preparation still plans read-only and stores original content references; it never persists a downscaled image.

Implementation note (2026-11-17): automatic compaction now validates every candidate through the same `plan_admitted_images` projection the next request uses. It retires older complete exchanges rather than running ADR 062's checkpoint-only eviction loop, so `PinnedExceedsBudget` was removed. An aggregate pinned conflict is a candidate no fit; `PinnedCannotFit` remains for a pinned image that cannot be reduced below a per-image limit.

### Pressure recovery

Reactive recovery (034h) may not omit admitted images in place, for the same reason the request path may not. It either compacts at a checkpoint or appends guidance to the conversation.

The existing provider image-rejection fallback is unchanged. It runs only after a provider refused the request, so there is no accepted prefix to preserve, and it drops every image and retries once with an explanatory note.

### Unchanged from ADR 062

- `[images] max_images`, `max_bytes`, and `max_pixels` with defaults eight images, 16 MiB, and 24,000,000 pixels; positive integers only; low-authority precedence and unknown-key warnings as before.
- `[limits].read_image_max_bytes` default 32 MiB, header dimension probe, 100,000,000 decoded-pixel rejection, and 512 MiB per-decode allocation guard.
- PNG, JPEG, GIF, and WebP in-memory downscaling; never upscale; keep an image that already fits byte-for-byte; transparency forces PNG; animated input uses the first frame.
- Digest verification before downscaling and again immediately before serialization; a mismatch or missing blob is a hard error that never logs payload bytes.
- The request-only projection, the per-request plan report under the `cake` tracing target at debug level, and the `images` snapshot in `cake debug config`.

## Consequences

- Good: an ordinary request is an exact prefix-extension of the previous one, so the architecture invariant and provider prefix caching hold without an exception.
- Good: the same image is never re-encoded for a different outcome; re-planning reproduces earlier decisions exactly.
- Good: the request path cannot fail because of image budgets, and no caller has to supply pins for it.
- Good: 034g gets one boundary to retire at instead of reconciling two pin models.
- Bad: once the budget fills, new images are omitted for the rest of the invocation. A long task that needs fresh screenshots after eight admitted images must compact or start a new session.
- Bad: every request still decodes and hashes its admitted images, and a downscaled image is re-encoded on every request. The repository never persists a downscaled projection.
- Bad: an image that is individually downscaled and then cannot be admitted was encoded for nothing.
- Bad: without 034g, nothing retires an admitted image, so the cap is effectively permanent per invocation.
- Bad: a downscaled animated GIF or WebP becomes a single frame, and the byte budget remains an emitted-image budget rather than an exact HTTP body limit.

## More Information

- Task 182, `Admit request images without rewriting sent history`.
- Task 147 and ADR 062, `Bound request images by count, pixels, and bytes`, which this ADR supersedes.
- ADR 047, core-native compaction, which owns checkpoint selection and the eight-MiB ceiling.
- ADR 061, content-addressed session images, which owns blob storage and hydration digest verification.
- Tasks 145 and 145a, which delivered image and large-tool-output content references.
- `ARCHITECTURE.md`, for the tail-only prefix invariant.

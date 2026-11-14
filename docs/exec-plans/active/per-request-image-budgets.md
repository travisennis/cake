## Enforce per-request image budgets and in-memory downscaling

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

After this change, a Cake session can read a large screenshot or photo, keep it in the session, and still send a bounded provider request. `ReadImage` accepts a source file up to an absolute source-read cap, decodes it in memory, and downscales it without touching the file on disk. At each provider request, one deterministic planner verifies every image digest, downscales an image that individually exceeds the request limits, and omits the oldest unpinned images when the total exceeds the count, pixel, or byte budget. Every omitted image leaves an explicit text marker in the message or tool output, so the model can tell that content existed and was left out.

A user can see the result without reading code. Reading a seven-MiB, 4000-by-3000 PNG succeeds instead of failing with the old five-MiB error; the result reports its emitted dimensions, and `shasum` before and after the run is identical. In a session with ten images, a request contains the newest images within the configured budgets and one marker for each older image that was dropped. If the current user input's images alone exceed a budget, the run fails before any HTTP request with an error naming the budget and the pinned images. A corrupt or missing image digest also fails before any provider request. `cake debug config --json` reports the resolved image policy.

The feature is an additive configuration surface with one deliberate default change: `[limits].read_image_max_bytes` rises from five MiB to 32 MiB because the source cap no longer has to double as a request cap. Provider wire shapes stay the same: each emitted image is still an inline data URL. Session records and checkpoint retained records keep their original image references; fitting is a request projection, not a persistence rewrite. The governing decision is ADR 062, `docs/adr/062-bound-request-images-by-count-pixels-and-bytes.md` (proposed at the time of writing), with ADR 047 for compaction and ADR 061 for session blobs.

## Progress

- [x] (2026-11-14) Read task 147, task 145/145a delivery evidence, ADR 061, ADR 047, and the current `ReadImage` code. Confirmed `145` and `145a` are on `master` at `13b79ff`.
- [x] (2026-11-14) Found that `ReadImage` already downscales an image whose longest edge exceeds 2048 pixels; the missing behavior is the aggregate request budget, not a per-image resize.
- [x] (2026-11-14) Drafted ADR 062 as proposed; it is the gate before code changes.
- [x] (2026-11-14) Drafted this ExecPlan.
- [ ] Trav accepts ADR 062, or records the changes that must be made first.
- [ ] Milestone 1: add `[images]` settings, defaults, profile overlay, resolution, diagnostics, and the `cake init` template.
- [ ] Milestone 1: prove defaults, per-key precedence, profile precedence, zero and negative rejection, unknown-key warnings, and generated template content.
- [ ] Milestone 2: extract the codec helpers into `src/clients/image_budget.rs`, raise the source-read default to 32 MiB, and add the 100-megapixel probe before full decode.
- [ ] Milestone 2: prove PNG, JPEG, GIF, and WebP downscaling; aspect ratio; no upscale; byte-identical source files; over-limit source rejection; decompression-bomb rejection; and bounded memory.
- [ ] Milestone 3: implement the image planner, digest verification, deterministic selection, omission markers, and the normal-request adapter.
- [ ] Milestone 3: wire the planner into `Agent::complete_turn_in_mode` and the two backend request builders; add `Agent::with_image_policy`.
- [ ] Milestone 3: prove count, pixel, and byte limits; oldest-first ordering; newest retention; pinned no-fit; digest mismatch; marker text; request-body shape; and no mutation of raw history.
- [ ] Milestone 4: call the same planner from compaction preparation in read-only mode, feed emitted counts into the estimate, return `NoFit` for pinned overflow, and keep retained records unchanged.
- [ ] Milestone 4: prove an ordinary request and compaction produce the same plan for the same inputs, and pressure recovery can reuse the planner without a second sizing path.
- [ ] Milestone 5: update `docs/configuration.md`, `docs/integrations.md`, the `ReadImage` description, affected snapshots, and the ADR/plan records.
- [ ] Milestone 5: run the task verification commands, the full gate, and the session-metrics check; record the outcomes and move this plan to `docs/exec-plans/completed/`.
- [ ] `ahm task complete 147` before the final implementing commit; commit and push `master`.

## Surprises & Discoveries

- Observation: `ReadImage` already downscales images above 2048 pixels and documents that behavior in `docs/configuration.md`. Evidence: `src/clients/tools/read_image.rs` defines `MAX_IMAGE_DIMENSION = 2048`, `prepare_image`, `downscale`, and `encode_downscaled`, and the existing test `read_image_downscales_an_image_over_the_cap`. The task's phrase "never resizes one" is stale; the real gap is that the source read cap is also the only byte bound and every historical image is sent.
- Observation: `ConversationState::request_context()` returns `Cow<'_, [ConversationItem]>` and borrows the raw history when no checkpoint is active. Evidence: `src/clients/agent_state.rs`. A request planner should return the borrowed value when every image fits, so an image-free or under-budget session pays no copy.
- Observation: session hydration already verifies every `content_id` before the agent starts. Evidence: `BlobStore::hydrate_session` and `hydrate_images` in `src/config/blob_store.rs`. The new request-boundary verification is a second check of the request projection, not a replacement for hydration.
- Observation: `ContentId::parse` and `ContentId::matches` are public and already used by persistence code. Evidence: `src/config/blob_store.rs`. The planner can verify a hydrated image's digest without adding a new hashing abstraction.
- Observation: compaction preparation already creates a provider-neutral `Vec<ConversationItem>` for estimation with `source_items`, and `RetainedRecord::to_conversation_item` exists. Evidence: `src/clients/compaction.rs` lines around `prepare`, `source_items`, and `estimate_full_request`. The same planner can plan on those items without mutating retained records.
- Observation: compaction already segments exchanges and exposes `Exchange::contains_pin`; pinned exchanges are the right source for compaction image pins. Evidence: `segment_exchanges`, `validate_exchanges`, and `select_exchanges` in `src/clients/compaction.rs`.
- Observation: several existing image tests use placeholder base64 such as `AAAA` or a truncated PNG header. Serialization-only tests can keep those bytes, but any test that passes through the planner or `verify_request_images` needs a valid tiny PNG or an explicitly opaque media type; otherwise the dimension probe fails before the behavior under test. Evidence: `src/clients/agent/image_recovery_tests.rs` uses `data_base64: "AAAA"`, and `src/clients/compaction_tests.rs` builds synthetic image parts. Milestone 3 updates only the fixtures that enter the planner.
- Observation: the `image` crate is already a direct dependency with the four needed codec features, and `ReadImage` uses it. Evidence: `Cargo.toml` line 55. There is no new dependency or binary-size decision beyond reusing that code.
- Observation: `[images]` is low-authority configuration like `[limits]`; it can change cost and request size but cannot grant paths, run code, or select a provider. Evidence: the low-authority list in `docs/configuration.md` and `Settings::without_authority` in `src/config/settings.rs`.

## Decision Log

- Decision: reuse the existing `image` crate and the existing `infer` detector; add no codec dependency. Rationale: both are already compiled into the binary for `ReadImage`, so the planner adds code but not a new supply-chain or binary-size surface. Date/Author: 2026-11-14, Travis Ennis (ADR 062).
- Decision: keep `[limits].read_image_max_bytes` as the absolute source-read cap and raise its compiled default from five MiB to 32 MiB. Rationale: source acceptance and request size are separate concerns; a larger source can be downscaled, while the request budgets bound what is sent. A user who wants the old rejection lowers the key. Date/Author: 2026-11-14, Travis Ennis (ADR 062).
- Decision: add `[images]` with `max_images = 8`, `max_bytes = 16777216`, and `max_pixels = 24000000`. The keys are positive integers with no `"unlimited"` value. Rationale: sending an unbounded number of images is the behavior this task exists to stop; an explicit numeric policy is easier to reason about and test. Date/Author: 2026-11-14, Travis Ennis (ADR 062).
- Decision: count emitted encoded image bytes, not base64 characters or an exact HTTP body size. Rationale: the value stays comparable to source files and blob sizes; the roughly four-thirds base64 expansion and JSON overhead are documented rather than guessed into the setting. Date/Author: 2026-11-14, Travis Ennis (ADR 062).
- Decision: pin the images on the most recent user message that carries images for an ordinary request. Other tool-result images are unpinned and may be dropped oldest first. Rationale: the current input must never be silently discarded, while a long task may read many screenshots whose older results are no longer useful. Date/Author: 2026-11-14, Travis Ennis (ADR 062).
- Decision: for compaction, pin every image in a record with a pin reason and every image in a complete exchange that contains such a record, including `unconsumed_image`. If pinned images alone exceed a budget, return `PrepareOutcome::NoFit`. Rationale: checkpoint selection already treats a pinned exchange as indivisible; image fitting must not quietly contradict that. Date/Author: 2026-11-14, Travis Ennis (ADR 062).
- Decision: keep the request projection read-only with respect to persistence. A normal request mutates only the owned request context; compaction plans for measurement and leaves retained records with their original `content_id` references. Rationale: downscaling is a request decision, and losing original pixels in a checkpoint would make recovery and later tasks worse. Date/Author: 2026-11-14, Travis Ennis (ADR 062).
- Decision: verify a source image's digest before downscaling and verify the emitted request image again immediately before the backend serializes it. The request-only copy carries the digest of its emitted bytes. Rationale: the first check catches a corrupt stored payload; the second catches an altered or truncated request projection before any HTTP request. Date/Author: 2026-11-14, Travis Ennis (ADR 062).
- Decision: an unsupported or opaque image format passes through when it fits the count and byte budgets, counts toward bytes and count but not pixels, and is omitted only if unpinned and over a budget. Rationale: toolbox or MCP tools may return a media type the four codecs do not cover; hard-failing those existing sessions would be a regression, while silently sending it after a pixel calculation would be dishonest. Date/Author: 2026-11-14, Travis Ennis (ADR 062).
- Decision: use one marker shape, `[image omitted: <reason>; <media_type>, <width>x<height>, <bytes> bytes]`, appended to the owning text in source order. Rationale: the model needs to know that something was omitted and why; a stable shape is testable and readable in a transcript. Date/Author: 2026-11-14, Travis Ennis (ADR 062).
- Decision: expose the resolved policy in `cake debug config` and log per-request decisions through `tracing` under the `cake` target, never payload bytes. Rationale: the task requires diagnostics without adding a session record or a provider field. Date/Author: 2026-11-14, Travis Ennis.

## Outcomes & Retrospective

Not started. The plan is waiting for ADR 062 acceptance. On completion, record the delivered settings, the source-cap change, the planner API, the exact verification commands and counts, any skipped platform check, and whether pressure recovery's 034h integration needed a planner change. Move this plan to `docs/exec-plans/completed/` only when the whole task is complete.

## Context and Orientation

Cake is a Rust 2024 binary-only assistant. A conversation is a sequence of provider-neutral `ConversationItem` values. Images ride in `ImagePart` values attached to a user or tool-result item. `ReadImage` is a built-in tool that reads one file, detects its media type, optionally downscales it, and returns an `ImagePart` containing base64 bytes. Session persistence externalizes each image to a content-addressed blob and stores a SHA-256 `content_id`; loading a session verifies the digest and restores the base64 bytes. A compaction checkpoint is a self-contained projection that replaces retired history with a summary and a retained tail.

The current request path is the important boundary. `Agent::send` loops over provider turns. `Agent::complete_turn_in_mode` calls `ConversationState::request_context()`, which returns the raw history when there is no checkpoint or a projected history when a checkpoint is active. It passes that slice to `AgentRunner::complete_turn`. The runner sends it through `Backend::send_request`; each backend's `build_request_json` turns images into inline data URLs and serializes the request. There is no request-level image budget today. A provider image rejection is handled separately by `complete_turn_with_image_recovery`, which drops all images and retries once; this plan keeps that backstop unchanged.

Compaction preparation is the other path that needs the same sizing rule. `prepare` in `src/clients/compaction.rs` takes a `PreparationInput`, validates pins and image-consumption evidence, segments whole exchanges, and estimates token counts with the content-class estimator. It returns `NoOp`, `NoFit`, or a `PreparedCompaction` whose retained tail may contain image references. It does not make a provider call. Tasks 034g (automatic compaction) and 034h (pressure recovery) will call this seam, so this plan must expose one planner rather than adding a second image path.

Terms used in this plan:

- **Source image** means the bytes in a file that `ReadImage` opens. The source file is never modified.
- **Encoded image bytes** means the bytes of one image after it has been encoded (PNG, JPEG, GIF, or WebP) and before base64. This is the unit of `max_bytes`.
- **Emitted image** means an image that survives planning and is put into the provider request. It may be the original bytes or a downscaled re-encode.
- **Pinned image** means an image the planner may not omit. It may still be downscaled.
- **Request projection** means the `Vec<ConversationItem>` a request would send. It is not the raw session history and is not persisted.
- **Planner** means the single function that verifies, sizes, selects, and reports image decisions. It is neither a backend-specific transform nor a persistence step.
- **Opaque image** means an image whose media type is outside the four supported codecs, so the planner can count bytes but cannot probe dimensions or downscale it.

The key files are:

- `src/clients/tools/read_image.rs` --- `ReadImage` arguments, path validation, source read, media detection, current 2048-pixel downscale, and the existing codec helpers.
- `src/types/conversation.rs` --- `ImagePart`, `ConversationItem`, and `ImagePart::data_url`.
- `src/clients/agent_state.rs` --- `ConversationState::request_context` and the checkpoint projection.
- `src/clients/agent/agent_loop.rs` --- `send`, `complete_turn_in_mode`, and the existing image-rejection recovery.
- `src/clients/agent_runner.rs` --- `complete_turn` and the request boundary.
- `src/clients/chat_completions.rs` and `src/clients/responses.rs` --- `build_request_json`, `build_messages`, and `build_input`.
- `src/clients/compaction.rs` --- `PreparationInput`, `prepare`, `apply_pins`, `segment_exchanges`, and `estimate_items_tokens`.
- `src/config/settings.rs` --- `Settings`, `ProfileSettings`, `LimitsSettingsOverlay`, `LoadedSettings`, and the existing merge and unknown-key machinery.
- `src/config/blob_store.rs` --- `ContentId`, `BlobStore::hydrate_session`, and the digest helpers.
- `src/cli/debug/config_report.rs` --- the text and JSON `cake debug config` report.
- `src/cli/init.rs` --- the commented `settings.toml` template.
- `src/clients/tools/read-image-description.txt` --- the model-visible tool description.
- `docs/configuration.md` and `docs/integrations.md` --- the settings contract and the diagnostics contract.

The current defaults are five MiB for `read_image_max_bytes`, 2048 pixels for the longest edge, JPEG quality 85 for a downscaled non-PNG image, and 512 MiB for one decoded image. The session image store and its digest verification are already delivered by task 145 and ADR 061. The checkpoint ceiling is eight MiB serialized with a 256 KiB summary reservation; image references do not count their payload bytes toward that ceiling.

## Plan of Work

### Milestone 1: settings and policy surface

Add the user-facing policy before any behavior depends on it. In `src/config/settings.rs`, add `ImagesSettingsOverlay` with three optional `PositiveLimit` fields: `max_images`, `max_bytes`, and `max_pixels`. Add `images: Option<ImagesSettingsOverlay>` to `Settings` and to `ProfileSettings`. Add the accumulated fields and a `merge_images` method to `SettingsAccumulator`, called from both the top-level merge and `apply_profile_overlay`, with the same per-key precedence as `merge_limits`. Resolve the three fields into `ResolvedImagePolicy` with compiled defaults and place it on `LoadedSettings` as `images`.

Define `ResolvedImagePolicy` in `src/config/settings.rs` or in a small `src/config/images.rs` module, with `max_images: usize`, `max_bytes: usize`, and `max_pixels: u64`, a `defaults()` constructor, and the compiled constants `DEFAULT_IMAGE_MAX_IMAGES: u32 = 8`, `DEFAULT_IMAGE_MAX_BYTES: u32 = 16 * 1024 * 1024`, and `DEFAULT_IMAGE_MAX_PIXELS: u32 = 24_000_000`. `PositiveLimit` already rejects zero; the missing-key path uses the compiled default. There is no `"unlimited"` sentinel, so a user cannot accidentally disable the bound.

Preserve `[images]` through `Settings::without_authority`; it is low-authority. Add it to the known-key round-trip path by deriving `Serialize`/`Deserialize` normally, so an unknown `[images]` key warns through the existing `collect_unknown_keys` path with a source file and section. Add the section to `docs/configuration.md` and to the generated `settings.toml` template in `src/cli/init.rs`, with comments that distinguish the per-request budgets from the source-read cap.

Add the resolved policy to `cake debug config`. In `src/cli/debug/config_report.rs`, add an `ImagesSnapshot` to the JSON report and an `images:` line to the text report. Include `max_images`, `max_bytes`, `max_pixels`, `read_image_max_bytes`, and the compiled `max_edge` (2048) so a reader can see the complete effective policy. Update the debug-config tests and snapshots that compare the report.

The proof for this milestone is that a settings file with a global `[images]` value, a project `[images]` value, and a `[profiles.review.images]` value resolves to the most specific value per key while an absent key inherits the lower layer; a zero or negative value fails to load; an unknown key warns once with the file and section; `cake init` contains the commented section; and `cake debug config --json` has the resolved values.

### Milestone 2: codec extraction and source acceptance

Create `src/clients/image_budget.rs`. Move the codec-only pieces out of `src/clients/tools/read_image.rs` into it: the supported media-type allowlist, `image_dimensions`, `decode_image`, `decode_limits`, `MAX_DECODED_IMAGE_BYTES`, `MAX_IMAGE_DIMENSION`, `encode_downscaled`, `has_transparency`, and the JPEG quality constant. Add a `PreparedImage` type that records the emitted bytes, emitted media type, emitted dimensions, source dimensions, and whether the bytes changed. Keep the tool-specific path validation, argument parsing, and model-visible error wording in `read_image.rs`; it calls the shared codec functions.

Add a compiled `MAX_SOURCE_PIXELS: u64 = 100_000_000` bound. `ReadImage` probes dimensions from the header before a full decode and returns an actionable error if `u64::from(width) * u64::from(height)` exceeds it; compute the product in `u64` so a hostile header cannot overflow. Continue to use `image::Limits` with `max_alloc = 512 MiB` as the second bound. The source read still stops at `read_image_max_bytes + 1` bytes and the file is never written.

Raise `DEFAULT_READ_IMAGE_MAX_BYTES` from five MiB to 32 MiB and update every place that names the default: `docs/configuration.md`, `src/clients/tools/read-image-description.txt`, the `cake init` comment, the debug-config text, and any snapshot. Keep `"unlimited"` behavior as it is documented today.

The proof for this milestone is a set of fixtures that generate a PNG, JPEG, GIF, and WebP, each larger than the 2048 edge or the byte test cap. `ReadImage` returns one image part, the emitted dimensions are bounded, the aspect ratio is preserved within one pixel, and the source file's SHA-256 is unchanged. A tiny 10x10 image remains 10x10. A file above `read_image_max_bytes` is rejected without reading the whole file. A crafted image header whose dimensions exceed `MAX_SOURCE_PIXELS` is rejected before a full decode. A decode failure returns the existing model-visible error rather than a panic.

### Milestone 3: the planner, normal requests, and omission markers

Implement the planner in `src/clients/image_budget.rs`. The public types are:

```rust
pub struct ImageSlot<'a> {
    pub ordinal: usize,
    pub owner: ImageOwner,
    pub image: &'a ImagePart,
    pub pinned: bool,
}

pub struct ImageOwner {
    pub item: usize,
    pub image: usize,
}

pub enum ImagePlanEntry {
    Keep,
    Replace(ImagePart),
    Omit { reason: ImageOmissionReason, marker: String },
}

pub enum ImageOmissionReason {
    Count,
    Pixels,
    Bytes,
    CannotFit,
}

pub struct ImageBudgetPlan {
    pub entries: Vec<ImagePlanEntry>,
    pub report: ImageBudgetReport,
}

pub fn plan_image_budget(
    slots: &[ImageSlot<'_>],
    policy: &ResolvedImagePolicy,
) -> Result<ImageBudgetPlan, ImageBudgetError>
```

`ImageBudgetReport` carries original and emitted image counts, decoded pixels, and encoded bytes, plus per-image change and omission records. `ImageBudgetError` carries the slot, media type, and a reason; it never contains payload bytes. Errors are `InvalidBase64`, `DigestMismatch`, `DimensionProbe`, `DecompressionBomb`, `PinnedExceedsBudget`, and `PinnedCannotFit`.

The planner processes slots in source order. For each slot it decodes the base64, verifies `content_id` when present, probes dimensions for a supported media type, and rejects a source above `MAX_SOURCE_PIXELS`. It decides whether the image is already within the edge, byte, and pixel limits. A fitting image becomes `Keep`. A supported over-limit image is decoded and re-encoded with the shared codec: preserve aspect ratio, never upscale, start at the edge and pixel limits, and shrink by 0.85 up to eight times while the encoded bytes exceed `max_bytes`. A successful result becomes `Replace` and its request-only `ImagePart` carries the digest of the emitted bytes. A supported image that cannot be made to fit, and an opaque image over a budget, is `Omit` when unpinned and an error when pinned.

After per-image handling, sum emitted count, pixels, and bytes. While any total exceeds its budget, find the oldest unpinned slot still marked `Keep` or `Replace` and change it to `Omit` with the reason that triggered the drop. If no unpinned slot remains and a total is still over, return `PinnedExceedsBudget` with the pinned totals and the budget. The selection is stable: source order determines "oldest", and a dropped image is never reconsidered.

Generate the marker text in the planner and carry it in the plan, so both adapters render the same string. Append markers in source order to the owning item's text. The exact shape is `[image omitted: <reason>; <media_type>, <width>x<height>, <bytes> bytes]`; when dimensions are unknown, omit them and keep the media type and byte count. A message whose images all disappear still exists with its original text plus markers; a tool output likewise.

Add the adapters in the same module. `prune_request` accepts `Cow<'a, [ConversationItem]>`, collects slots, pins the images on the most recent user message that carries images, calls the planner, and returns the borrowed slice when every entry is `Keep`; otherwise it clones the slice and applies `Replace` and `Omit` by item and image index. `apply_plan_to_history` is the shared mutation helper. `verify_request_images(history: &[ConversationItem])` decodes each image, verifies `content_id` when present, and returns an error on a mismatch; each backend calls it before building messages. The normal request path never writes to `ConversationState`, the session writer, or a checkpoint.

Add `image_policy: ResolvedImagePolicy` to `Agent`, defaulting to `ResolvedImagePolicy::defaults()` and set by a new `Agent::with_image_policy(policy)`. Call it from `src/main.rs` next to `with_limits(&loaded.limits)`, and from the session-factory paths that build an agent. In `complete_turn_in_mode`, replace `let history = self.conversation.request_context();` with a call that prunes the owned request projection before passing it to `runner.complete_turn`. The pruning happens after the tool definitions and output constraint are known and before the first retry attempt, so every attempt of a turn uses the same projected history.

In `src/clients/chat_completions.rs` and `src/clients/responses.rs`, call `verify_request_images(history)` at the start of `build_request_json`, before `build_messages` or `build_input`. This is the second digest check. It must not re-run the planner or change the image budget. Update the existing image-recovery and request-shape fixtures that pass through the planner to use a valid tiny PNG; leave serialization-only fixtures synthetic.

The proof for this milestone is a planner unit suite covering count-only, pixel-only, and byte-only drops; a mixture that drops the oldest unpinned images in order and keeps the newest; a downscale that preserves aspect ratio and never upscales; a PNG that stays PNG; a transparent image that stays PNG; a JPEG/GIF/WebP that becomes JPEG; a digest mismatch; an invalid base64 payload; an opaque image that passes through under budget; a pinned set that cannot fit; and a marker for every dropped image. An agent test mounts a mock provider and asserts the request body contains only the expected data URLs and the marker text, and a zero-request mock proves a pinned over-budget or corrupt image fails before the provider call. A test also proves the raw conversation history still has every original image after a pruned request.

### Milestone 4: compaction and pressure recovery reuse

Add `image_policy: &'a ResolvedImagePolicy` to `PreparationInput` in `src/clients/compaction.rs`. After `apply_pins` and `segment_exchanges`, build slots from the retained records with `pinned = true` for every record in an exchange where `Exchange::contains_pin` is true, and for every record whose `pin` is `Some`. Call `plan_image_budget` on those slots. Do not apply `Replace` or `Omit` to the retained records; use the report's emitted count, pixels, and bytes in the estimate and in the `PreparedCompaction` diagnostics. Add the plan or its report to `PreparedCompaction` so a later caller can inspect the decision without recomputing it.

The estimate must count the planner's emitted images, not the raw images. The existing `estimate_items_tokens` gives each image a flat 1,500-token allowance, so a `Replace` keeps that allowance and an `Omit` removes it; the marker text is estimated as text. A source item list for estimation is a request-only clone, and the retained records in the checkpoint remain the originals. When the planner returns `PinnedExceedsBudget`, convert it to `PrepareOutcome::NoFit` with a reason that names the budget and the pinned image count. When the planner returns a per-image `PinnedCannotFit`, do the same.

For 034h, expose the planner as the only image-fitting entry point. Pressure recovery may call `plan_image_budget` on the current request projection to decide whether omitting unpinned images can make the request fit, but it must not add a second downscale path. This plan does not implement the pressure-recovery trigger; it only leaves the seam and proves the planner is callable without a session mutation.

The proof for this milestone is a compaction test that builds the same history twice, once as `ConversationItem`s for an ordinary request and once as `RetainedRecord`s with the same pins, and asserts the two plans have the same emitted images, markers, and totals. A second test proves a pinned over-budget retained image returns `NoFit` and leaves every retained `ImagePart` byte-identical. A third test proves an unpinned over-budget image is omitted from the estimate while the retained record still carries its original `content_id`. The existing replay tests must still pass because no session record shape changes.

### Milestone 5: documentation, diagnostics, and gates

Update `docs/configuration.md` with a new Images section after Limits. Document the three keys, their defaults, their positive-only values, the per-key global/project/profile precedence, the low-authority status, the source-read default change, the byte-unit definition and base64 approximation, the per-image 2048 edge, the four supported codecs, the transparent-PNG and JPEG rules, the first-frame behavior for a downscaled animated GIF or WebP, and the pinned no-fit behavior. Update the `read_image_max_bytes` row and paragraph to 32 MiB and explain that it is a source-read cap, not a request cap.

Update `docs/integrations.md` for the additive `cake debug config` images object and the unchanged provider request shape. State that image omission is a request projection and does not rewrite session records or checkpoints. Update `src/clients/tools/read-image-description.txt` with the new source cap, the in-memory downscale rule, and a note that a request-level budget may omit older images with markers. Update the tool-description and prompt snapshots if the text changes.

Update the ADR 062 status only if Trav accepts it; do not mark a proposed ADR accepted on your own. Record the acceptance in the task and plan comments. Update this plan's `Outcomes & Retrospective` with the delivered behavior, exact command results, and any skipped platform check. Move the plan to `docs/exec-plans/completed/` when the whole plan is done. Complete task 147 with `ahm task complete 147` before the final implementing commit, then commit and push `master` with a Conventional Commit scoped from `cog.toml` (likely `feat(clients)` or `feat(agent)`).

The proof for this milestone is `just docs-check`, `just check`, `just session-metrics-check`, and the focused commands listed below. No paid model calls are made at any point.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake/cake`.

Read the task, ADR, and current code before editing:

```bash
ahm task show 147
sed -n '1,240p' docs/adr/062-bound-request-images-by-count-pixels-and-bytes.md
sed -n '1,320p' src/clients/tools/read_image.rs
sed -n '1,260p' src/config/settings.rs
```

Milestone 1 focused checks:

```bash
cargo test --bin cake settings
cargo test --bin cake init
cargo test --bin cake debug_config
just docs-check
```

Milestone 2 focused checks:

```bash
cargo test --bin cake read_image
```

Milestone 3 focused checks:

```bash
cargo test --bin cake image_budget
cargo test --bin cake clients::agent
cargo test --test replay
```

Milestone 4 focused checks:

```bash
cargo test --bin cake compaction
cargo test --bin cake clients::agent
```

Milestone 5 full checks:

```bash
just docs-fmt
just docs-check
just session-metrics-check
just check
```

`cargo test` accepts one test-name filter per invocation; run the commands separately rather than combining filters. If a focused filter selects no tests, use `cargo test --bin cake -- --list` to confirm the module path before changing the filter.

## Validation and Acceptance

The behavior is accepted when a human can observe all of these:

- `ReadImage` on a seven-MiB, 4000-by-3000 PNG succeeds, reports emitted dimensions at or below 2048 on the longest edge, and leaves the source file byte-identical. A file above the configured `read_image_max_bytes` still fails with the existing actionable error. A crafted header above 100 megapixels fails before a full decode.
- A conversation with ten unpinned images and `max_images = 8` sends exactly the newest eight. Each of the two dropped images leaves one marker in its owning text, and the provider body contains no data URL for them. The raw session history still contains all ten images after the request.
- A conversation whose total pixels or bytes exceed the budgets drops the oldest unpinned images until it fits. The ordering is deterministic across repeated runs and across the Chat Completions and Responses backends.
- An image on the most recent user message is never dropped. If pinned images alone exceed a budget, the run fails before the mock provider receives a request, and the error names the budget and the pinned image count.
- An image with a mismatched `content_id`, invalid base64, or a missing blob fails before the provider request. The error names the slot, media type, and digest where available and never prints payload bytes.
- The same input history produces the same emitted image set and markers whether it is planned as an ordinary request or as a compaction retained tail. A pinned over-budget tail returns `NoFit`, and the retained records are unchanged.
- `cake debug config --json` contains an `images` object with the resolved `max_images`, `max_bytes`, `max_pixels`, source-read cap, and edge cap. The text report prints the same values.
- All existing session-record, stream-json, replay, and provider-wire snapshots are unchanged except for documented additive diagnostics and tool-description text.

Run the exact focused checks above, then `just docs-check`, `just session-metrics-check`, and `just check`. Record the pass counts and any skipped platform check in the task's Acceptance Notes and in this plan's Outcomes & Retrospective.

## Idempotence and Recovery

Settings edits, documentation edits, and test fixtures are safe to repeat. The planner is pure over its inputs and writes no files. A failed downscale or digest check leaves the session, the request projection, and the source file unchanged. Re-running a test or a command after a failure starts from the same on-disk state.

Do not rewrite a session file, a checkpoint, or a blob while debugging a request-budget failure. Add a temporary test fixture instead. If a test fixture writes a large image, use `tempfile::TempDir` so a failed run cleans up on drop. If `just check` fails in the complexity gate, split the planner or adapter helpers rather than editing `ci/cargo-crap-baseline.json`; that baseline is only for deliberately grandfathered code.

If ADR 062 is rejected or amended, update the ADR first, then revise this plan's Decision Log, Plan of Work, and Progress before changing code. Do not implement a conflicting behavior and document it afterward.

## Artifacts and Notes

The proposed ADR is `docs/adr/062-bound-request-images-by-count-pixels-and-bytes.md`. The governing prior decisions are `docs/adr/047-*` for compaction and `docs/adr/061-*` for content-addressed images. Task 147 depends on task 145; the current `master` base is `13b79ff`, which includes the 145a tool-output references.

The current source constants to preserve are `MAX_IMAGE_DIMENSION = 2048`, `MAX_DECODED_IMAGE_BYTES = 512 * 1024 * 1024`, `DOWNSCALE_JPEG_QUALITY = 85`, and `SUPPORTED_IMAGE_MEDIA_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"]`. The new settings defaults are eight images, 16 MiB, and 24,000,000 pixels. The new source default is 32 MiB; the new source-pixel safety bound is 100,000,000 pixels.

The existing image-rejection recovery in `src/clients/agent/agent_loop.rs` remains the backstop for a provider that still rejects an image after planning. This plan does not remove or change it; it only reduces how often it is needed.

No paid model calls are required. The tests use generated fixtures and local mock providers.

## Interfaces and Dependencies

The new module is `crate::clients::image_budget`, declared in `src/clients/mod.rs`. Its stable public surface is:

```rust
pub struct ImageSlot<'a>;
pub struct ImageOwner;
pub enum ImagePlanEntry;
pub enum ImageOmissionReason;
pub struct ImageBudgetPlan;
pub struct ImageBudgetReport;
pub enum ImageBudgetError;
pub fn plan_image_budget(slots: &[ImageSlot<'_>], policy: &ResolvedImagePolicy)
    -> Result<ImageBudgetPlan, ImageBudgetError>;
pub fn prune_request<'a>(history: Cow<'a, [ConversationItem]>, policy: &ResolvedImagePolicy)
    -> Result<Cow<'a, [ConversationItem]>, ImageBudgetError>;
pub fn verify_request_images(history: &[ConversationItem]) -> anyhow::Result<()>;
```

The settings surface is `ResolvedImagePolicy { max_images, max_bytes, max_pixels }` in `src/config/settings.rs`, with `DEFAULT_IMAGE_MAX_IMAGES`, `DEFAULT_IMAGE_MAX_BYTES`, and `DEFAULT_IMAGE_MAX_PIXELS`. `LoadedSettings` gains `images: ResolvedImagePolicy`. `Agent` gains `image_policy` and `Agent::with_image_policy`. `PreparationInput` gains `image_policy: &ResolvedImagePolicy`. `PreparedCompaction` gains a budget report or plan so callers can inspect the decision.

No Cargo dependency is added. `image = { version = "0.25.10", default-features = false, features = ["png", "jpeg", "gif", "webp"] }` and `infer = { version = "0.22.0", default-features = false }` already exist in `Cargo.toml`. The planner uses `base64`, `sha2` through `ContentId`, and the existing codec helpers.

Compatibility surfaces: `[images]` is additive; `read_image_max_bytes` changes its compiled default and its documented meaning from a request cap to a source-read cap; the omission marker is model-visible request text; `cake debug config` gains an additive `images` object; session records, checkpoint records, stream-json, replay, and provider wire shapes do not change. A user who wants the old five-MiB behavior sets `read_image_max_bytes = 5242880`.

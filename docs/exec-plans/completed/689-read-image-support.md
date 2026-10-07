## Read images with a ReadImage tool and send them to both API backends

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds. Every revision must keep this file self-contained: a reader with only the current working tree and this file should be able to finish the work.

## Purpose / Big Picture

Cake is a headless coding agent that talks to OpenAI-compatible Chat Completions and Responses endpoints. Today it is blind to images. A model that wants to look at a screenshot, a design mock, a diagram, or a chart has no tool that returns pixels, and even if one existed the conversation model is text-only, so an image could not be carried to the provider. The Read tool explicitly rejects binary files and tells the model to use `file`, `hexdump`, or `xxd` instead.

After this change, a user running a vision-capable model can point cake at an image file inside the workspace and get a description or an answer grounded in the image. Concretely, the model can call a new `ReadImage` tool with a path, and cake will send those pixels to the provider in whatever shape that provider's API requires. What someone can do afterward that they cannot do now:

- Ask cake to review a screenshot or diagram in the repository and receive an answer that references what is actually in the image.
- Use image input with both supported backends: the Chat Completions API and the Responses API.
- Resume or fork a session that used an image and have cake replay the same image to the provider, so the conversation stays valid.

How to verify it works, at the highest level:

- With a vision-capable model configured, run cake in a directory containing a PNG and ask it to describe the image without naming the file. The model calls `ReadImage`, and its final answer correctly describes the picture.
- Run the focused backend tests for both translations and the new tool; they pass.
- Run the full local gate; it passes.

## Progress

- [x] (2026-10-06) Investigated the current conversation model, both backends, tool plumbing, and persistence; opened issue #689 and created branch `feat/read-image-support`.
- [x] (2026-10-06) Milestone 1: proved the native image-bearing `function_call_output` on the configured gateway's Responses endpoint and on the Codex backend (red and blue fixtures both answered correctly on each) and recorded the finding in the Decision Log and Artifacts and Notes. The Codex probe first returned HTTP 401 `token_expired` and passed once the Codex CLI refreshed `~/.codex/auth.json`.
- [x] (2026-10-06) Milestone 2: added `ImagePart` and the additive `images` fields to the conversation items and session records, added the `ReadImage` tool with its `read_image_max_bytes` `[limits]` cap, threaded tool images through the agent loop into history and persistence, and added the Read binary-rejection image hint. `cargo test` passes (1673 tests plus the integration binaries); the snapshot changes are the new tool's entries in the prompt and request snapshots.
- [x] (2026-10-07) Milestone 3: translated images for the Chat Completions backend. `ChatMessage.content` is now an untagged `ChatContent` enum (a bare string, or a parts array), and a `FunctionCallOutput` carrying images emits the usual text `tool` message followed by a synthetic user message whose content array holds a text part naming the tool result plus one `image_url` part per image. User messages that carry images use the same parts array. Text-only histories serialize byte-identically, so no existing request snapshot changed; the new `build_messages_tool_image_parts` snapshot pins the image shape and matches probe 5.
- [x] (2026-10-07) Milestone 4: translated images for the Responses backend. `ResponsesMessageContent` is now an untagged enum (a text block or an `input_image` block), `FunctionCallOutput.output` is an untagged enum (a bare string or a parts array), and the `From<&ConversationItem>` conversion emits an `input_text` block plus one `input_image` block per image for both tool outputs and user messages. Text-only inputs serialize byte-identically, so no existing snapshot changed; two new snapshots (`to_api_input_function_call_output_with_images`, `to_api_input_user_message_with_images`) pin the native shape from probe 1. `cargo test` passes (1688 tests).
- [x] (2026-10-07) Milestone 5: gated `ReadImage` on a per-model `supports_images` flag (default false). Added the flag to `ModelConfig`/`ModelDefinition`, gave `ToolEntry` a `requires_images()` capability and `ToolRegistry::retain_model_supported_tools`, applied it in `Agent::new` and `format_tool_list_section`, and moved prompt construction after model resolution (`RunInputs::initial_messages`) so the tool list can reflect the model. `ReadImage` joined `BUILTIN_TOOL_NAMES` and the description filter, whose line match now requires a whole word so a `Read` reference no longer deletes `ReadImage`'s own guidance. Documented `supports_images` in `docs/configuration.md`; `cargo test` passes (1697 tests).
- [x] (2026-10-07) Milestone 6: ran the full local gate (`just check`: fmt, strict Clippy, the 1699-test all-features suite plus the integration binaries, import/dependency/module/glossary lints, the complexity ratchet, and the Python fixture suites) and the focused tool, backend, prompt, and session tests; confirmed no existing snapshot moved (no `.snap.new` remained); added a live end-to-end describe run and a resume replay to Artifacts and Notes; filled in Outcomes & Retrospective and moved this plan to `completed/`. The `docs/integrations.md` and `docs/configuration.md` updates landed with their milestones.

## Surprises & Discoveries

- Observation: cake is text-only at every layer, not just in the tool layer. Evidence: `ConversationItem::Message { content: String }` and `ConversationItem::FunctionCallOutput { output: String }` in `src/types/conversation.rs`; `ChatMessage.content: Option<Cow<str>>` in `src/clients/chat_types.rs`; `ResponsesApiInputItem::FunctionCallOutput { output: &str }` in `src/clients/responses_types.rs`; `ToolResult.output: String` in `src/clients/tools/mod.rs`; `MessageData.content: String` and `FunctionCallOutputData.output: String` in `src/types/session.rs`. Any image feature must touch all of these.
- Observation: the Chat Completions specification restricts a tool-role message's content to text parts, but at least one OpenAI-compatible gateway is more permissive. Evidence: the configured gateway (`https://opencode.ai/zen/go/v1/chat/completions`, model `deepseek-v4.1-flash`) accepted a `tool` message whose content array carried an `image_url` part and answered correctly about the image, while the tool result is emitted as a plain string tool message in `src/clients/chat_completions.rs` (`push_function_call_output`). The synthetic-user-message design in Milestone 3 is therefore the conservative shape that also works on strict providers, not the only shape that works at all. Probes 3 and 4 in Artifacts and Notes.
- Observation: the Codex credential store can hold an expired access token and cake never refreshes it, so codex-backed models fail until the Codex CLI rewrites `~/.codex/auth.json`. Evidence: a hand-built Responses request to `https://chatgpt.com/backend-api/codex/responses` returned HTTP 401 `token_expired` while `codex login status` still reported `Logged in using ChatGPT`, and `src/auth.rs` only reads the file (`ChatGptAuth::load`) with no refresh path. The same request returned HTTP 200 after a Codex CLI refresh (probe 6).
- Observation: the dependencies needed for image work already exist. Evidence: `Cargo.toml` lists `base64` and `infer`; `infer` is already used for MIME detection in `src/clients/tools/bash.rs` (`detect_mime_type`).
- Observation: `infer` is compiled with `default-features = false`, which drops its `std` feature, so `infer::get_from_path` does not exist in this build. Evidence: `Cargo.toml` pins `infer = { version = "0.22.0", default-features = false }` and the compiler rejected `infer::get_from_path`; the Read image hint and the `ReadImage` tool therefore read bytes themselves and call the buffer-based `infer::get`.
- Observation: adding a field to `ConversationItem::Message` and `::FunctionCallOutput` is mechanically broad: it required an explicit `images` value at about 160 struct-literal sites, nearly all in backend and agent tests. Evidence: the Milestone 2 diff touches 25 files, of which 20 are tests or test fixtures.
- Observation: real image files do not reach Read's null-byte rejection; they fail earlier as invalid UTF-8, so the `ReadImage` hint belongs on the read-error path rather than only in `binary_file_error`. Evidence: a file whose first bytes are the PNG signature (`0x89`) fails `decode_utf8` in `read_line_bounded` and surfaces as `Failed to read file '...': stream did not contain valid UTF-8`, while the null-byte message needs decodable text (a GIF's ASCII signature plus a null byte). The hint is therefore appended in `execute_read` via `append_image_hint`, covering both paths; two tests pin the UTF-8 and null-byte cases.

## Decision Log

- Decision: Add a dedicated `ReadImage` tool instead of overloading `Read`. Rationale: tool schemas, descriptions, and snapshots are a compatibility surface; overloading `Read` would change its contract, make `start_line`/`end_line` meaningless for images, and force a binary-versus-image branch into a text tool. A new tool registers read-only and replay-safe, gates cleanly, and carries its own description and snapshots. Date/Author: 2026-10-06, cake.
- Decision: Represent images as an additive `images: Vec<ImagePart>` field on the message and tool-output domain types, leaving the existing `content`/`output` strings unchanged. Rationale: this keeps the persisted text contract, the stream-json `message` and `function_call_output` shapes, and every existing text test intact; older readers ignore the new field and newer readers tolerate its absence. Date/Author: 2026-10-06, cake.
- Decision: Persist image bytes inline as base64 in the session JSONL, bounded by a per-image size cap, rather than in a content-addressed blob store. Rationale: cake's session files are append-only, self-contained, and must replay identically on resume and fork; inline data keeps that guarantee with no new store to create, back up, or garbage-collect. A blob store can be a later optimization if session size becomes a problem. Date/Author: 2026-10-06, cake.
- Decision: Prefer the Responses API's native image-bearing `function_call_output`; fall back to the synthetic-user-message shape everywhere if provider verification fails. Rationale: the native path keeps history clean and avoids a synthetic message, but its availability varies across OpenAI-compatible gateways. Milestone 1 settles this before other work depends on it. Date/Author: 2026-10-06, cake.
- Decision: Gate `ReadImage` on a new per-model capability setting, default off. Rationale: sending an image to a text-only model produces a provider error; an opt-in capability avoids that class of failure and keeps the default tool set unchanged. Date/Author: 2026-10-06, cake.
- Decision: Adopt the native Responses form (`function_call_output.output` as an array of a text block plus an `input_image` block) and keep the synthetic-user-message design for Chat Completions. Rationale: the configured gateway's Responses endpoint (`https://opencode.ai/zen/go/v1/responses`, model `gpt-6-luna`) accepted the native array and answered correctly about both the red and the blue 16x16 PNG, which meets the promotion criterion; the same gateway also accepted an image part inside a `tool` message, but the Chat Completions specification allows only text parts there, so the synthetic user message remains the shape that works on strict providers too (the planned tool-message-then-user-message sequence was verified on the gateway in probe 5). The Codex backend confirmed the same result once its stored token was refreshed: both fixtures answered correctly, and its requirement that `store` be `false` matches what `build_request_json` already sends for that backend. Date/Author:
  2026-10-06, cake.
- Decision: Bound `ReadImage` with a new `read_image_max_bytes` `[limits]` key, default 5 MiB. Rationale: image bytes are sent inline as base64, so an unbounded file would inflate both memory and the request body; 5 MiB covers a full-size screenshot while keeping the encoded body under about 7 MB, and `"unlimited"` stays available for a user who wants no cap. An oversized file is rejected before it is read and before any provider request. Date/Author: 2026-10-06, cake.
- Decision: Register `ReadImage` unconditionally in Milestone 2 and add the model-capability gate in Milestone 5. Rationale: the gate needs the new `ModelConfig` field that Milestone 5 introduces, and keeping the milestones separate keeps each change reviewable. Consequence: the prompt and request snapshots list the tool between the two milestones and are updated again when the gate lands. Date/Author: 2026-10-06, cake.
- Decision: Model a request message's content as an untagged `ChatContent` enum (a bare string or a parts array) rather than adding a second parts field or letting the tool message carry images. Rationale: the untagged enum makes an image-free message serialize exactly as before, so no existing request snapshot moves and the text path keeps its old bytes; a second field would need a serde alias to reuse the `content` key; and the Chat Completions specification restricts a tool-role message to text parts, so the image has to ride in a separate user message regardless of the content type. Date/Author: 2026-10-07, cake.
- Decision: Reuse the tool result's own text as the synthetic user message's leading text part instead of a fixed label. Rationale: the output string already names the file, its size, and its media type, so the model can tie the image to the call without cake inventing new model-visible vocabulary, and the message keeps a text part next to its images. Date/Author: 2026-10-07, cake.
- Decision: Model a Responses content block and a `function_call_output.output` value as untagged enums (a text block/string, or a parts array) rather than adding a second image field. Rationale: the untagged enums make a text-only item serialize exactly as before, so no existing Responses request snapshot moves and the text path keeps its old bytes; the native image-bearing `function_call_output` array is the shape Milestone 1 verified, and a user message reuses the same block list for its own images. Date/Author: 2026-10-07, cake.
- Decision: Express the image gate as a tool capability (`ToolEntry::requires_images`) filtered by `ToolRegistry::retain_model_supported_tools`, not a name check on `ReadImage`. Rationale: registration is already the single place that declares what a tool is (#277), and both the agent registry and the prompt tool list consume the same declaration, so they cannot drift. Date/Author: 2026-10-07, cake.
- Decision: Build the initial prompt messages after the model is resolved by moving the call into `RunInputs::initial_messages`, instead of resolving the model twice or hoisting resolution above the run-mode dispatch. Rationale: the tool list now depends on the model, but `build_client_and_session` built messages before any run mode resolved its model (resume and fork need the stored session to know theirs); building them in the run-mode step that already holds the resolved config keeps one resolution and one load per run. Date/Author: 2026-10-07, cake.
- Decision: Match built-in tool references by whole word in `filter_builtin_description`. Rationale: adding `ReadImage` to the description filter would otherwise let a `Read` reference delete `ReadImage`'s own lines (they name both tools), and the whole-word rule also stops a `Bash` reference from deleting `BashSession` lines. Date/Author: 2026-10-07, cake.
- Decision: Keep `supports_images` default-false and set the shared agent-test model fixture to image-capable. Rationale: the product default must not advertise `ReadImage` to a text-only model, while the existing tool-selection tests assert the full tool list and are about sandbox and allowlist behavior, not capability; the gate has its own focused tests at the registry, prompt, and agent layers. Date/Author: 2026-10-07, cake.

## Outcomes & Retrospective

Cake can now see images. A vision-capable model calls `ReadImage` on a PNG, JPEG, GIF, or WebP file inside the workspace and receives the pixels: the tool returns one inline `ImagePart`, the agent loop carries it into history and persistence, and each backend translates it into the shape its API requires. Chat Completions sends the text `tool` message followed by a synthetic `user` message whose content array holds an `image_url` part; Responses sends the native `function_call_output` whose `output` is an `input_text` block followed by one `input_image` block. `ImagePart` and the additive `images` fields on the conversation items and session records keep every existing text record, request, and stream-json shape byte-identical, so old sessions still load and replay and no pre-existing request snapshot moved. `ReadImage` is gated on the per-model `supports_images` flag (default `false`), so a text-only model never sees it; the real registry and the generated prompt tool list share one
`requires_images` capability, and `--tools` narrows the set but cannot re-add a gated tool.

Verification: `just check` passed --- fmt, strict Clippy, the 1699-test all-features suite plus the integration binaries, the import/dependency/module/glossary lints, the per-function complexity ratchet, and the Python fixture suites --- as did the focused tool, backend, prompt, and session tests. No `.snap.new` remained, and the additive fields left every pre-existing snapshot unchanged; three new snapshots (`build_messages_tool_image_parts`, `to_api_input_function_call_output_with_images`, `to_api_input_user_message_with_images`) pin the two wire shapes against probes 1 and 5. A live end-to-end run on the Codex Responses backend called `ReadImage` and answered with the image's actual colors, its persisted record carried the `images` array, and a resume of that session replayed the stored image and answered again without a tool call. Platform sandbox tests and the Coverage job remain CI-owned, as usual.

Residuals and lessons. The Chat Completions specification restricts a tool-role message to text parts, so the image must ride in a synthetic user message even though the configured gateway accepted it inline (probe 4 versus 5); that divergence is why the two backends do not converge on one shape. `infer` is built with `default-features = false` here, so its `std` feature is absent and MIME detection reads the file's bytes and calls `infer::get` instead of `infer::get_from_path`. Images are persisted inline as base64 bounded only by `read_image_max_bytes` (default 5 MiB); if session size becomes a problem, a content-addressed blob store is the natural later optimization, and downscaling large images would add an image-processing dependency that is deliberately out of scope here.

## Context and Orientation

Cake is a Rust binary with one internal conversation representation, translated at each backend's edge. The internal representation lives in `src/types/conversation.rs` as the enum `ConversationItem`, with variants for messages, function calls, function call outputs, and reasoning. A "function call" is a tool invocation the model requested; a "function call output" is that tool's result. `src/clients/chat_completions.rs` and `src/clients/responses.rs` translate `ConversationItem` values into the two wire formats, using DTOs in `src/clients/chat_types.rs` and `src/clients/responses_types.rs`. Request and response shapes are pinned by insta snapshots under `src/clients/snapshots/`.

Tools are defined in `src/clients/tools/`, one module per tool, with the model-facing description in a sibling `*-description.txt`. A tool returns a `ToolResult` (in `src/clients/tools/mod.rs`) whose `output` is a string, plus telemetry. The registry that pairs a tool definition with its executor is built in `default_tool_registry()` in `src/clients/tools/mod.rs`. A `ToolEntry` declares capabilities: `read_safe` keeps a tool available under the read-only sandbox, and `replay_safe` marks it safe to re-execute after an interrupted run. `Read` is both read-safe and replay-safe.

The system prompt's "Available tools" section is generated from the registry by `format_tool_list_section()` in `src/clients/tools/mod.rs`, called from `src/prompts/mod.rs`. That function builds its own registry from `default_tool_registry()`, so a capability-gated tool must be threaded into both the real registry and this prompt list.

Model configuration is resolved into `ResolvedModelConfig` (`src/config/model.rs`), which carries `ModelConfig` fields such as `api_type`. `Agent::new(config, ...)` in `src/clients/agent.rs` has the resolved model config available and builds the real tool registry, which is where a capability gate can add the tool.

Persisted sessions are append-only JSONL. The on-disk record types are in `src/types/session.rs` (`MessageData`, `FunctionCallOutputData`, and the `SessionRecord`/`StreamRecord` enums), with conversions to and from `ConversationItem` in the same file. Stream-json output is emitted for the current invocation; replay re-emits a session as stream-json.

Terms used in this plan, defined plainly:

- Content part: one piece of a message's content, either text or an image. The APIs accept a message whose content is an array of such parts.
- Data URL: a string of the form `data:image/png;base64,<data>` that embeds a file's bytes inline. Both APIs accept an image as a data URL, so no upload step is needed.
- Tool message (Chat Completions): the message whose role is `tool`, carrying a tool result and a `tool_call_id` that pairs it with the requesting call. Its content is text only.
- Synthetic user message: an extra user-role message cake inserts into a request, not present verbatim in the stored history, to carry something the API will not accept elsewhere. Here it carries image parts immediately after a tool result.
- MIME type: the standard label for a file's format, for example `image/png`. The `infer` crate guesses it from the file's leading bytes.
- Capability gate: a configuration switch that decides whether a tool is registered at all, so the model never sees a tool it cannot use.

## Plan of Work

The work proceeds in the order below. Milestone 1 is a prototype that removes the biggest unknown first. Milestones 2 through 4 build the feature from the inside out: the domain model and tool, then each backend. Milestone 5 makes the feature opt-in and visible in prompts. Milestone 6 is the final gate.

First, in `src/types/conversation.rs`, add an image part type and give the message and function-call-output variants an additive `images` field. Second, in `src/types/session.rs`, add the matching optional field to `MessageData` and `FunctionCallOutputData` and update the two conversion directions; the serializer must skip the field when empty so existing snapshots and text sessions are unchanged. Third, in `src/clients/tools/mod.rs`, add `images` to `ToolResult` and default it empty at every construction site. Fourth, add `src/clients/tools/read_image.rs` and `read-image-description.txt`, reusing the path validation and the `infer` MIME helper, and register the tool in `default_tool_registry()` as read-safe and replay-safe. Fifth, update the `Read` binary-rejection message to suggest `ReadImage` when the rejected file is an image.

For the Chat Completions backend, `src/clients/chat_types.rs` needs a content type that serializes either as a plain string (today's behavior) or as an array of parts. `src/clients/chat_completions.rs` currently pushes one tool-role message per function-call output; it must additionally push a user-role message with image parts after an output that carries images. Because the builder borrows from history, the image parts must be borrowable or cloned; choose the simpler of the two and record the choice if it changes.

For the Responses backend, `src/clients/responses_types.rs` needs an image-bearing variant of the message content block and of the function-call-output `output` field, and `src/clients/responses.rs` (`From<&ConversationItem>`) must produce them. If Milestone 1 shows the native tool-output image is unsupported, use the same synthetic-user-message approach as Chat Completions instead, and say so in the Decision Log.

For the capability gate, add a boolean field to `ModelConfig` in `src/config/model.rs` with a serde default of false, register `ReadImage` in `Agent::new` only when it is true (or when the name is explicitly enabled), thread the same flag into `format_tool_list_section()` and its `src/prompts/mod.rs` caller, and document it in `docs/configuration.md`.

Finally, add focused tests and refresh snapshots, update `docs/integrations.md` for the additive record fields, and run the full gate.

## Milestones

### Milestone 1: Prove the Responses API's image-in-tool-output behavior (prototyping)

This milestone answers one question with evidence before any production code depends on it: when a Responses API request contains a `function_call_output` whose `output` is an array of content parts including an `input_image`, does the target provider accept it and answer about the image, or does it reject the request? The answer selects the Responses translation strategy and may simplify the Chat Completions strategy too.

Work: build a minimal request by hand against each configured Responses endpoint and one Chat Completions endpoint. Use a tiny PNG (a solid-color 1x1 or a few-pixel image) encoded as a data URL so the request is small. Send the request with `curl` using the same base URL, path, and auth header cake uses, or write a throwaway Rust test that builds the request through the existing client and prints the response. Do not commit the throwaway; record the outcome in the Decision Log and Artifacts and Notes.

Result at the end: a recorded finding that says, for each backend, whether tool-output images work, are silently dropped, or are rejected, with the exact error text for any rejection.

Proof: the recorded response, including any error code and message, plus a one-paragraph recommendation for the Responses strategy. Promotion criterion: if the provider accepts and reasons about the image, adopt the native `function_call_output` array for Responses. Discard criterion: if it rejects or drops the image, use the synthetic-user-message path for Responses as well, and note that this makes the two backends converge.

Outcome (2026-10-06): the promotion criterion was met on both Responses endpoints --- the configured gateway and the Codex backend each returned HTTP 200 and named the fixture color correctly for the red and the blue PNG. The Codex probe first returned HTTP 401 `token_expired` (a stale local token, not a shape rejection) and passed after a Codex CLI refresh. The full record is in Artifacts and Notes.

### Milestone 2: Domain model, `ReadImage` tool, and additive persistence

At the end of this milestone, a `ReadImage` call produces image bytes in the internal representation, a session can store and reload them, and every existing text test still passes. No provider request carries an image yet.

Work: in `src/types/conversation.rs`, add `ImagePart { media_type: String, data_base64: String }` and an `images: Vec<ImagePart>` field to `ConversationItem::Message` and `ConversationItem::FunctionCallOutput`, defaulting empty. In `src/types/session.rs`, add the optional `images` field to `MessageData` and `FunctionCallOutputData`, skip it when empty, and update `from_conversation_item_with_replay` and `to_conversation_item`. In `src/clients/tools/mod.rs`, add `images: Vec<ImagePart>` to `ToolResult` and set it empty wherever a `ToolResult` is constructed (including the bash, edit, write, and toolbox paths, and test fixtures). Create `src/clients/tools/read_image.rs` and `src/clients/tools/read-image-description.txt`: parse a required `path`, validate it with the existing cwd/allowed-dir validation, read the file under a size cap, detect the MIME type with `infer`, reject non-image types with a message that points at `Read`, and return a `ToolResult` whose `output` is a short
human-readable summary and whose `images` holds one sized-capped `ImagePart`. Register it in `default_tool_registry()` with `read_safe()` and `replay_safe()`. Add a `[tools]` limit for the maximum image bytes in `src/config/settings.rs`, mirroring the existing read limits, and use it in the tool. Update `src/clients/tools/read.rs` so its binary-file rejection message mentions `ReadImage` when the file looks like an image.

Result at the end: `ReadImage` is registered and executable, returns an image part, and a session round-trips that image part through JSON.

Proof: a unit test calls the ReadImage executor on a fixture PNG and asserts the returned `images` has one entry with the expected media type, and a session-record round-trip test writes a `MessageData` and a `FunctionCallOutputData` with images, serializes them, reloads them, and asserts equality. The existing session snapshot tests and `cargo test clients::tools` still pass. The text-only fixtures show no `images` field in their JSON.

Outcome (2026-10-06): implemented. `ImagePart { media_type, data_base64 }` lives in `src/types/conversation.rs` and is exported from `crate::types`; `ConversationItem::Message` and `::FunctionCallOutput` each carry an `images` field with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`, and `MessageData`/`FunctionCallOutputData` carry the same optional field, so old records load and text records serialize unchanged. `ToolResult.images` carries images out of a tool, and `ConversationState::push_tool_output` plus `ToolRunResult` carry them into history and persistence. `src/clients/tools/read_image.rs` and `read-image-description.txt` add the `ReadImage` tool, registered read-safe and replay-safe; `read_image_max_bytes` (default 5 MiB) bounds it under `[limits]`. `src/clients/tools/read.rs` appends a `ReadImage` hint to a rejected file's read error when its leading bytes look like an image, covering both the invalid-UTF-8 and null-byte rejection paths. Registration is
unconditional here; the model-capability gate arrives in Milestone 5, so the prompt and request snapshots now list `ReadImage` and will list it only for image-capable models once the gate lands.

### Milestone 3: Chat Completions translation

At the end of this milestone, a Chat Completions request built from a history containing a tool image contains the tool message followed by a user message whose content array carries the image, and the tool call/output pairing remains valid.

Work: extend `src/clients/chat_types.rs` with a content type that serializes as either a plain string or an array of parts, where an image part is `{"type":"image_url","image_url":{"url":"data:<media_type>;base64,<data>"}}` and a text part is `{"type":"text","text":"..."}`. Extend `ChatMessage` so its `content` uses this type. In `src/clients/chat_completions.rs`, keep emitting the text tool message as today, and when a `FunctionCallOutput` carries images, emit a following user-role message with an array content: a short text part naming the tool result, then one image part per image. Keep the plain-string serialization for every message that has no images so existing snapshots do not change.

Result at the end: a synthetic user message carries the image after the tool result.

Proof: a focused test builds a history with a function call, a function-call output carrying an image, and asserts the produced messages are `[assistant(tool_calls), tool(text), user(content array with image_url)]`; a snapshot pins the exact JSON. An unmodified text history produces byte-identical output to today.

Outcome (2026-10-07): implemented. `src/clients/chat_types.rs` gains `ChatContent<'a>` (`Text(Cow<str>)` or `Parts(Vec<ChatContentPart>)`, `#[serde(untagged)]` so a text message still serializes as a bare string), `ChatContentPart` (`Text` and `ImageUrl` variants), and `ChatImageUrl`; `ImagePart::data_url()` in `src/types/conversation.rs` builds the `data:<media_type>;base64,<data>` URL both backends need. `ChatMessageBuilder::push_function_call_output` now takes the output's images, keeps the text tool message, and appends the synthetic user message; `push_message` carries a user message's own images the same way. Because the untagged enum serializes text-only messages exactly as before, every existing request snapshot is unchanged and only the new `build_messages_tool_image_parts` snapshot was added. Two tests cover the tool-image sequence and a user message with two images, and a third pins the JSON.

### Milestone 4: Responses translation

At the end of this milestone, a Responses request built from a history containing a tool image carries the image, using the strategy chosen in Milestone 1, and reasoning and message inputs are unchanged for text.

Milestone 1 selected the native path (see the Decision Log); the synthetic fallback below is retained only if a target provider later rejects the native form.

Work: if the native path was chosen, extend `src/clients/responses_types.rs` so `ResponsesMessageContent` can be a text block or an image block (`{"type":"input_image","image_url":"data:..."}`) and so `ResponsesApiInputItem::FunctionCallOutput`'s `output` can be a string or an array of those blocks; update the `From<&ConversationItem>` conversion in `src/clients/responses.rs` accordingly. If the fallback path was chosen, keep `function_call_output` text-only and insert a synthetic user `message` item after the output, mirroring Milestone 3. Also support user messages that carry images directly, since a future CLI attachment path would reuse them.

Result at the end: the Responses request carries the image in the shape the provider accepts.

Proof: focused tests and snapshots for a tool image and for a user message with an image, asserting the exact JSON, plus the existing `to_api_input_*` snapshots unchanged for text. If the native path is used, a live call against the target provider returns an answer that describes the image.

Outcome (2026-10-07): implemented. `src/clients/responses_types.rs` gains `ResponsesMessageContent` as an untagged enum over `ResponsesTextContent` (the former struct) and `ResponsesImageContent` (`{"type":"input_image","image_url":"data:..."}`), plus `ResponsesFunctionCallOutput` as an untagged enum over a borrowed string and a parts array; `ResponsesApiInputItem::FunctionCallOutput.output` now uses the latter, and helpers `ResponsesMessageContent::text`/`::image` build blocks. The `From<&ConversationItem>` conversion in `src/clients/responses.rs` emits a text block followed by one image block per image for a message, and a bare string for a text-only tool output or a `[input_text, input_image, ...]` array when the output carries images. Because both enums are untagged, every existing `to_api_input_*` request snapshot is unchanged and only the two new snapshots were added; the shape matches probe 1. `ImagePart::data_url()` supplies the inline URL, so no new dependency is needed.
Focused tests: `cargo test clients::responses` (97 tests), plus the full `cargo test` suite (1688 tests). Live check: a cake run on the Codex Responses backend (`--model codex-luna-none`) called `ReadImage` and answered about the image, and the persisted `function_call_output` record carries the `images` array (session `b706bb8f-16dd-4a5f-b5ac-1b4ac462abc2`; see Artifacts and Notes).

### Milestone 5: Capability gate, prompt list, and configuration docs

At the end of this milestone, `ReadImage` appears only for models that declare image support, and both the registry and the prompt tool list agree.

Work: add a boolean `supports_images` (name to confirm against the configuration vocabulary) to `ModelConfig` in `src/config/model.rs`, with a serde default of false and a clear doc comment. In `src/clients/agent.rs`, register `ReadImage` when the flag is true; keep it out otherwise. Thread the flag into `format_tool_list_section()` in `src/clients/tools/mod.rs` and its caller in `src/prompts/mod.rs`, and add the new builtin name to the lists used for description filtering (`BUILTIN_TOOL_NAMES` and the filter set). Document the key in `docs/configuration.md` and note the `[[models]]` interaction. Confirm the `--tools` allowlist still narrows the set and cannot re-add a tool the gate removed.

Result at the end: a text-only model never sees `ReadImage`; a model with the flag set sees it in the prompt and in the request tools array.

Proof: tests assert the registry names and the generated prompt section with the flag on and off, and that `--tools ReadImage` alone does not bypass the gate. A snapshot pins the prompt section with the tool present.

Outcome (2026-10-07): implemented. `ModelConfig` and `ModelDefinition` gained `supports_images` (serde default false), `to_model_config` propagates it, and `debug models --json` now carries the key (the `tests/diagnostic_json.rs` golden was updated; `schema_version` stays 1 because the field is additive). `ToolCapabilities` gained `requires_image_support`, `ToolEntry::requires_images()` marks `ReadImage`, and `ToolRegistry::retain_model_supported_tools` drops it for a text-only model. `Agent::new` applies the filter from the resolved config, and `format_tool_list_section` takes `supports_images` and applies it before the sandbox and allowlist filters, so `--tools ReadImage` cannot re-add a gated tool. Because the prompt list depends on the model, `build_client_and_session` now builds the initial messages in the run-mode step that already resolved the model (`RunInputs::initial_messages`); resume and fork still resolve and load exactly once. `ReadImage` was added to `BUILTIN_TOOL_NAMES`
and the description filter set, and the line match became whole-word so a `Read` reference no longer deletes `ReadImage`'s own guidance lines. Focused tests cover the registry filter, the prompt list with the flag on and off, the allowlist, whole-word matching, `supports_images` parsing, and the agent registry. All existing prompt and request snapshots are unchanged: the shared prompt/backend test call sites pass an image-capable model, and the gate's text-only path is pinned by new tests. `cargo test` passes (1697 unit tests plus the integration binaries).

### Milestone 6: Full validation, documentation, and pull request

At the end of this milestone, the change is verified and described.

Work: run the routed checks in `CONTRIBUTING.md`, including the focused backend and tool tests and the full local gate. Add a live end-to-end transcript to Artifacts and Notes. Refresh any snapshots changed by the additive fields and confirm the ones that should be unchanged are unchanged. Update `docs/integrations.md` for the additive `images` fields on `message` and `function_call_output` records and any stream-json change. Fill in Outcomes & Retrospective, move this file to `docs/exec-plans/completed/` with `git mv`, and open a pull request that closes #689.

Result at the end: a reviewed pull request, a completed plan, and a passing gate.

Proof: the recorded commands and their results, and the pull request.

## Concrete Steps

All commands run from the repository root `/Users/travisennis/Projects/cake/cake-1` unless stated otherwise.

Confirm the baseline before editing:

```
cargo test clients::tools
cargo test clients::chat_completions
cargo test clients::responses
```

For Milestone 1, build a minimal request by hand. A Responses request that inlines a few-pixel PNG in a tool output looks like this (replace the URL, key, model, and call id):

```
curl -sS https://<responses-base-url>/responses \
  -H "Authorization: Bearer $API_KEY" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "<model>",
    "input": [
      {"type":"message","role":"user","content":[{"type":"input_text","text":"What color is the image?"}]},
      {"type":"function_call_output","call_id":"call_1",
       "output":[{"type":"input_text","text":"image"},
                 {"type":"input_image","image_url":"data:image/png;base64,<tiny-png-base64>"}]}
    ]
  }'
```

Interpretation: an answer describing the color means the native path works. An error that names `input_image` or `function_call_output` means it does not. Record either outcome verbatim.

After Milestone 2, exercise the tool and persistence:

```
cargo test read_image
cargo test types::session
```

After Milestones 3 and 4, exercise both translations:

```
cargo test clients::chat_completions
cargo test clients::responses
cargo insta review
```

After Milestone 5:

```
cargo test prompts
cargo test clients::tools
```

Then the full gate, per CONTRIBUTING.md:

```
just check
```

For the live end-to-end check, place a small unique image (for example a red triangle on white) in a temporary directory, configure a vision-capable model, and run cake from that directory with a prompt such as "Describe any images in this directory." Expect the model to call `ReadImage` and answer with the shape and color. Record the session id.

Finally, commit the specific changed paths and open the pull request:

```
git push -u origin feat/read-image-support
just pr
```

## Validation and Acceptance

A vision-capable model calls `ReadImage` on a PNG, JPEG, GIF, or WebP file inside the workspace and its answer correctly describes the image. A Chat Completions request carries the image as a user-message `image_url` part immediately after the tool result, and the request pairs every `tool` message with its `tool_calls` id. A Responses request carries the image in the shape Milestone 1 selected. Resume and fork replay the same image input the original run sent, verifiable by resuming a session that used an image and observing the same successful answer without re-reading the file. A text-only model never receives `ReadImage` and the prompt tool list omits it. Existing text sessions, existing session files written before this change, and the completion JSON shape are unchanged; an old session file with no `images` fields still loads and replays. Sending an image larger than the configured limit produces a clear model-visible error and no provider request.

## Idempotence and Recovery

All read, test, and snapshot commands are safe to repeat. Adding fields to records is additive: an interrupted run leaves older records valid, and a session written mid-change still loads because the new fields are optional. If a snapshot differs in a way this plan does not describe, stop and compare against the intended wire shape before accepting it. If Milestone 1 disproves the native Responses path, switch to the synthetic-user-message design and update the Decision Log and Milestones 4 before continuing; no earlier milestone is invalidated. If a live provider call fails for auth or network reasons, retry it without changing code.

## Artifacts and Notes

Issue: https://github.com/travisennis/cake/issues/689

Record here, as they occur: the Milestone 1 responses verbatim; the produced Chat Completions and Responses request fragments; and the live end-to-end session id with the model's answer.

### Milestone 1 probes (2026-10-06)

Fixture: 16x16 solid PNGs generated with Python `zlib` and inlined as `data:image/png;base64,...` (79-byte red, 78-byte blue). Requests were sent by hand with the endpoint's own auth (Codex auth file or bearer env var); base64 payloads are elided below.

1. Gateway Responses, native tool-output image --- `POST https://opencode.ai/zen/go/v1/responses`, model `gpt-6-luna`, input `message(user, input_text)`, `function_call(ReadImage)`, then `{"type":"function_call_output","call_id":"call_m1","output":[{"type":"input_text","text":"ReadImage red.png (16x16, image/png)"},{"type":"input_image","image_url":"data:image/png;base64,<elided>"}]}`. Result: HTTP 200, SSE ending in `response.completed` with `output_text` `"Red"`; the blue fixture returned `"Blue"`; no `error` event.
2. Gateway Responses, synthetic user message --- the same items with a text-only `function_call_output` followed by `message(user, [input_text, input_image])`. Result: HTTP 200, `"Red"`, status `completed`.
3. Gateway Chat Completions, user-message image --- `POST https://opencode.ai/zen/go/v1/chat/completions`, model `deepseek-v4.1-flash`, `messages: [{role:user, content:[{type:text,...},{type:image_url,image_url:{url:"data:image/png;base64,<elided>"}}]}]`. Result: HTTP 200, content `"Red"`.
4. Gateway Chat Completions, tool-message image --- assistant `tool_calls` plus a `tool` message whose `content` is an array with a text part and an `image_url` part. Result: HTTP 200, content `"Red"` for the red fixture and `"Blue"` for the blue fixture. The gateway accepts this shape, but the Chat Completions specification restricts tool-message content to text parts, so Milestone 3 keeps the synthetic user message.
5. Gateway Chat Completions, planned Milestone 3 shape --- assistant `tool_calls`, a text-only `tool` message, then `message(user, [input_text, input_image])`. Result: HTTP 200, content `"Red"`, so the synthetic user message keeps the tool call/output pairing valid on the configured endpoint.
6. Codex backend, native tool-output image --- `POST https://chatgpt.com/backend-api/codex/responses`, model `gpt-6-luna`, same input as probe 1 plus `store: false` and `stream: true` as `build_request_json` sends for this backend. First attempt with the stale token: HTTP 401, body `{"error":{"message":"Provided authentication token is expired. Please try signing in again.","type":"invalid_request_error","code":"token_expired","param":null},...}`. After a Codex CLI refresh: HTTP 200, SSE ending in `response.completed` with `output_text` `"Red"`, and `"Blue"` for the blue fixture; no `error` event. Without `store: false` the same request returns HTTP 400 `{"detail":"Store must be set to false"}`, which cake already satisfies.
7. Codex backend, synthetic user message --- probe 2's input with `store: false` and `stream: true`. Result: HTTP 200, `"Red"`, status `completed`.

### Milestone 3 request fragment (2026-10-07)

Produced by `build_messages` for a history of `Message(user)`, `FunctionCall(ReadImage)`, `FunctionCallOutput` carrying the 69-byte PNG from the tool tests; pinned by the `build_messages_tool_image_parts` snapshot. Role `tool` carries the text result, then a role `user` message carries the image, matching probe 5's verified shape.

```json
[
  {"role":"user","content":"Describe the screenshot."},
  {"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"ReadImage","arguments":"{\"path\":\"shot.png\"}"}}]},
  {"role":"tool","content":"ReadImage shot.png (69 bytes, image/png)","tool_call_id":"call-1"},
  {"role":"user","content":[
    {"type":"text","text":"ReadImage shot.png (69 bytes, image/png)"},
    {"type":"image_url","image_url":{"url":"data:image/png;base64,<elided>"}}
  ]}
]
```

### Milestone 4 request fragments (2026-10-07)

Produced by the `From<&ConversationItem>` conversion for the Responses `input` array, using the 69-byte PNG from the tool tests; pinned by the `to_api_input_function_call_output_with_images` and `to_api_input_user_message_with_images` snapshots and matching probe 1's native shape. The tool output leads with an `input_text` block and follows with one `input_image` block:

```json
{"type":"function_call_output","call_id":"call-1",
 "output":[
   {"type":"input_text","text":"ReadImage shot.png (69 bytes, image/png)"},
   {"type":"input_image","image_url":"data:image/png;base64,<elided>"}
 ]}
```

A user message that carries an image uses the same text-then-image block list:

```json
{"type":"message","role":"user",
 "content":[
   {"type":"input_text","text":"What is in this picture?"},
   {"type":"input_image","image_url":"data:image/png;base64,<elided>"}
 ]}
```

Live end-to-end against the native path (2026-10-07): a 32x32 PNG (a green right triangle with a blue top-left corner) in `/tmp/cake-m4`, run from that directory as `cake --model codex-luna-none --reasoning-effort none --output-format json "Describe any images in this directory, including colors and shapes."`. The model ran `Bash` (`find`), then `ReadImage /private/tmp/cake-m4/shape.png`, and answered "It shows a green square with a dark-blue diagonal stripe running from the upper-left corner to the lower-right." Session `b706bb8f-16dd-4a5f-b5ac-1b4ac462abc2`. The answer names both actual colors, so the pixels reached the model through the image-bearing `function_call_output` (its persisted record carries the `images` array).

### Milestone 6 live end-to-end and resume (2026-10-07)

Both runs used a temporary isolated config (`XDG_CONFIG_HOME` and `CAKE_DATA_DIR` under `/tmp/cake-m6`) defining one vision-capable Codex model (`gpt-6-luna`, `api_type = responses`, `supports_images = true`), so the user's real settings were untouched. The fixture is a 64x64 PNG, `figure.png` (234 bytes): a red disc centered on a blue background.

Describe run: `cargo run -- --model codex-luna-vision --reasoning-effort none --output-format json "Describe any images in this directory, including their colors and shapes."` from the fixture directory. The model ran `Bash` (`find . -type f`) and then `ReadImage /private/tmp/cake-m6/work/figure.png`, and answered "The image shows a small red circle centered on a solid blue square background. It's named `figure.png`." The persisted `function_call_output` record carries `images: [{"media_type":"image/png", ...}]`. Session `3b1ca3e4-0d18-433e-a5c2-d43d6c5bc971`.

Resume replay run: `... --resume 3b1ca3e4-0d18-433e-a5c2-d43d6c5bc971 "Without using any tool, tell me the exact colors of the shape and the background..."`. One turn, no tool call, answered "The circle is red, and the background is blue." The pixels reached the provider from the stored session record, not a re-read of the file, so resume replays the same image input the original run sent.

## Interfaces and Dependencies

New and changed types, using repository paths:

- `crate::types::ImagePart`, a struct with `media_type: String` and `data_base64: String`. It is the single representation of an image in the internal conversation and in persisted records.
- `crate::types::ConversationItem::Message` and `::FunctionCallOutput` each gain an `images: Vec<ImagePart>` field. No existing field changes type, so provider translation and persistence keep their current text behavior when the vector is empty.
- `crate::types::session::MessageData` and `::FunctionCallOutputData` each gain an optional `images` field, serialized only when non-empty, preserving existing snapshots and old records.
- `crate::clients::tools::ToolResult` gains `images: Vec<ImagePart>`; the tool executor contract stays a single returned struct.
- The Chat Completions DTOs gain a content enum that serializes to a string or a parts array; the Responses DTOs gain an image content block and, if Milestone 1 permits, an array form of `function_call_output.output`.
- `crate::config::model::ModelConfig` gains a boolean image-support flag with a false default; the registry and prompt-list builder consume it.

No new crate dependency is required: `base64` and `infer` are already present. If downscaling large images becomes necessary later, that would add an image-processing dependency and is deliberately out of scope here.

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
- [ ] Milestone 1: prove the Responses API behavior for images inside a function-call output and record the finding in the Decision Log.
- [ ] Milestone 2: add the typed image part, the `ReadImage` tool, and additive persistence so a tool can produce an image and a session can store and reload it.
- [ ] Milestone 3: translate images for the Chat Completions backend end to end.
- [ ] Milestone 4: translate images for the Responses backend end to end.
- [ ] Milestone 5: gate `ReadImage` on a model capability, surface it in the prompt tool list, and document the setting.
- [ ] Milestone 6: run the full gate, complete documentation and snapshots, and prepare the pull request.

## Surprises & Discoveries

- Observation: cake is text-only at every layer, not just in the tool layer. Evidence: `ConversationItem::Message { content: String }` and `ConversationItem::FunctionCallOutput { output: String }` in `src/types/conversation.rs`; `ChatMessage.content: Option<Cow<str>>` in `src/clients/chat_types.rs`; `ResponsesApiInputItem::FunctionCallOutput { output: &str }` in `src/clients/responses_types.rs`; `ToolResult.output: String` in `src/clients/tools/mod.rs`; `MessageData.content: String` and `FunctionCallOutputData.output: String` in `src/types/session.rs`. Any image feature must touch all of these.
- Observation: the Chat Completions API cannot carry an image in a tool-role message; images are only accepted in user-role message content arrays. Evidence: the tool result is emitted as a plain string tool message in `src/clients/chat_completions.rs` (`push_function_call_output`), and the API's tool message content is text-only. This forces the synthetic-user-message design in Milestone 3.
- Observation: the dependencies needed for image work already exist. Evidence: `Cargo.toml` lists `base64` and `infer`; `infer` is already used for MIME detection in `src/clients/tools/bash.rs` (`detect_mime_type`).

## Decision Log

- Decision: Add a dedicated `ReadImage` tool instead of overloading `Read`. Rationale: tool schemas, descriptions, and snapshots are a compatibility surface; overloading `Read` would change its contract, make `start_line`/`end_line` meaningless for images, and force a binary-versus-image branch into a text tool. A new tool registers read-only and replay-safe, gates cleanly, and carries its own description and snapshots. Date/Author: 2026-10-06, cake.
- Decision: Represent images as an additive `images: Vec<ImagePart>` field on the message and tool-output domain types, leaving the existing `content`/`output` strings unchanged. Rationale: this keeps the persisted text contract, the stream-json `message` and `function_call_output` shapes, and every existing text test intact; older readers ignore the new field and newer readers tolerate its absence. Date/Author: 2026-10-06, cake.
- Decision: Persist image bytes inline as base64 in the session JSONL, bounded by a per-image size cap, rather than in a content-addressed blob store. Rationale: cake's session files are append-only, self-contained, and must replay identically on resume and fork; inline data keeps that guarantee with no new store to create, back up, or garbage-collect. A blob store can be a later optimization if session size becomes a problem. Date/Author: 2026-10-06, cake.
- Decision: Prefer the Responses API's native image-bearing `function_call_output`; fall back to the synthetic-user-message shape everywhere if provider verification fails. Rationale: the native path keeps history clean and avoids a synthetic message, but its availability varies across OpenAI-compatible gateways. Milestone 1 settles this before other work depends on it. Date/Author: 2026-10-06, cake.
- Decision: Gate `ReadImage` on a new per-model capability setting, default off. Rationale: sending an image to a text-only model produces a provider error; an opt-in capability avoids that class of failure and keeps the default tool set unchanged. Date/Author: 2026-10-06, cake.

## Outcomes & Retrospective

To be completed when the work lands.

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

### Milestone 2: Domain model, `ReadImage` tool, and additive persistence

At the end of this milestone, a `ReadImage` call produces image bytes in the internal representation, a session can store and reload them, and every existing text test still passes. No provider request carries an image yet.

Work: in `src/types/conversation.rs`, add `ImagePart { media_type: String, data_base64: String }` and an `images: Vec<ImagePart>` field to `ConversationItem::Message` and `ConversationItem::FunctionCallOutput`, defaulting empty. In `src/types/session.rs`, add the optional `images` field to `MessageData` and `FunctionCallOutputData`, skip it when empty, and update `from_conversation_item_with_replay` and `to_conversation_item`. In `src/clients/tools/mod.rs`, add `images: Vec<ImagePart>` to `ToolResult` and set it empty wherever a `ToolResult` is constructed (including the bash, edit, write, and toolbox paths, and test fixtures). Create `src/clients/tools/read_image.rs` and `src/clients/tools/read-image-description.txt`: parse a required `path`, validate it with the existing cwd/allowed-dir validation, read the file under a size cap, detect the MIME type with `infer`, reject non-image types with a message that points at `Read`, and return a `ToolResult` whose `output` is a short
human-readable summary and whose `images` holds one sized-capped `ImagePart`. Register it in `default_tool_registry()` with `read_safe()` and `replay_safe()`. Add a `[tools]` limit for the maximum image bytes in `src/config/settings.rs`, mirroring the existing read limits, and use it in the tool. Update `src/clients/tools/read.rs` so its binary-file rejection message mentions `ReadImage` when the file looks like an image.

Result at the end: `ReadImage` is registered and executable, returns an image part, and a session round-trips that image part through JSON.

Proof: a unit test calls the ReadImage executor on a fixture PNG and asserts the returned `images` has one entry with the expected media type, and a session-record round-trip test writes a `MessageData` and a `FunctionCallOutputData` with images, serializes them, reloads them, and asserts equality. The existing session snapshot tests and `cargo test clients::tools` still pass. The text-only fixtures show no `images` field in their JSON.

### Milestone 3: Chat Completions translation

At the end of this milestone, a Chat Completions request built from a history containing a tool image contains the tool message followed by a user message whose content array carries the image, and the tool call/output pairing remains valid.

Work: extend `src/clients/chat_types.rs` with a content type that serializes as either a plain string or an array of parts, where an image part is `{"type":"image_url","image_url":{"url":"data:<media_type>;base64,<data>"}}` and a text part is `{"type":"text","text":"..."}`. Extend `ChatMessage` so its `content` uses this type. In `src/clients/chat_completions.rs`, keep emitting the text tool message as today, and when a `FunctionCallOutput` carries images, emit a following user-role message with an array content: a short text part naming the tool result, then one image part per image. Keep the plain-string serialization for every message that has no images so existing snapshots do not change.

Result at the end: a synthetic user message carries the image after the tool result.

Proof: a focused test builds a history with a function call, a function-call output carrying an image, and asserts the produced messages are `[assistant(tool_calls), tool(text), user(content array with image_url)]`; a snapshot pins the exact JSON. An unmodified text history produces byte-identical output to today.

### Milestone 4: Responses translation

At the end of this milestone, a Responses request built from a history containing a tool image carries the image, using the strategy chosen in Milestone 1, and reasoning and message inputs are unchanged for text.

Work: if the native path was chosen, extend `src/clients/responses_types.rs` so `ResponsesMessageContent` can be a text block or an image block (`{"type":"input_image","image_url":"data:..."}`) and so `ResponsesApiInputItem::FunctionCallOutput`'s `output` can be a string or an array of those blocks; update the `From<&ConversationItem>` conversion in `src/clients/responses.rs` accordingly. If the fallback path was chosen, keep `function_call_output` text-only and insert a synthetic user `message` item after the output, mirroring Milestone 3. Also support user messages that carry images directly, since a future CLI attachment path would reuse them.

Result at the end: the Responses request carries the image in the shape the provider accepts.

Proof: focused tests and snapshots for a tool image and for a user message with an image, asserting the exact JSON, plus the existing `to_api_input_*` snapshots unchanged for text. If the native path is used, a live call against the target provider returns an answer that describes the image.

### Milestone 5: Capability gate, prompt list, and configuration docs

At the end of this milestone, `ReadImage` appears only for models that declare image support, and both the registry and the prompt tool list agree.

Work: add a boolean `supports_images` (name to confirm against the configuration vocabulary) to `ModelConfig` in `src/config/model.rs`, with a serde default of false and a clear doc comment. In `src/clients/agent.rs`, register `ReadImage` when the flag is true; keep it out otherwise. Thread the flag into `format_tool_list_section()` in `src/clients/tools/mod.rs` and its caller in `src/prompts/mod.rs`, and add the new builtin name to the lists used for description filtering (`BUILTIN_TOOL_NAMES` and the filter set). Document the key in `docs/configuration.md` and note the `[[models]]` interaction. Confirm the `--tools` allowlist still narrows the set and cannot re-add a tool the gate removed.

Result at the end: a text-only model never sees `ReadImage`; a model with the flag set sees it in the prompt and in the request tools array.

Proof: tests assert the registry names and the generated prompt section with the flag on and off, and that `--tools ReadImage` alone does not bypass the gate. A snapshot pins the prompt section with the tool present.

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

## Interfaces and Dependencies

New and changed types, using repository paths:

- `crate::types::ImagePart`, a struct with `media_type: String` and `data_base64: String`. It is the single representation of an image in the internal conversation and in persisted records.
- `crate::types::ConversationItem::Message` and `::FunctionCallOutput` each gain an `images: Vec<ImagePart>` field. No existing field changes type, so provider translation and persistence keep their current text behavior when the vector is empty.
- `crate::types::session::MessageData` and `::FunctionCallOutputData` each gain an optional `images` field, serialized only when non-empty, preserving existing snapshots and old records.
- `crate::clients::tools::ToolResult` gains `images: Vec<ImagePart>`; the tool executor contract stays a single returned struct.
- The Chat Completions DTOs gain a content enum that serializes to a string or a parts array; the Responses DTOs gain an image content block and, if Milestone 1 permits, an array form of `function_call_output.output`.
- `crate::config::model::ModelConfig` gains a boolean image-support flag with a false default; the registry and prompt-list builder consume it.

No new crate dependency is required: `base64` and `infer` are already present. If downscaling large images becomes necessary later, that would add an image-processing dependency and is deliberately out of scope here.

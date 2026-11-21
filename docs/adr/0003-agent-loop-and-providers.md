# 0003 --- Agent loop, provider turns, and stream recovery

- **Status:** Accepted
- **Date:** 2026-11-20
- **Governs:** `src/clients/agent.rs`, `src/clients/agent/`, `src/clients/agent_runner.rs`, `src/clients/provider_strategy.rs`, `src/clients/chat_completions.rs`, `src/clients/chat_types.rs`, `src/clients/responses.rs`, `src/clients/compaction.rs`, `src/clients/termination.rs`, `src/clients/image_budget.rs`, `src/clients/mcp/`

## Context

The loop owns conversation state, provider turns, retries, compaction, tool scheduling, and the record fan-out. Two provider wire formats share one typed conversation. Recovery paths (image rejection, output-schema correction, input overflow, stale connections) can each replay part of a turn, so the rules about what a discarded attempt may leave behind are the difference between a clean transcript and duplicated usage, duplicated tool calls, or a provider request that is not a prefix extension of the previous one.

## Decisions

### Invocation boundary and history

- Items at or after the current invocation index belong to this task. A resumed session's earlier tasks never contribute to this task's partial result.
- Conversation growth is tail-only: each request's message sequence is an exact prefix extension of the previous one, except when a committed context checkpoint starts a new projection. Requests use the projection, never the raw vector. Raw offsets keep their meaning for attribution while a checkpoint may replace retired history.
- A checkpoint replacement pins synthetic recovery and schema-correction context because those modes belong to the same invocation.
- The loop reconciles the tool surface before each request. A server may have changed its tools since the previous turn; calls from the just-finished turn already ran, so the next request must carry one coherent set.

### Limits and turns

- Check a user-configured turn or tool-call limit **before** starting another turn or executing a batch. A turn that already consumed the budget, or a batch that would exceed the tool-call budget, could never produce a final answer within the configured limits.
- Count every completed API turn unconditionally. `AgentRunner` settles usage before it classifies or retries the provider attempt, so a retried turn still counts.
- A correction turn offers no tools. If a provider nevertheless returns a function call, do not execute it: append synthetic `FunctionCallOutput` items for every unexecuted call so history stays well formed for both Responses and Chat Completions, and treat the turn as a failed validation attempt.
- Output-schema correction is bounded to `MAX_SCHEMA_CORRECTION_TURNS`. When the final message fails validation, re-enter with tools disabled.

### Recovery and discarded attempts

- Classify a provider response before persisting or streaming the turn's items and before dispatching returned tools, so a discarded response leaves no trace beyond usage its attempt already settled.
- A discarded response never also receives a semantic continuation. One recovery per invocation is shared by the HTTP overflow and in-band Responses failure paths.
- The provider accepted a response, so every image the request emitted reached it. A discarded response never reaches this point; an image-recovery retry replaces the plan, so a rejected attempt cannot consume pixels.
- A typed input overflow is also a `400`, but it is not a rejected constraint. Retrying without the schema cannot make the input fit and would spend a second request before the compaction owner sees the verdict. The terminal error carries the classifier's verdict so compaction can act without re-parsing the body; an overflow that only rejected the requested output allowance was already retried with a reduced allowance.
- Argument-driven compensations are classified centrally. The repair pass and Edit parse are the source of truth, and the events survive calls that fail after a repair. An unregistered tool never reached an argument parser, so it cannot carry an argument compensation.
- Tool errors carry the events observed while the tool failed (a judge block verdict, a fail-closed denial), so those events still reach session telemetry.

### Scheduling

- Planning may complete out of order, but the first error and every successful plan retain the model's issue order.

### Images

- A context fraction is meaningful only when the provider reported every counter and the model configures a window. A partial report would otherwise read as a measured context size the provider never gave, and summary attempts never describe the main-agent context.
- Image admission is request-only. It happens before the first attempt so every retry sees the same projection, and it never changes an image an earlier request already sent, which is what keeps requests prefix extensions. A text-only model never receives images and therefore plans none.

### Chat Completions shaping

- Backward merge: when a `FunctionCall` immediately follows an assistant message, attach the tool call to that message. This supports the `[Message(assistant), FunctionCall]` ordering produced by the shared conversation type.
- A turn's tool messages must stay adjacent to the assistant's tool call, so synthetic image messages queued by a tool output are emitted only when the run of tool outputs ends. Every other arm flushes the queue first.
- A tool-role message's content is text only, so an image result travels as a synthetic user message, queued rather than pushed.
- Extract text content before tool calls so stream-json events match the canonical Responses ordering: reasoning, message, function call. Tool calls may coexist with text. If the provider returned neither, add an empty assistant message so the caller knows the model responded.
- A complete JSON body that arrived just before a stall is still usable; a truncated body keeps the bounded preview on the error.

### Responses shaping

- The Responses API requires an `annotations` field (even empty) on assistant `output_text` blocks. Non-assistant `input_text` blocks must omit it; sending an empty array on the wrong block produces a malformed turn.
- A message carrying images leads with its text block and follows with one inline image block per image.
- `summary: []` is sent instead of omitting the field. The API treats the two equivalently, and the DTO maps the domain `Option` to a non-optional `Vec`. The behavior predates `summary` becoming optional and is retained for wire compatibility.
- Normalize missing, empty, or whitespace-only tool arguments to `{}`. Some providers send none or an empty string for a parameterless call; the toolbox argument parser already does the same normalization.
- A stream that delivered its terminal event or a complete JSON body before the stall is usable. Anything else fails with the stall error and a bounded preview. A terminal `response.failed` event that arrived before the stall keeps its typed provider metadata and retry classification instead of being replaced by a generic stall.
- Return a typed error so `AgentRunner` can downcast it, classify retryability from the bounded metadata, and carry structured diagnostics to telemetry instead of a flattened string.

### Provider strategy

- Responses may normalize an omitted `strict` setting. Explicitly opt out to preserve toolbox optionality, matching Codex's function tools, without changing other compatible providers.
- Demote developer messages to the user role for Chat Completions providers that do not support the `developer` role.
- Backfill a placeholder into assistant tool-call messages that lack reasoning content. Reasoning models (Kimi, DeepSeek, and others) in thinking mode require the field to round-trip, and provider-side routing can open the gap mid-session without cake's knowledge. The placeholder is a no-op for providers that ignore the field.
- The no-`ToolSearch` fallback registers deferred tools directly and replaces the registry entry so a changed definition reaches the model.

### Retries, timing, and body reads

- Keep provider timing separate from the synchronous telemetry write. The phase record is synchronous, so remove it from the provider's total duration as well as from body parsing time. The body timer starts after the phase update is persisted, so telemetry I/O is not attributed to response parsing.
- Start from the persistent pooled client. Stale-connection recovery swaps in a temporary no-reuse client local to this turn, so `self.client` keeps normal pooling whether the turn retries successfully or fails, and later turns observe the pooled client.
- The recorded failure class selects the recovery path, so telemetry and behavior cannot disagree about the failure's nature.
- A reset, broken pipe, or truncated body read after headers were accepted is the same transient transport class the request phase recovers from. It takes the same bounded retry rather than the body-parse terminal path.
- Keep the read cause first so a large provider body cannot push it out of a bounded sink, and do not repeat the body's preview when the body itself is already retained.

### Compaction

- One assistant response groups every consecutive assistant item, then the outputs that close its calls. A user, developer, or system message starts its own exchange, so an answer never shares an exchange with the input that follows it.
- When the pin owner is unknown, treat the pin as current. Clearing it would let selection retire content the active checkpoint promised to keep.
- The serialized summary reservation is measured before generation; the final record is the authoritative post-generation check.

### Termination

- A partial usage object does not prove which counters the provider reported, so neither comparison may trust it.

### MCP subscriptions

- Subscribe before the initial listing. A change that lands during startup is then counted and refreshed instead of being lost between the listing and the subscription.
- Start from zero rather than the server's current count. A notification that arrives between the listing and the builder call must still trigger one re-list before the first request.
- Record the pre-request generation. A notification during the re-list makes the next loop run again instead of being swallowed.
- A notification for a subscription this client did not open cannot be trusted as belonging to this connection's tool stream.
- A `subscriptions/listen` response reports graceful closure, not a request result, and never has a pending sender. A response arriving after its request timed out is harmless.

## Consequences

The projection, cancellation, and recovery rules are invisible in the types. Changing them requires checking this record against the request-shaping code in both backends and the session records that attribute usage to an attempt.

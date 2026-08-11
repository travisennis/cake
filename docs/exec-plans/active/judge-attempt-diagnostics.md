# Expose judge attempts and opt-in request diagnostics

This ExecPlan is a living document, maintained per
`docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries,
Decision Log, and Outcomes & Retrospective must be kept current as work
proceeds.

## Purpose / Big Picture

After this change, a saved Cake session explains every provider attempt made by
the Bash command-safety judge: which resolved model controls were used, how long
request construction, the HTTP request, response parsing, and verdict parsing
took, what status and termination the provider returned, and what token usage
was available. Timeouts and transport failures retain their elapsed duration,
so an operator can distinguish a slow provider from response or verdict parsing
without reconstructing the request path from source code.

For an incident that requires exact request inspection, a contributor can run
`cake bash check --diagnostic -- <command>`. The command still never executes
the supplied shell command, but it prints a sensitivity warning followed by the
effective system prompt, user prompt, provider-transformed request JSON,
resolved request controls, zero-tool configuration, and parsed response and
verdict metadata. This raw view is opt-in because it contains command text,
paths, repository state, and the model-supplied reason. Ordinary session
telemetry remains metadata-only.

The compatibility effect is additive: the sidecar gains a new
`judge_attempt` record and `cake bash check` gains an optional flag. Existing
CLI output and exit behavior, provider request semantics, judge verdict and
fail-closed behavior, compensation records, session transcripts, and old
sidecar parsing remain unchanged.

## Progress

- [x] (2026-08-11 19:00Z) Read issue #206, selected Ready P1 child #202, claimed it, and created `feat/judge-attempt-diagnostics` from `origin/master`.
- [x] (2026-08-11 19:00Z) Read the task and ExecPlan workflows plus the architecture, integration, security, telemetry, provider, judge, Bash preflight, and CLI authorities.
- [x] (2026-08-11 19:00Z) Chose the additive telemetry and opt-in `cake bash check --diagnostic` design recorded below.
- [x] (2026-08-11 19:25Z) Added reusable provider request construction and focused Chat Completions and Responses request-shape coverage.
- [x] (2026-08-11 19:25Z) Instrumented the bounded judge call and returned one metadata-only attempt for every provider call outcome.
- [x] (2026-08-11 19:25Z) Carried judge attempts from Bash tool success and error paths into session telemetry without changing compensation or denial behavior.
- [x] (2026-08-11 19:25Z) Added and verified the opt-in raw diagnostic renderer, sensitivity warning, and API-key redaction boundary.
- [x] (2026-08-11 19:25Z) Updated the sidecar parser and tests so old and new telemetry coexist, then updated integration and security documentation.
- [ ] Run final verification and preflight, record results, finish the issue acceptance notes, and archive this plan (completed: focused unit and Python tests, strict Clippy, CC, and module-size checks; remaining: preflight and `just ci`).

## Surprises & Discoveries

- Observation: the current judge wraps request send, response body parsing, and
  verdict parsing in one `tokio::time::timeout`, then discards all phase state
  on timeout. Evidence: `src/clients/judge.rs::JudgeClient::judge` returns only
  `JudgeVerdict` or `JudgeError`, while the Bash preflight times the outer call
  only for successful verdict compensation events.

- Observation: judge metadata can cross the concurrent tool boundary through
  the same success and error objects that already preserve compensation
  events. Evidence: `src/clients/tools/mod.rs::ToolResult` and `ToolError` carry
  compensation events, and `src/clients/agent/agent_loop.rs` records them after
  each tool finishes in issue order.

- Observation: both backend adapters construct their provider-transformed
  request locally inside `send_request`, so the current code has no supported
  way to show the exact JSON without duplicating transformation logic.
  Evidence: `src/clients/chat_completions.rs::send_request` and
  `src/clients/responses.rs::send_request` each build a private request value
  immediately before sending it.

- Observation: the metrics loader safely ignores unknown telemetry record
  types, but it cannot expose judge attempts until its `Invocation` data model
  gains a separate collection. Evidence:
  `scripts/session-metrics/cakelib.py::load_telemetry` dispatches known record
  names with an `if` chain and otherwise continues.

- Observation: provider request identifiers are available in both response
  headers and parsed response envelopes. Evidence: the observer first checks
  `x-request-id`, `request-id`, and `openai-request-id`, then falls back to the
  Chat Completions or Responses body `id` retained on `TurnResult`.

- Observation: keeping request, parse, timeout, diagnostic, and verdict paths
  in `JudgeClient::judge_observed` exceeded the new-function CC target and
  pushed `src/clients/judge.rs` over the module-size threshold. Evidence: the
  initial `just cc-check` reported CC 16; extracting the phase state machine to
  `src/clients/judge_observer.rs` reduced the gate to zero exceedances and left
  `src/clients/judge.rs` at 686 lines.

## Decision Log

- Decision: add a first-class `judge_attempt` sidecar record rather than
  extending `api_attempt` or overloading `compensation`. Rationale: ordinary
  agent API attempts are keyed by conversation turn and have retry semantics
  that the judge does not yet share; keeping the records identifiable avoids
  false aggregation while reusing provider-neutral field names where they mean
  the same thing. Date/Author: 2026-08-11 / Codex.

- Decision: use `cake bash check --diagnostic` as the explicit raw inspection
  surface. Rationale: `cake bash check` already resolves the same judge model,
  rubric, request context, allowlist, bypass, and exit classification as the
  Bash preflight, and already promises not to spawn the inspected command. The
  option is local to one invocation and cannot silently make normal telemetry
  sensitive. Date/Author: 2026-08-11 / Codex.

- Decision: refactor each backend adapter to build a serializable request JSON
  through one reusable code path, then send that exact value. Rationale: the
  diagnostic must match the wire body after provider strategy transformation;
  two independently implemented serializers would eventually drift. Normal
  agent requests continue through the same builder, preserving wire behavior.
  Date/Author: 2026-08-11 / Codex.

- Decision: keep telemetry usage optional as a complete block and keep token
  subfields provider-neutral. Rationale: providers may omit usage entirely;
  serializing `null` instead of a zero-filled block distinguishes unavailable
  accounting from a genuine zero-token result while retaining Cake's existing
  canonical `Usage` vocabulary when usage is supplied. Date/Author: 2026-08-11
  / Codex.

- Decision: do not create a new ADR. Rationale: this change observes the
  already accepted ADR-018 request and retains ADR-007's separate,
  metadata-only, best-effort sidecar. It does not change the command gate,
  persisted session state, settings precedence, or trust decision. The raw
  diagnostic is an explicit CLI action and is documented as sensitive.
  Date/Author: 2026-08-11 / Codex.

- Decision: defend the diagnostic boundary against four bypass classes before
  implementation: accidentally enabling raw capture for Bash preflight,
  persisting raw diagnostic fields in the sidecar, rendering authentication or
  provider-secret headers, and changing request transformation between the
  displayed and sent bodies. Tests must prove the first three remain absent and
  captured-wire equality must prove the fourth. Date/Author: 2026-08-11 / Codex.

- Decision: transport judge attempts through a serde-skipped collection on the
  existing compensation event, then emit them as first-class records before
  serializing the compensation. Rationale: compensation events already survive
  concurrent tool success and error paths; the skipped carrier reuses that
  proven path without nesting attempt data in compensation JSON or widening
  every generic tool result type. Date/Author: 2026-08-11 / Codex.

## Outcomes & Retrospective

Implementation is in progress. This section will record the delivered
behavior, checks, documentation assessment, and any residual risk before the
plan moves to `docs/exec-plans/completed/`.

## Context and Orientation

Cake is a Rust binary. Every non-empty model-generated Bash command passes
through the LLM judge in `src/clients/judge.rs` before the Bash executor may
spawn it. `JudgeClient::judge` builds a two-item internal conversation: an
embedded system rubric and one user message containing the raw command,
working directory, compact repository digest, and optional untrusted reason as
JSON. It chooses `Backend::ChatCompletions` or `Backend::Responses` from the
resolved model configuration, sends an empty tool list, parses a provider turn,
and parses the assistant text as a strict `JudgeVerdict`. Every error fails
closed.

`src/clients/tools/bash.rs::bash_judge_preflight` calls that path for normal
agent Bash use. It currently emits one compensation event: `judge_verdict`,
`judge_fail_closed`, or `judge_bypass`. Those events cross tool execution in
`ToolResult` or `ToolError`, reach
`src/clients/agent/agent_loop.rs::record_tool_results`, and are appended to the
session telemetry writer by
`src/clients/agent/agent_telemetry.rs`. A judge attempt needs the same reliable
success-and-error transport, but it is an operational provider record rather
than a model-compensation counter.

The telemetry schema is in `src/session_telemetry.rs`. Its existing
`api_attempt` record contains request, parse, and total time, status, usage,
termination, and request overrides for the conversation model. The new
`judge_attempt` record will be additive and independently named. It will carry
attempt ordinal; request-build, request, response-parse, verdict-parse, and
total milliseconds; prompt byte counts and history item count; the resolved
raw model, API type, reasoning effort, temperature, top-p, output and reasoning
token limits, configured timeout, tool count, and tool-choice state; optional
HTTP status, provider request identifier, canonical usage, and provider
termination; and a bounded terminal class. It must not carry prompt fields,
command, reason, cwd, raw response body, credential, authorization header, or
arbitrary provider error body.

`src/clients/chat_completions.rs` and `src/clients/responses.rs` are the wire
authorities. Each currently constructs and sends its request inside one
function. Request construction will be extracted without changing serialized
fields, provider-specific transformations, headers, URLs, or response parsing.
Their existing snapshots and focused tests protect this compatibility surface.

`src/cli/bash.rs` owns `cake bash check`. Its ordinary output is the existing
verdict, code, message, confidence, override, and latency display. The new
`--diagnostic` flag will ask the observed judge path to retain raw in-memory
diagnostic data for this one call and render it after an explicit sensitivity
warning. Nothing is written by that command solely because the flag is set.

## Plan of Work

### Milestone 1: Make provider-transformed request JSON inspectable without drift

Extract request construction in `src/clients/chat_completions.rs` and
`src/clients/responses.rs` into adapter-local helpers that return the exact
serializable JSON value used for the HTTP body. Add `Backend` methods in
`src/clients/backend.rs` that build the value and send a prebuilt value through
the existing URL, provider-header, and bearer-auth behavior. Keep the existing
`send_request` entry point for the agent runner, implemented through the same
builder and sender.

Focused request tests must cover both API types, the configured temperature,
top-p, output and reasoning controls, provider transformations, and an empty
tool slice. Capturing a stub request must produce a JSON body equal to the
value returned for diagnostics. Completion of this milestone means there is
one source of truth for both displayed and transmitted wire JSON.

### Milestone 2: Observe every bounded judge attempt

Add `JudgeAttemptTelemetry` and a terminal classification vocabulary in
`src/session_telemetry.rs`, then refactor `JudgeClient` so an observed call
returns the verdict result plus its attempt metadata. Apply the configured
total deadline around each asynchronous phase using the remaining budget so a
timeout can retain elapsed time for the phase in which it occurred. Measure
request construction, sending through response headers, response body parsing,
and verdict parsing separately. Capture provider termination, usage, status,
and a recognized provider request ID when available.

Keep `JudgeClient::judge` and `evaluate_command` as compatibility wrappers if
that keeps existing callers and tests narrow. The Bash preflight will use the
observed form, attach the attempt to its existing success or error telemetry
payload, and preserve all verdict, allowlist, bypass, model-visible error, and
command-not-spawned behavior. The agent telemetry layer will append every
attached attempt as a `judge_attempt` record before or alongside the existing
compensation record. Configuration and rubric failures occur before a provider
attempt and therefore retain their fail-closed compensation without fabricating
an attempt; bypass similarly makes no attempt.

Focused tests will prove success, HTTP failure, transport failure, timeout,
malformed verdict, refusal, missing usage, both API types, zero tools, and the
absence of every raw sensitive input from serialized attempt records.

### Milestone 3: Render exact opt-in diagnostics

Add `--diagnostic` to `BashCheckCommand` in `src/cli/bash.rs`. When absent, use
the existing output byte-for-byte. When present, retain the assembled system
and user prompt, exact request JSON, resolved non-secret model controls, tool
count and tool choice, parsed assistant content, canonical usage and
termination, and final verdict. Render a prominent warning that raw prompts
may contain command text, paths, repository state, reason text, and secrets
embedded by the caller.

The renderer must never include the resolved API key, authorization headers,
provider-secret configured headers, or unrelated environment variables. Tests
will use sentinel secrets in the resolved config and headers, assert that none
appear, and compare the displayed request JSON with the stub server's captured
body. The command remains spawn-free and retains existing exit-code
classification on timeout, transport, malformed, and refusal errors.

### Milestone 4: Preserve analysis compatibility and document the boundary

Teach `scripts/session-metrics/cakelib.py` to retain `judge_attempt` records in
an invocation without requiring them. Add fixtures or unit tests that load an
old sidecar with no judge attempts and a new sidecar containing one, proving
both parse without errors and existing aggregates remain unchanged.

Update `docs/integrations.md` with the new additive record and its field
semantics. Update `docs/security.md` to restate that default telemetry is raw
content-free and to document the raw diagnostic's sensitivity and explicit
one-invocation scope. Update `scripts/session-metrics/README.md` so operators
know the loader recognizes judge attempts. No configuration documentation is
needed unless implementation introduces a setting, which this plan avoids.

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake`.

First, implement and test the shared wire request builders:

    cargo test clients::chat_completions_tests
    cargo test clients::responses_tests

The existing request tests and new diagnostic equality cases should pass for
both backends with no snapshot drift beyond deliberately added coverage.

Next, implement judge attempt telemetry and its tool-to-sidecar transport:

    cargo test clients::judge_tests
    cargo test clients::tools::bash_tests::test_judge
    cargo test session_telemetry

The judge suite should show one attempt on every provider call, elapsed time on
timeouts, no attempt for bypass or pre-request configuration failure, and no
raw command or prompt in serialized records.

Then implement the CLI diagnostic and parser compatibility tests:

    cargo test cli::bash_tests
    python3 -m unittest discover -s scripts/session-metrics/tests

The normal CLI tests should retain their current output assertions. Diagnostic
tests should show the warning, prompts, exact request JSON, zero-tool state,
parsed response, usage, and verdict while excluding sentinel credentials.

Finally format, run focused complexity verification, invoke the repository
preflight skill, and run the full gate:

    cargo fmt --check
    just cc-check
    just ci

The expected result is that every command exits zero. If a platform-dependent
or credentialed check cannot run, record the exact skipped command and reason
in the issue and pull request handoff rather than weakening the deterministic
stub coverage.

## Validation and Acceptance

A stubbed successful Bash call through the normal agent path must produce one
`judge_attempt`, one `judge_verdict`, and one `tool_call` sidecar record tied to
the same invocation. The attempt must report status 200, attempt 1, non-null
phase and total durations, the selected model and controls, tool count zero,
tool choice absent, usage when the stub supplies it, and completed termination.
Neither the serialized command nor sentinel reason, cwd, API key,
authorization value, request body, or response body may occur anywhere in the
attempt record.

A stub that never responds within a short configured deadline must produce one
`judge_attempt` with terminal class `timeout`, a non-null total duration near
that deadline, and the elapsed duration of the active phase. It must also
produce the existing `judge_fail_closed:timeout` compensation and a Bash tool
error, and no command process may spawn.

A malformed successful provider response must produce one status-200 attempt
with response and verdict parsing attribution, explicit missing usage when the
provider omitted it, the available termination metadata, and the existing
malformed fail-closed result. Transport and non-success HTTP cases must retain
their known status or absence without serializing the response body.

Running `cake bash check --diagnostic -- 'git status'` against a stub must print
the sensitivity warning, complete effective system and user prompts, request
JSON exactly equal to the body captured by the stub, resolved controls, zero
tools, parsed response metadata, and final verdict. It must not execute `git
status`, create session telemetry solely for the diagnostic, or print a
sentinel API key or provider-secret header.

Loading an old telemetry sidecar and a new sidecar through
`scripts/session-metrics/cakelib.py` must succeed with zero parse errors. Old
invocations expose an empty judge-attempt list; new invocations expose their
attempts; existing API, retry, tool, compensation, and summary aggregation is
unchanged.

## Idempotence and Recovery

All code, test, and documentation edits are safe to repeat. The telemetry
change is append-only at runtime: new records do not rewrite session transcripts
or prior sidecar lines. If telemetry writing fails, the existing best-effort
writer behavior disables telemetry and leaves command execution semantics
unchanged.

Use stub providers and temporary `CAKE_DATA_DIR` locations for validation. Do
not run paid or credentialed provider calls for this issue; #205 owns real
provider benchmarking. If a diagnostic test fails after capturing sensitive
test values, keep those values synthetic and confined to the test process.

If request-builder refactoring changes existing wire snapshots unexpectedly,
stop and compare the old constructed request with the new JSON before updating
any snapshot. Preserve the old request unless issue #202 explicitly requires a
field addition. If telemetry transport becomes entangled with generic tool
results, prefer a small typed operational-event collection over adding raw
fields to compensation serialization.

## Artifacts and Notes

The intended metadata-only record is conceptually:

    {"type":"judge_attempt","attempt":1,"model":"provider/model","api_type":"responses","request_build_ms":0,"request_ms":412,"response_parse_ms":2,"verdict_parse_ms":0,"total_ms":414,"configured_timeout_ms":30000,"history_items":2,"system_prompt_bytes":4200,"user_prompt_bytes":210,"tool_count":0,"tool_choice":null,"status_code":200,"provider_request_id":"req_123","usage":{"input_tokens":900,"output_tokens":60,"total_tokens":960},"termination":{"classification":"completed","provider_status":"completed"},"terminal_class":"verdict"}

No example in this plan is a frozen field-level serialization contract; Rust
types and focused tests remain the authority. The important properties are
complete phase attribution, explicit missing data, and the absence of raw
request content from default telemetry.

## Interfaces and Dependencies

`crate::session_telemetry::JudgeAttemptTelemetry` will be the serializable
provider-neutral metadata for one judge provider call. A new
`SessionTelemetryRecord::JudgeAttempt` variant will add invocation identity and
timestamp at persistence time.

`crate::clients::backend::Backend` will expose request construction and sending
of that exact constructed JSON to the judge while retaining the existing
`send_request` interface for the agent runner.

`crate::clients::judge::JudgeClient` will expose an observed evaluation result
containing one attempt and optional in-memory raw diagnostic data. Existing
callers that only need `Result<JudgeVerdict, JudgeError>` may continue through a
thin wrapper.

`crate::clients::tools::ToolResult` and `ToolError`, or one narrowly typed event
carrier shared by them, will transport judge attempts across concurrent tool
execution. The agent loop remains the owner that adds session and invocation
identity and appends sidecar records.

No new crate dependency is expected. `serde_json`, `reqwest`, `tokio`, and the
existing telemetry and backend abstractions provide the required behavior.

Revision note (2026-08-11): created the initial self-contained plan after
selecting issue #202 as issue #206's next Ready child and inspecting the current
judge, provider, telemetry, CLI, and security paths.

Revision note (2026-08-11): recorded the implemented request builder, observed
phase state machine, sidecar transport, diagnostic redaction, metrics and
documentation work, plus the complexity-driven observer-module extraction.

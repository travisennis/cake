## Enforce telemetry data-minimization invariants

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

Cake writes a per-session telemetry sidecar under `~/.cache/cake/session-telemetry/{session_id}.ndjson` (or the `CAKE_DATA_DIR` path). `docs/integrations.md` and `docs/security.md` promise that this file holds bounded operational metadata only: no prompts, no assistant text, no tool or provider response bodies, no credentials, and no raw provider-controlled identifiers. Today the code enforces that promise inconsistently, so a sidecar can hold a full provider error body, a raw provider request ID, a raw tool-call ID, an API key echoed into the base URL, and the absolute working directory with the local username in it.

After this change, the invariant holds at the one boundary every record crosses. A reader of a sidecar written by the new build sees:

- provider request IDs and tool-call IDs only as lowercase SHA-256 hex digests;
- provider-supplied text (`error`, `message`, `detail`) capped at 1,024 bytes with a trailing `...[truncated]` marker, cut on a UTF-8 boundary;
- `telemetry_init` with no `working_directory` key at all, and a `settings.base_url` with userinfo, query, and fragment removed;
- the session-metrics suite (`scripts/session-metrics`) still reporting the same tables, sourcing project identity from the transcript when the sidecar no longer carries it.

You can see it working by running a session against a mock provider whose response id and error message contain canary strings, then grepping the sidecar: the digests are present, the canaries are not. Historical sidecars remain readable because nothing in Cake ever reads a sidecar back; the Python metrics reader tolerates both shapes, which its tests will prove.

## Progress

- [x] (2026-10-15T13:05Z) Branch `fix/telemetry-data-minimization` cut from `origin/master`; task 077d accepted and started.
- [x] Add serialization-boundary helpers to `src/session_telemetry.rs` and apply the serde attributes.
- [x] Remove construction-time digesting from `src/clients/judge_observer.rs`; update its tests.
- [x] Change `ToolCallTelemetry.call_id` to `Option<String>` and update the three construction sites.
- [x] Remove `working_directory` from the `telemetry_init` record and its `attach_session_telemetry` parameter.
- [x] Unit tests: digest, bound, endpoint, and cwd canaries.
- [x] Integration tests in `tests/session_telemetry.rs` with canaries end to end.
- [x] Update `scripts/session-metrics` (cwd fallback, `tool_call.call_id` digest) and its tests.
- [x] Update `docs/integrations.md`, `docs/security.md`, `scripts/session-metrics/README.md`.
- [x] (2026-10-15T14:40Z) `just check` and `just check-coverage` pass; preflight passes 1-3 run; commits `d1e8e02`, `50a95fd`, `5d1ac33`.
- [ ] Open the pull request and hand off.

## Surprises & Discoveries

- The recorded task evidence pointed at `src/clients/judge_observer.rs:765-783` for `digest_identifier`; the function actually lives in `src/session_telemetry.rs` and the judge observer only calls it. Evidence: `rg -n "fn digest_identifier" src/session_telemetry.rs`.
- `ResponsesFailedMetadata` is dual-purpose: the same struct instance feeds the user-visible error text (`ResponsesStreamFailed::Display`) and telemetry, so truncating the value at construction would silently shorten user-facing errors. This is the strongest argument for enforcing at the serialization boundary instead.
- Field-level `serialize_with` works under `#[serde(flatten)]` on the record enum's payload structs, which the approach depends on. Evidence: `clients::judge::observer::tests::attempt_identifiers_serialize_only_as_digests` passes, serializing a `SessionTelemetryRecord::JudgeAttempt` whose payload fields are flattened.
- A `serialize_with` adapter cannot make a field *absent*: serde evaluates `skip_serializing_if` against the field value before the adapter runs, so a present-but-empty or unparsable value serializes as `null`. The docs state that shape rather than promising omission, and the empty-identifier construction sites filter the value themselves when absence is the intent.
- The previous implementation decided the policy by hand at each boundary (build time for the judge attempt's call id, `finish` time for its provider id, not at all for a tool call); the drift those paths had already accumulated is why the identifiers are now hashed at one boundary instead. `sanitize_attempt_provider_fields` stays for the termination vocabulary check, which is a separate policy (reject unknown values rather than transform them).

## Decision Log

- Decision: enforce minimization with `#[serde(serialize_with = ...)]` helpers in `src/session_telemetry.rs` rather than at each construction site, and delete the construction-time digest helpers in `src/clients/judge_observer.rs`. Rationale: the serializer is the only boundary every present and future construction path crosses, `ResponsesFailedMetadata` must stay full-length in memory for the user-facing error, and the task requires one documented ownership and hashing policy rather than three. Date/Author: 2026-10-15, cake.
- Decision: `ToolCallTelemetry.call_id` becomes `Option<String>` and the key is omitted for an empty identifier. Rationale: matches the existing `judge_attempt`, `compensation`, and `type_safe_shadow` convention ("an absent or empty identifier attaches nothing") and avoids persisting the digest of an empty string, which would collide across id-less calls. Date/Author: 2026-10-15, cake.
- Decision: delete `telemetry_init.working_directory` rather than replace it, and let `scripts/session-metrics` source project identity from the transcript's `session_meta.working_directory` (the transcript is the documented sensitive artifact and already carries the same path). Rationale: the task forbids another raw absolute path, and the alternatives (basename, path digest) either still disclose the project name or lose the documented `--project` substring filter. Date/Author: 2026-10-15, cake.
- Decision: truncation marker is the ASCII string `...[truncated]`, counted inside the 1,024-byte budget; the reaper walks back to a UTF-8 boundary. Rationale: ASCII keeps the byte budget and `jq`/`grep` handling trivial, and a clear marker beats the bare `...` used for human-facing output elsewhere. Date/Author: 2026-10-15, cake.
- Decision: endpoint metadata strips userinfo, query, and fragment with `reqwest::Url`, then applies the same byte bound; a `base_url` that is not a parsable URL, or whose userinfo cannot be cleared, serializes as `null` rather than the raw string. Rationale: fail closed, because an unparsable value cannot be proven credential-free; the adapter cannot omit a field, so `null` is the absent shape. Date/Author: 2026-10-15, cake. (Revised 2026-10-15: the original entry said "omitted entirely"; the implemented behavior is `null`, and the docs now say so.)
- Decision: widen the bound from the three named fields to every unbounded provider-controlled string in telemetry: `responses_failed` `type`/`code`/`param`, `compensation.detail`, and `session_summary.error`. Rationale: identical adversarial shape and a one-attribute fix; in-memory values (retry classification by `code`) are untouched. Out of scope: provider identity values that downstream analysis matches exactly (`api_attempt.response_model`, `provider`, and `termination` vocabulary), where a silent truncation would create wrong matches; the judge path's vocabulary allowlist owns those, and task #202 owns judge-diagnostic breadth. Date/Author: 2026-10-15, cake.

## Outcomes & Retrospective

Delivered. Every telemetry record now crosses one data-minimization boundary, and a reviewer can see it in three places: the unit tests in `src/session_telemetry.rs` (`metadata_bound_is_utf8_safe_and_marked`, `provider_text_fields_serialize_bounded`, `provider_identifiers_serialize_only_as_digests`, `telemetry_init_omits_working_directory_and_sanitizes_endpoint`), the end-to-end canary test `sidecar_minimizes_provider_identifiers_paths_and_error_text`, and the documented policy in `docs/integrations.md` with `docs/security.md`.

Against the purpose: a sidecar written by this build carries provider identifiers only as digests, bounds every provider-supplied string to 1,024 bytes with the `...[truncated]` marker, keeps the endpoint without userinfo, query, or fragment, and has no `working_directory` key. The session-metrics suite still reports the same tables and filters, taking project identity from the transcript; historical sidecars keep working, which its tests cover (`test_invocation_borrows_transcript_working_directory`, `test_digested_tool_call_id_joins_its_judge_attempt`).

What changed against the plan: the bound widened from the three named fields to every unbounded provider-controlled string (recorded in the Decision Log), the endpoint's unparsable case is `null` rather than omitted, and `--diagnostic` turned out to render attempt metadata through the same `Serialize` implementation, so it keeps showing digests and no raw identifier appears on that surface either. Nothing remains from the original scope; the follow-up worth its own task is bounding provider *identity* strings (`response_model`, `provider`, `termination` vocabulary), which this change deliberately left alone because truncating a value that downstream analysis matches exactly would create silent mismatches.

Lessons: enforcing at the serializer removed the need to reason about every exit path, and the tests that asserted in-memory digests had to move to serialized assertions -- the cost of the design, paid once. The `#[expect(clippy::ref_option)]` attributes are the visible price of serde's `serialize_with` signature.

## Context and Orientation

Cake is a Rust CLI. Two artifacts persist per session: the transcript (`~/.local/share/cake/sessions/{uuid}.jsonl`), which is the content surface and may hold prompts, tool output, and paths, and the telemetry sidecar (`~/.cache/cake/session-telemetry/{uuid}.ndjson`), which is supposed to hold bounded operational metadata only. `docs/integrations.md` section "Telemetry sidecars" and `docs/security.md` section "Sessions, logs, and telemetry" state that contract.

`src/session_telemetry.rs` defines every record: `SessionTelemetryRecord` (a serde-tagged enum with `#[serde(flatten)]`ed payload structs), the payload structs (`ApiAttemptTelemetry`, `JudgeAttemptTelemetry`, `RetryScheduledTelemetry`, `ToolCallTelemetry`, `CompensationEventTelemetry`, `TypeSafeShadowTelemetry`, `ResponsesFailedMetadata`, `SessionTelemetrySettings`), the writer (`SessionTelemetryWriter` and the `SharedSessionTelemetryWriter` fail-stop wrapper), and `digest_identifier`, the single SHA-256 hex helper. Records are written with `serde_json::to_writer`, one JSON object per line, flushed per record; nothing in Cake reads a sidecar back.

Construction sites: `src/clients/agent/agent_loop.rs` builds `ToolCallTelemetry` for every tool call (three sites, including the synchronously blocked path and a test helper); `src/clients/agent_runner.rs` builds `ApiAttemptTelemetry` on four paths (cancellation in `Drop`, accepted-body parse, HTTP failure, transport failure); `src/clients/responses.rs` builds `ResponsesFailedMetadata` from a `response.failed` stream event and wraps it in `ResponsesStreamFailed`, whose `Display` is user-facing; `src/clients/judge_observer.rs` builds `JudgeAttemptTelemetry`; `src/clients/agent.rs::session_telemetry_settings` builds `SessionTelemetrySettings`; `src/main.rs::attach_session_telemetry` builds the `TelemetryInit` record and today passes `current_dir.display().to_string()`.

Reader: `scripts/session-metrics/cakelib.py` loads transcripts and sidecars into `Session` and `Invocation` objects. `Invocation.working_directory` comes from the `telemetry_init` record; `Session.working_directory` comes from the transcript's `session_meta`/`session_start`/`init` record. `--project` filters both; `tokens.py` groups by invocation project. `time_breakdown.py` currently digests a tool-call identifier itself at line 98, which is why changing the serialized field must update that line. `scripts/evals/fake_cake.py` writes synthetic telemetry records for the eval harness.

Terms used below: a *digest* is the lowercase hex SHA-256 of the raw string, produced by `digest_identifier`; a *bound* is a hard cap in bytes applied to a string value, cut on a UTF-8 boundary and marked with `...[truncated]`; the *serialization boundary* is the derived `Serialize` implementation that `serde_json::to_writer` calls, where a field-level `serialize_with` function can transform the value as the last step before it is persisted.

## Plan of Work

In `src/session_telemetry.rs`, add the shared policy vocabulary and helpers next to `digest_identifier`: `METADATA_MAX_BYTES` (1,024), `METADATA_TRUNCATION_MARKER` (`...[truncated]`), `bound_metadata(&str) -> String` (returns the input unchanged when it fits; otherwise cuts on a char boundary so the marker fits inside the cap), `sanitize_endpoint(&str) -> Option<String>` (parse with `reqwest::Url`, clear username and password, drop query and fragment, then bound), and three serde adapters: `serialize_bounded_optional`, `serialize_bounded_required`, and `serialize_digested_optional`, each with the signature serde expects for `serialize_with`. Apply attributes: `ResponsesFailedMetadata.message`/`error_type`/`error_code`/`error_param` bounded; `ApiAttemptTelemetry.error` bounded and `provider_request_id` digested; `JudgeAttemptTelemetry.call_id` and `provider_request_id` digested; `RetryScheduledTelemetry.detail` bounded; `ToolCallTelemetry.call_id` becomes `Option<String>` with
`skip_serializing_if` plus digest; `CompensationEventTelemetry.detail` bounded and `call_id` digested; `TypeSafeShadowTelemetry.call_id` digested; `SessionTelemetrySettings.base_url` endpoint-sanitized; `SessionTelemetryRecord::SessionSummary.error` bounded. Delete the `working_directory` field from the `TelemetryInit` variant and note in its doc comment that historical records may carry it. Update the doc comments of every touched field to state the policy and point at the helpers.

In `src/clients/judge_observer.rs`, delete `digest_optional_identifier` and `digest_provider_identifier` and their call sites, keep `sanitize_attempt_provider_fields` for the termination vocabulary check only, and rewrite the two digest tests to assert on the serialized record (`serde_json::to_value`) instead of the in-memory value. In `src/clients/agent/agent_loop.rs`, pass `Option<String>` for the three `ToolCallTelemetry.call_id` sites, keeping the sibling `ToolRunResult.call_id` raw because hooks, transcripts, and permission labels use it. In `src/main.rs`, drop the `current_dir` parameter from `attach_session_telemetry` and the `working_directory` field from the init record; update the call site.

In `scripts/session-metrics/cakelib.py`, add `Invocation.transcript_working_directory`, make the `working_directory` property prefer the `telemetry_init` value and fall back to the transcript value, and thread a `{session_id: working_directory}` map from `load()` into `load_telemetry`. In `scripts/session-metrics/time_breakdown.py`, stop digesting `tool_call.call_id`, which the sidecar now digests itself. Update `scripts/evals/fake_cake.py` to stop writing `working_directory` into its synthetic init records. Update the fixtures in `scripts/session-metrics/tests` that set a raw `call_id` on a `tool_call` record so they carry the digest the new format writes, and add a test for the cwd fallback.

Documentation: in `docs/integrations.md`, extend the telemetry section with the digest policy (which fields, and that consumers hash the transcript's raw value), the 1,024-byte bound and marker, the endpoint policy, the removal of `working_directory` with historical compatibility, and the parent/child ownership of `responses_failed.provider_request_id`. In `docs/security.md`, state the same invariants in one paragraph. In `scripts/session-metrics/README.md`, note that `tool_call.call_id` is a digest and that telemetry project identity falls back to the transcript.

## Concrete Steps

All commands run from the repository root, `/Users/travisennis/Projects/cake/cake`.

Focused Rust tests while iterating:

```
cargo test --lib session_telemetry
cargo test --test session_telemetry
```

Python reader tests:

```
just session-metrics-check
```

Full fast gate before handoff:

```
just check
```

## Validation and Acceptance

Behavior a reviewer can check:

1. `cargo test --lib session_telemetry` passes with new tests proving: a 1,024-byte ASCII value and a multi-byte UTF-8 value over the cap serialize bounded with the marker inside the cap; a raw call ID, provider request ID, error text, `message`, and `detail` never appear in `serde_json::to_string(&record)`, while `digest_identifier(raw)` does; `base_url` with userinfo (`https://user:sk-canary@host/v1?api_key=sk-canary#frag`) serializes as `https://host/v1`; an unparseable `base_url` serializes as absent; a new `telemetry_init` record has no `working_directory` key; the `responses_failed` nested object carries no `provider_request_id` while the parent `api_attempt` carries its digest.
2. `cargo test --test session_telemetry` passes with an end-to-end test: a mock provider returns a tool call whose id and a `response.failed` error message contain canary strings; the sidecar written by the run contains the digest and the bounded text but neither canary, and `telemetry_init` has no `working_directory`.
3. `just session-metrics-check` passes, including the updated fixtures and the new cwd-fallback test.
4. `just check` passes (fmt, strict clippy, all-feature tests, complexity ratchet, script suites, docs lint).

## Idempotence and Recovery

Every step is a source edit plus a test run, so re-running is harmless. Nothing in the change rewrites existing sidecars: historical records keep their raw `working_directory` and raw identifiers, and the readers tolerate both shapes, so a rollback (revert the branch) leaves old files valid and new files readable by the old reader, which only ever adds tolerance for a missing key. If a test fixture turns out to depend on a raw identifier that the change removes, the fix is to update the fixture to the digest form, not to relax the invariant.

## Artifacts and Notes

`cargo test --test session_telemetry sidecar_minimizes`:

```
running 1 test
test sidecar_minimizes_provider_identifiers_paths_and_error_text ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 17 filtered out
```

`just check` tail:

```
Script fixture suites passed!
Fast local checks passed!
```

`just check-coverage` tail:

```
session_telemetry.rs  1520  31  97.96%  ...
Coverage: 95.76%
PASS: Total coverage (95.76%) meets threshold (90%)
CRAP gate: 1224 functions checked, 1 grandfathered, 2 warned, 0 over allowed
CC gate: 1224 functions checked, 13 grandfathered, 0 over allowed
PASS: All coverage gates passed
```

The end-to-end canary run also proves the transcript side is untouched: `turn_usage.provider_request_id` still names the raw provider response, which is what the transcript is for.

## Interfaces and Dependencies

New in `crate::session_telemetry`: `pub const METADATA_MAX_BYTES: usize`, `const METADATA_TRUNCATION_MARKER: &str`, `pub fn bound_metadata(value: &str) -> String`, `pub fn sanitize_endpoint(value: &str) -> Option<String>`, and the private serde adapters `serialize_bounded_optional`, `serialize_bounded_required`, `serialize_digested_optional`, `serialize_endpoint_optional`; `pub fn digest_identifier(value: &str) -> String` stays as the single hash. `reqwest::Url` (already a dependency, re-exporting the `url` crate) does URL parsing; `serde` field attributes `serialize_with` and `skip_serializing_if` carry the policy; no new dependency is added. `ToolCallTelemetry.call_id` changes type to `Option<String>`, which is the only field-type change; `SessionTelemetryRecord::TelemetryInit` loses `working_directory`, which is the only removed field. `scripts/session-metrics/cakelib.py` gains `Invocation.transcript_working_directory: str | None` and an extra `load_telemetry` parameter with a
default, so the module stays importable by the standalone scripts.

## Revision Notes

- 2026-10-15: recorded the `#[serde(flatten)]` and `null`-versus-absent discoveries, corrected the endpoint decision to match the implemented behavior, and filled in Outcomes, Artifacts, and Progress before task completion.

## Compact automatically before provider requests

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

This is the stage plan for task 034g (milestone M3 of the parent plan at `docs/exec-plans/active/context-compaction.md`). The governing decision is `docs/adr/047-compact-model-context-with-self-contained-append-only-checkpoints.md` (ADR 047). The parent plan defines the feature; this plan defines the code changes, decisions, and proof for automatic pre-request compaction. The plan was reviewed and accepted on 2026-11-15; implementation proceeds by milestone.

## Purpose / Big Picture

After this change, a Cake run with `[compaction]` enabled can survive a long tool loop that would otherwise fill the model's input window. Immediately before each provider request, Cake estimates the projected request. When the estimate reaches the configured trigger, Cake replaces the oldest complete tool exchanges with a self-contained checkpoint: a bounded summary plus the recent tail and exact pins. The next request sends the new summary, the retained tail, and the current invocation's context instead of the retired history.

The original session stays append-only and auditable. Raw history and its offsets never move, so tool attribution, partial results, and the current user input keep their meaning. A resumed or forked session restores the newest checkpoint even when automatic creation is disabled, including on the Codex backend. For sessions without a checkpoint, omitting `[compaction]` or setting `enabled = false` preserves today's behavior exactly.

A user can see it working without reading code. In a text run, a long task prints a progress line when it compacts; the next provider request carries the summary and retained tail; the task finishes with every tool side effect executed once. `cake replay` shows one `compaction_start`, one `compaction`, and one `compaction_end` in physical order. Resuming the session with compaction disabled still starts from the checkpoint. No paid model calls are needed for any part of this stage.

## Progress

- [x] (2026-11-15) Read task 034g, the parent ExecPlan M1-M3, ADR 047, ADR 062/063, the initiative handoff, and the prerequisite deliveries: 034a (`4e2474c`), 034b (`48b8775`), 034e, 034f (`07e4bb7`), 147 (`96d9a31`), and 182 (`d7d67b2`).
- [x] (2026-11-15) Inspected the current compaction seam, `ConversationState`, `Agent::send`/`complete_turn_in_mode`, the settings overlays, `SessionWriter` health, session restore/fork, record/replay vocabulary, and the 034a tail-only test.
- [x] (2026-11-15) Drafted this stage ExecPlan; no Rust, settings, CLI, or user-documentation behavior changed.
- [x] (2026-11-15) Folded five review findings into the plan: request-validated image selection, per-attempt writer health, conservative restored-image evidence, exhaustive selection bounds, and explicit invocation pins across checkpoints. No implementation has started.
- [x] (2026-11-15) Review verification: `cargo test compaction --offline` passed 65 tests; the image-budget checks passed 22 tests, with three localhost tests rerun outside the sandbox after port-binding denials; documentation checks and `git diff --check master...HEAD` passed. The review changed no files.
- [x] (2026-11-15) Landed the first provenance slice on `feat/automatic-compaction`: `ConversationState` keeps a side table aligned with raw history, captures live origin/task/skill/replay/image evidence in the push helpers, starts restored image evidence unknown, and exposes oldest-first retirable raw candidates. That slice left preparation-source construction, pin normalization, checkpoint activation, and retained-image clearing for the next slice.
- [x] (2026-11-15) Landed the second M2 slice: shared `InvocationPin`/`ImageConsumption` types and `PinReason::{SyntheticInput,DeveloperContext}`; `preparation_source` builds projector-aligned records, source refs, current-user index, invocation pins, and image states; normalization clears stale exact pins and applies current user, synthetic, Developer, skill, and image evidence; activation and restore seed retained image state; `drop_images` clears retained pixels and pins; `mark_images_consumed` updates raw and retained evidence. `prune_request_with_plan` and accepted-response wiring remain.
- [x] (2026-11-15) Second-slice verification: `just check` passed with 2,280 all-features tests; the CC gate checked 2,380 functions with none over, and the import, dependency, module-size, domain-glossary, and script fixture gates passed. Focused suites: `agent_state` 39, `types::compaction` 25, `clients::compaction` 29.
- [x] (2026-11-15) Landed the third and final M2 slice: `prune_request_with_plan` returns the admitted history, image plan, and owners; `fully_emitted_items` excludes any item with a partial omission. `request_source_refs` shares the preparation projection order without cloning records, and `TurnResult.consumed_sources` carries emitted source refs from the request that produced the response. `Agent::send` marks images consumed only after `termination::classify` accepts the turn and after the context-pressure discard return. `prune_request` remains the plan-free wrapper.
- [x] (2026-11-15) Third-slice verification: `just check` passed with 2,284 all-features tests; the CC gate checked 2,384 functions with none over, and the import, dependency, module-size, domain-glossary, and script fixture gates passed. Focused suites: image budget and consumption 26, `agent_state` 39. M2 is complete.
- [x] (2026-11-15) Landed M1: `CompactionSettingsOverlay` joins global and profile settings with independent per-key precedence, low-authority preservation, and unknown-key warnings. `CompactionPolicy`, `ResolvedCompactionPolicy`, and their defaults moved to `src/config/settings.rs`; the client module re-exports both policy types. The policy gained an absolute `trigger_tokens`, a `floor(3 * trigger / 4)` target even without a window, and strict validation against a known window. `Agent::with_compaction_policy` validates the policy, rejects enabled compaction without a window, and checks the summary backend without a network call; `CodingAssistant::run` applies the loaded policy once after session construction and before persistence. `cake debug config` prints the configured policy and the init template documents all five keys.
- [x] (2026-11-15) M1 verification: `just check` passed with 2,292 all-features tests; the CC gate checked 2,394 functions with none over. Focused suites: new compaction settings 4, policy defaults/clamps and absolute trigger, agent policy application 2, backend validation, `debug_config` 20, and init template. M1 is complete.
- [x] Milestone 1: add resolved `[compaction]` settings, validation, diagnostics, and agent policy application.
- [x] Milestone 2: add live provenance, preparation-source construction, pin normalization, and image-consumption tracking.
- [ ] Milestone 3: make trigger, measurement, and retained-tail selection explicit and testable.
- [ ] Milestone 4: call preparation before provider requests, commit and activate checkpoints, emit lifecycle records, suppress failed proactive attempts, and gate on writer health.
- [ ] Milestone 5: prove deterministic continuation, prefix-only growth except at checkpoints, failure behavior, image behavior, usage accounting, and session modes offline.
- [ ] Milestone 6: update configuration, integrations, architecture, init-template, and debug-config documentation; run the full gates and record acceptance evidence.

## Surprises & Discoveries

Observation: the M2 seam is offline and has no production trigger. `src/clients/compaction.rs` exposes `prepare`, `summarize`, `CompactionPolicy`, and `ResolvedCompactionPolicy`; nothing calls them in production, and the module carries `#![allow(dead_code)]`. Evidence: `prepare` returns `NoOp`, `NoFit`, or `Prepared`; `summarize` returns a complete `CompactionData`; `src/clients/mod.rs` exports the module.

Observation: `prepare` does not decide whether compaction should fire. It always measures and tries to select, and its `CompactionTrigger` input only labels the resulting checkpoint. The automatic caller therefore needs a trigger gate, and the current `PrepareOutcome` variants do not expose the estimate needed to decide whether continuing uncompacted is safe. Evidence: `prepare` computes `before_tokens` internally and returns only `NoOp { reason }` or `NoFit { reason }` on failure.

Observation: the configured retained-tail target is resolved but not used by selection. `ResolvedCompactionPolicy.retained_tokens` is computed and validated, while `select_exchanges` uses only `target_tokens` and the minimum reduction. The first candidate that meets the outer target wins, so `keep_recent_tokens` has no effect today. Evidence: `rg -n "retained_tokens" src/clients/compaction.rs` finds only policy construction and validation.

Observation: policy types currently live in `src/clients/compaction.rs`, so `src/config/settings.rs` cannot own a `[compaction]` overlay without a configuration-to-client dependency. The codebase treats configuration and types as lower layers than agent/client orchestration. Evidence: `clients::compaction` already imports `config::settings::ResolvedImagePolicy`; the reverse import would create a cycle.

Observation: `SessionWriter` already has the durable commit and terminal failed state M1 added, but the only health query is `#[cfg(test)] pub(crate) fn is_failed`. `AgentObserver` has a persist callback and a progress callback, but no way for the agent to ask whether the shared writer has terminally failed. Usage persistence failures only warn, so a summary batch or retry can still send after the writer is gone. Evidence: `src/config/session.rs` `WriterState::open_buffer`, `fail`, and `is_failed`; `src/clients/agent_observer.rs` callback types; `AgentRunner::complete_turn` retry loop.

Observation: `Session::last_turn_usage()` returns the newest complete main-agent usage record without regard to a later checkpoint. `restored_client_and_session` and `forked_client_and_session` seed `Agent::with_last_usage(restored.last_turn_usage())`, so a checkpoint-bearing resume would anchor context estimates on the pre-compaction request. Evidence: `src/config/session.rs::last_turn_usage` filters purpose and presence but not position; `src/cli/session_factory.rs` calls it after `with_context_seed`.

Observation: live `ConversationItem` values do not carry checkpoint provenance. `RetainedRecord` has optional `pin`, `source_task`, `origin`, and `skill_read`; `ConversationItem` has none of those, and `FunctionCall`/`FunctionCallOutput` also lose replay declarations in the live vector. A live checkpoint therefore needs a side table aligned with raw history, populated when items are appended. Evidence: `src/types/conversation.rs::ConversationItem`; `src/types/compaction.rs::RetainedRecord`; `SessionRecord::from_conversation_item` reconstructs `arguments_parse_error` but cannot recover replay safety.

Observation: the projector derives the prompt prefix from the leading System/Developer run in raw history, not from the initial message count. Appended Developer context can extend that run, so a stored initial count would make preparation's source refs disagree with `request_context` and misindex the image-owner map. Preparation now shares `prompt_context_prefix_len` with the projector. Evidence: `src/types/compaction.rs::project_context`; `src/clients/agent_state.rs::preparation_source`; `preparation_source_aligns_the_prompt_prefix_with_the_projector`.

Observation: at the start of M2, `apply_pins` added a current-task skill pin but never cleared a `SkillInstructions` or `UserInput` pin left by an earlier task. The same gap applied to synthetic and Developer invocation pins, and `PinReason` had no variant for either. A retained checkpoint written across task boundaries would keep old pins forever, and a text-only Developer message could not be protected at all. Evidence: `pin_skill_read` and `pin_current_user_input` in `src/clients/compaction.rs`; the new `normalize_source_pins` clears stale exact pins and the new `PinReason` variants close the gap.

Observation: `prune_request` returns only the pruned history. It computes an `ImageBudgetPlan` internally but discards it, so the agent cannot learn which image-bearing records were actually admitted into the request. Image-consumption evidence needs the plan plus a map from projected item index to raw or retained record. Evidence: `src/clients/image_budget.rs::prune_request`; `ImageOwner` already identifies item and image positions.

Observation: `complete_turn_in_mode` is the one place that builds the projected request, applies image admission, and calls the runner. `Agent::send` is the loop that decides when the next logical request starts. The pre-request compaction seam belongs at the top of that loop, after the existing limit check and MCP tool refresh, not inside the runner or a backend. Evidence: `src/clients/agent/agent_loop.rs::send` and `complete_turn_in_mode`.

Observation: the session and stream vocabularies already carry `compaction` and main-agent `turn_usage`, but there are no lifecycle record variants. `session_factory` and `replay` handle `Compaction` explicitly; every other record match is exhaustive and will force new variants to be handled. Evidence: `src/types/session.rs::SessionRecord`/`StreamRecord`; `src/cli/replay.rs::emit_records`.

Observation: `ARCHITECTURE.md` currently states the tail-only request growth invariant has no sanctioned exception, and the 034a test's `tail_only_violation` comment says the same. This milestone is the first sanctioned exception and must update both. Evidence: `ARCHITECTURE.md` invariants; `src/clients/agent/tail_only_growth_tests.rs`.

Observation: task 182 replaced image eviction with admission-only request planning. Ordinary requests now call `prune_request` and never rewrite an earlier admitted image; checkpoint preparation still calls `plan_image_budget` in read-only mode on the retained tail. The two planners disagree: `plan_admitted_images` omits the image that would exceed a budget in source order, while `plan_image_budget` omits the oldest unpinned image. With a one-image budget, a checkpoint candidate can preserve a newer pinned image that the next ordinary request omits because an older image was admitted first. ADR 063 supersedes ADR 062's pin and eviction rules, so automatic selection must validate against the next request's admission projection, not the checkpoint planner alone. Evidence: ADR 063; `src/clients/image_budget.rs`; task 182 acceptance notes.

Observation: M2 generates the checkpoint UUID inside `prepare` and does not expose it on no-fit/no-op outcomes. Lifecycle start/end records need one correlation id from the first attempt through the final record. Evidence: `prepare` allocates `checkpoint_id` after measuring and only `PreparedCompaction` carries it.

## Decision Log

- Decision: add `[compaction]` with `enabled`, `threshold_percent`, `trigger_tokens`, `keep_recent_tokens`, and `summary_tokens`, defaulting to disabled, 80, unset, 20000, and 4096. Rationale: those are ADR 047's policy and task 034g's requested keys; absent keys inherit lower-precedence values, and no key has an `"unlimited"` sentinel. Date/Author: 2026-11-15, Travis Ennis.
- Decision: move `CompactionPolicy`, `ResolvedCompactionPolicy`, and their defaults from `src/clients/compaction.rs` to `src/config/settings.rs`, then re-export them from the client module for existing callers. Rationale: settings owns configuration and must not depend on clients; the validator and the agent both consume the same resolved type. Date/Author: 2026-11-15, Travis Ennis.
- Decision: keep the new, restored, and forked session factory policy-agnostic and apply the loaded compaction policy exactly once in `CodingAssistant::run`, after session construction and before persistence attachment. Rationale: all three session modes share one application point, and the factory stays free of model-window and backend validation. Date/Author: 2026-11-15, Travis Ennis.
- Decision: an absolute `trigger_tokens` replaces the percent-derived line when set. The post-compaction target is `floor(3 * trigger / 4)`. The value must be positive and strictly below a known window; it does not enable automatic compaction and does not replace `threshold_percent` as a stored fallback. Rationale: this expresses the validated 350k/400k pair on any supported window without changing the meaning of the window. Date/Author: 2026-11-15, Travis Ennis.
- Decision: enabling automatic compaction without a configured input window is a configuration error before the run starts. Disabled or omitted policy stays valid without a window. Rationale: automatic checks cannot run without a window, and silently doing nothing is worse than a clear failure. Date/Author: 2026-11-15, Travis Ennis.
- Decision: allocate one checkpoint UUID in the caller and pass it into preparation. Rationale: lifecycle start/end, summary usage, and the committed record must share one id even when preparation returns no-fit or fails. Date/Author: 2026-11-15, Travis Ennis.
- Decision: `prepare` returns a `Preparation` wrapper holding the outcome and a `PreparationMeasure` with the before estimate, basis, and trigger result. A separate pure `measure` entry point estimates the projected request without selecting exchanges or applying retirement pins. Rationale: the agent needs an estimate after a failed attempt to decide whether continuing uncompacted is safe, and pins do not change the request's current size; repeating selection would duplicate the rule. Date/Author: 2026-11-15, Travis Ennis.
- Decision: `prepare` enforces the trigger for `CompactionTrigger::Threshold` and returns `NoOp` below the line or when no window is known. Manual and overflow triggers bypass the gate. Rationale: one function owns the trigger rule, while later milestones reuse preparation without a threshold check. Date/Author: 2026-11-15, Travis Ennis.
- Decision: the trigger gate runs before pin evidence, exchange segmentation, or selection. Below the trigger, `prepare` is a pure no-op even when the source has ambiguous legacy provenance or pins that could not fit. Rationale: compaction is not needed yet, and failing a request early over retirement metadata would be a regression. Date/Author: 2026-11-15, Travis Ennis.
- Decision: pin every current-task Developer message and every synthetic message at or after `invocation_start`. Developer context is appended before `send` by hook and output-schema setup, so the side table records the current task id at append time and the pin scan uses `source_task`, not `invocation_start`, for Developer records. Rationale: output-schema and hook context must survive repeated checkpoints even though its raw index precedes the invocation start. Date/Author: 2026-11-15, Travis Ennis.
- Decision: selection retires oldest eligible whole exchanges until the estimated retained tail is at or below `keep_recent_tokens` and the full request is at or below every hard bound, or no eligible exchange remains. Hard bounds are `target_tokens`, the minimum reduction, the checkpoint byte ceiling, and the pinned-image admission check below. Pins may keep the tail above `keep_recent_tokens` only when every hard bound still fits. Rationale: for a 100k window, 10% trigger, and default budgets, the outer target is 7.5k while the retained target is 20k; stopping at the retained target would report no-fit with removable exchanges left. Date/Author: 2026-11-15, Travis Ennis.
- Decision: place the proactive check at the top of the `Agent::send` loop, after `enforce_limits(0, invocation_start)` and `refresh_changed_mcp_tools()`, before `complete_turn_with_image_recovery`. Put the implementation in a new `src/clients/agent/compaction.rs` submodule. Rationale: the seam sees complete tool batches, current MCP tools, hooks already appended, and the current turn mode, while keeping `agent_loop.rs` narrow. Date/Author: 2026-11-15, Travis Ennis.
- Decision: add a provenance side table to `ConversationState` aligned with raw history. It records message origin, source task, skill read, replay safety, and image consumption; `push_user_message` distinguishes real from synthetic input; `push_tool_output` records skill-read and image evidence. Rationale: live checkpoints need the provenance that session records carry, and `ConversationItem` cannot carry it. Date/Author: 2026-11-15, Travis Ennis.
- Decision: build the preparation source from fresh prompt prefix, the active checkpoint's marked summary, the checkpoint's retained records with their provenance, and raw history after the positional boundary. The previous summary is a synthetic user-role record and is the first retirable body record. Rationale: a new checkpoint must summarize the old summary once and preserve retained provenance; projecting through `ConversationItem` would lose it. Date/Author: 2026-11-15, Travis Ennis.
- Decision: normalize invocation pins before selection through an explicit `invocation_pins: &[InvocationPin]` input, where each pin carries a source index and `UserInput`, `SyntheticInput`, or `DeveloperContext`. Add `PinReason::SyntheticInput` for synthetic user-role messages and `PinReason::DeveloperContext` for text-only Developer messages. Pin the current real user input, every current-task synthetic message at or after `invocation_start`, and every current-task Developer message, including hook and output-schema context appended before `send`; clear every `UserInput`, `SyntheticInput`, `DeveloperContext`, and `SkillInstructions` pin whose source task is not the current task. Rationale: role alone cannot represent a text-only Developer pin, and an implicit index list cannot survive activation or be validated. Date/Author: 2026-11-15, Travis Ennis.
- Decision: track image consumption from the admission plan with three explicit states: `Consumed`, `Unconsumed`, and `Unknown`. An image-bearing record counts as consumed only when every image on that record was emitted (kept or downscaled) in a main request whose response was not discarded. Live records start `Unconsumed`; restored raw records with images start `Unknown`; a checkpoint retained record with images and no `UnconsumedImage` pin starts `Consumed` because the checkpoint writer had live evidence when it omitted the pin. Unknown evidence adds a conservative `UnconsumedImage` pin, not a no-fit. Activation seeds retained state from the candidate pins instead of resetting it. Rationale: ADR 047 requires conservative retention when consumption cannot be proved, and rejecting ordinary restored images would disable compaction after resume. Date/Author: 2026-11-15, Travis Ennis.
- Decision: after a provider image-rejection recovery, clear images from the in-memory active checkpoint tail as well as raw history. Do not rewrite the persisted checkpoint; document that a resumed session may encounter the same rejected image until a later recovery stage. Rationale: the immediate retry must not resend the checkpoint's images, but rewriting committed bytes or inventing a new checkpoint on an image error is outside this stage. Date/Author: 2026-11-15, Travis Ennis.
- Decision: add `compaction_start` and `compaction_end` session and stream record variants with bounded status, trigger, size, count, and reason metadata. Success orders start, checkpoint commit, checkpoint emission, activation, then end. A failed prepare or summary emits end metadata without a checkpoint; a failed checkpoint commit emits no end. Rationale: ADR 047's lifecycle contract and the ordering needed for honest stream consumers. Date/Author: 2026-11-15, Travis Ennis.
- Decision: commit and emit the checkpoint through one `stream_record` call. `AgentObserver::stream_record` persists before it emits, and `SessionWriter::append_record` routes a `compaction` record through the durable `commit_checkpoint`. Activate only after that call returns success. Rationale: it gives the required persist-then-emit order without a second duplicate append or a new observer API. Date/Author: 2026-11-15, Travis Ennis.
- Decision: validate the candidate structure in no-session runs before activating it in memory, because there is no writer to validate it. In persisted runs the writer validates again. Rationale: both paths must reject an invalid candidate before it changes the projection. Date/Author: 2026-11-15, Travis Ennis.
- Decision: one proactive failure suppresses further proactive attempts for the rest of that `Agent::send` invocation. Each later request still runs the pure measure step; if the estimate reaches or exceeds the known window, the run fails with the session id instead of sending an over-window request. A new invocation or manual command resets the budget. Rationale: ADR 047's failure-suppression rule, without turning suppression into an unchecked over-window request. Date/Author: 2026-11-15, Travis Ennis.
- Decision: expose `SessionWriter::failure()` and `ensure_healthy()` in production and a writer-health callback on `AgentObserver`, then check that callback at the top of every provider attempt inside `AgentRunner::complete_turn`, not once per compaction operation. Both the main turn and `AgentSummaryCaller` pass the same health handle, so a terminal usage append stops the next summary batch and the next retry. The proactive helper checks health before its enabled/disabled gate so a disabled policy cannot hide a failed writer. Rationale: a terminal append failure must stop the loop before another paid request. Date/Author: 2026-11-15, Travis Ennis.
- Decision: after a successful checkpoint, clear the agent's last usage and last measurement; make `Session::last_turn_usage()` consider only usage records after the newest checkpoint. Rationale: a pre-checkpoint measurement describes retired context; the checkpoint estimate is the new basis until a main response reports fresh usage. Date/Author: 2026-11-15, Travis Ennis.
- Decision: validate every prepared candidate against the exact post-checkpoint request projection using `plan_admitted_images`, the same planner `prune_request` uses, whenever the target model supports images. Every image owned by a pinned source record must be emitted; if admission would omit one, retire the next oldest eligible exchange and retry, or return `PinnedNoFit` when none remains. The admission plan, not `plan_image_budget`, supplies the candidate's `image_plan` and `after_tokens`. Rationale: the checkpoint and request planners choose different images, so a checkpoint can otherwise protect an image the next request never sends. Date/Author: 2026-11-15, Travis Ennis.
- Decision: an image-bearing record with unknown consumption is conservatively pinned, not rejected. `with_restored_context` initializes raw image evidence as `Unknown` and checkpoint-retained evidence from the checkpoint's pins. `activate_checkpoint` seeds retained evidence from the prepared candidate, and `drop_images` clears both raw and retained images and marks the retained records consumed. Rationale: restored records carry no admission history, and ADR 047 prefers retaining unproven pixels over inventing evidence. Date/Author: 2026-11-15, Travis Ennis.
- Decision: `PinReason` gains `SyntheticInput` and `DeveloperContext`. `SyntheticInput` requires a retained user-role message with `origin = synthetic`; `DeveloperContext` requires a retained Developer message and no user origin. `UserInput` remains the real-input pin and accepts an absent origin only for legacy checkpoints. A completed checkpoint carries these pins across activation, and raw records after the boundary keep their raw indices and side-table provenance. Rationale: two successive checkpoints must preserve the exact roles and origins of synthetic corrections and Developer context. Date/Author: 2026-11-15, Travis Ennis.
- Decision: keep the stage's public behavior additive. No new dependency, no tokenizer, no second agent, no background worker, no history rewrite, no change to sandbox/tool/skill/hook authority, and no change to ordinary prompt completion JSON. Rationale: the parent plan's boundaries. Date/Author: 2026-11-15, Travis Ennis.

## Outcomes & Retrospective

Planning outcome (2026-11-15): this stage plan translates parent-plan M3 into implementation-sized work. It records the current seam's gaps (no trigger gate, unused retained target, no live provenance, no writer-health query, no lifecycle records) and the decisions that close them. No Rust, settings, CLI, fixture, or user-documentation behavior has changed. Implementation milestones remain unstarted; the plan stays active until task 034g is delivered and verified.

Review outcome (2026-11-15): a five-finding review tightened the plan before implementation. Candidate image selection now validates against the next request's admission plan and retires more exchanges when a protected image would be omitted. Writer health moved into the provider attempt loop so summary batches and retries cannot spend after a terminal append failure. Restored images with unknown evidence are conservatively pinned instead of rejected, and activation transfers the evidence the checkpoint already carries. Selection continues until the retained target and every hard bound fit, not just the first bound. Invocation pins gained explicit `SyntheticInput` and `DeveloperContext` variants and a concrete source-index input so two successive checkpoints preserve roles, origins, and pins. No code has changed. Review verification: `cargo test compaction --offline` passed 65 tests; image-budget checks passed 22 tests, with three localhost tests rerun outside the sandbox after
port-binding denials; documentation checks and `git diff --check master...HEAD` passed.

## Context and Orientation

Cake is a Rust 2024 binary-only coding assistant. A session is an append-only version-4 JSONL stream. A conversation is a sequence of provider-neutral `ConversationItem` values. Raw history is the ordered record of what happened, and its indices are used for invocation boundaries, provider-turn boundaries, partial results, and tool attribution. A request projection is the ordered `ConversationItem` slice actually sent to the provider; it may differ from raw history when a checkpoint is active.

A compaction checkpoint is one self-versioned `compaction` record containing a summary and a retained tail of complete provider exchanges plus exact pins. The latest checkpoint's physical position in the stream is the retired boundary: conversation records before it are represented by the checkpoint, and records after it are sent as-is. `project_context` in `src/types/compaction.rs` builds the projection. A checkpoint never rewrites earlier bytes and never changes session identity or the format version.

A trigger line is the estimated input size at which automatic compaction runs. Under a percent policy it is `floor(window * threshold_percent / 100)`; under an absolute policy it is `trigger_tokens`. The post-compaction target is `floor(3 * trigger / 4)`. The retained-tail target is `keep_recent_tokens`, clamped to one quarter of the window. A no-fit result means the fixed surface, pins, or bounds cannot produce a valid checkpoint without dropping protected content. A no-op means there is nothing useful to retire or the trigger has not been reached.

An image-bearing record has one of three consumption states: `Consumed` when every image on it was emitted by a main request whose response was not discarded, `Unconsumed` when at least one image was not, and `Unknown` when a restored record carries no admission evidence. Unknown evidence is treated as unconsumed for protection: it adds a conservative pin, not a no-fit. An invocation pin is an exact record the current task may not retire: the real user input (`UserInput`), a synthetic correction or recovery message (`SyntheticInput`), a text-only Developer message for the current task (`DeveloperContext`), current-task skill instructions (`SkillInstructions`), and an unconsumed image (`UnconsumedImage`).

The current implementation status matters. M1 delivered validated checkpoint data, the projector, positional restore/fork, and the durable writer commit. M2 delivered pure `prepare` and bounded `summarize` behind a `SummaryCaller` seam, the content-class estimator, summary usage attribution, and backend cap checks. Task 147 delivered image budgets; task 182 replaced request eviction with admission-only request planning. Task 034a delivered the scripted tail-only prefix test; task 034b delivered termination classification. M3 is the first production caller of compaction.

The key files are:

- `src/config/settings.rs` --- settings structs, global/project/profile overlays, per-field merge, validation, unknown-key warnings, and resolved `LoadedSettings`.
- `src/main.rs` --- model resolution, agent construction, persistence attachment, hook attachment, and the agent turn.
- `src/cli/session_factory.rs` --- new, restored, and forked agent/session construction.
- `src/clients/compaction.rs` --- `prepare`, `summarize`, policy/enum types, selection, and the production summary adapter.
- `src/clients/compaction_tests.rs` --- M2 unit fixtures for preparation and summarization.
- `src/types/compaction.rs` --- checkpoint domain types, the projector, estimator, and validation.
- `src/types/session.rs` --- `SessionRecord` and `StreamRecord` vocabularies.
- `src/config/session.rs` --- session loading, `SessionWriter`, checkpoint commit, writer failure state, and `last_turn_usage`.
- `src/clients/agent.rs` --- `Agent` state, usage settlement, builder methods, and record emission helpers.
- `src/clients/agent/agent_loop.rs` --- the `send` loop, request construction, image admission, tool batches, and turn modes.
- `src/clients/agent/compaction.rs` --- new home for the proactive compaction orchestration.
- `src/clients/agent_state.rs` --- raw history, request projection, checkpoint activation, restore, and push helpers.
- `src/clients/agent_observer.rs` --- stream/persist/progress callbacks.
- `src/clients/image_budget.rs` --- `plan_admitted_images`, `plan_image_budget`, `prune_request`, and plan metadata.
- `src/clients/agent/tail_only_growth_tests.rs` --- the 034a prefix invariant.
- `src/cli/replay.rs` --- physical transcript replay.
- `docs/configuration.md`, `docs/integrations.md`, and `ARCHITECTURE.md` --- shipped behavior and invariants.
- `src/cli/debug/config_report.rs`, `src/cli/init.rs`, and `tests/debug_config.rs` --- configuration diagnostics and template content.

## Plan of Work

### Milestone 1: resolve `[compaction]` settings and agent policy

Add `CompactionSettingsOverlay` to `src/config/settings.rs` with `enabled: Option<bool>`, `threshold_percent: Option<u8>`, `trigger_tokens: Option<PositiveLimit>`, `keep_recent_tokens: Option<PositiveLimit>`, and `summary_tokens: Option<PositiveLimit>`. Add the section to `Settings` and `ProfileSettings`, merge every key independently in `SettingsLoader::merge_settings` and `SettingsAccumulator::apply_profile_overlay`, and resolve it once in `SettingsAccumulator::into_loaded` into `LoadedSettings::compaction: CompactionPolicy`. Keep the section low-authority: it changes cost and request shape but cannot grant paths, execute code, or select a provider. Unknown keys continue to warn through the existing round-trip diagnostics.

Move `CompactionPolicy`, `ResolvedCompactionPolicy`, and the default constants from `src/clients/compaction.rs` to `src/config/settings.rs`; re-export the policy types from `src/clients/compaction.rs` so M2 tests and callers keep compiling. Add `trigger_tokens: Option<u64>` to the policy and `trigger_tokens: Option<u64>` to the resolved policy. `CompactionPolicy::validate(window)` keeps the existing threshold and positive-budget checks, adds the absolute-trigger validation, and computes the effective trigger and target:

```
trigger = configured trigger_tokens, else floor(window * threshold_percent / 100)
target  = floor(3 * trigger / 4)
retained = min(keep_recent_tokens, floor(window / 4))
summary  = min(summary_tokens, floor(window / 16))
```

With a known window, `trigger_tokens >= window` is an error; with no window, the absolute value is accepted but cannot enable automatic checks. `enabled = true` with no known window is an error in the agent construction path. A zero effective trigger, target, retained, or summary budget is an error.

Add `Agent::with_compaction_policy(policy: CompactionPolicy) -> anyhow::Result<Self>` in `src/clients/agent.rs`. It validates the policy against `self.config.model_config.context_window`, stores the validated policy for preparation and the resolved copy for the enabled/window gate, and, when enabled, calls a new public `crate::clients::compaction::validate_summary_backend(&self.config)` that rejects a backend which drops the generation cap (the Codex adapter) and a reasoning allowance that cannot fit the summary cap. This check makes no network call. Apply the policy in `src/main.rs` after the session factory returns and before `attach_persistence`, so new, restored, and forked agents all receive it. Record in the Decision Log that the factory stays policy-agnostic and the single call site applies it.

Extend `src/cli/debug/config_report.rs` with a `CompactionSnapshot` carrying the configured `enabled`, `threshold_percent`, `trigger_tokens`, `keep_recent_tokens`, and `summary_tokens`, and print one `compaction:` line. Add the `[compaction]` comments to the generated `src/cli/init.rs` template after `[images]`. Update the settings, init, and debug-config fixtures that construct `LoadedSettings` or assert the template text.

At the end of this milestone, `cargo test settings`, `cargo test init`, and `cargo test --test debug_config` show defaults, per-key global/project/profile precedence, low-authority preservation, invalid values, unknown-key warnings, template content, and resolved diagnostics. No agent behavior changes yet.

### Milestone 2: carry live provenance and build a preparation source

Add a provenance side table to `ConversationState` in `src/clients/agent_state.rs`, aligned one-to-one with raw history. Each entry records the optional message origin (`User` or `Synthetic`), the optional source task id, an optional `SkillRead`, optional replay safety for a function call or output, and an image-consumption state (`Consumed`, `Unconsumed`, or `Unknown`). Non-image records and the prompt prefix start `Consumed`; live image-bearing pushes start `Unconsumed`; restored raw image-bearing records start `Unknown`. Initialize the table with the prompt prefix, append one entry for every history push, and keep it private to the agent layer; it never becomes a provider item or a session record.

Change the push helpers so provenance is captured at creation. `push_user_message` gains an origin argument or a sibling `push_synthetic_user_message`; the real input in `Agent::send` is `User`, while the semantic-recovery prompt, schema-correction prompt, and image-recovery note are `Synthetic`. `append_developer_context` records a Developer message with no user origin and stamps the current task id on the entry. `push_tool_output` records the tool's replay safety, the known-skill `SkillRead` name and path when `record_tool_results` observed one, and `Unconsumed` when the output carries images. Function-call items record the replay safety already computed by `stream_turn_items` and compute `arguments_parse_error` from the arguments when a retained record is built. Add `mark_images_consumed(&[SourceRef])`; the agent calls it with the admission plan's emitted source references after an accepted main response, but only for a record whose every image was emitted. Add a
`prune_request_with_plan` sibling that returns the applied plan and owner list; keep `prune_request` as a wrapper for callers that only need the history, so emitted slots can be mapped back to raw or retained source references. The side table also records the current task id for every record appended by the current task, including Developer context appended before `send`; restored earlier records remain unknown unless a checkpoint supplies provenance.

Add a `preparation_source` method that returns a struct with the projected source records, prompt-prefix length, current-user source index, explicit invocation pins, per-source image-consumption states, and a source-reference map. Build it in projection order:

- leading System/Developer history as prompt-prefix records;
- when a checkpoint is active, one synthetic user-role record carrying the marked historical summary, then the checkpoint's retained records with their pins and provenance;
- raw history after `checkpoint_boundary`, converted to `RetainedRecord` values with the side-table metadata.

The source-reference map is aligned with those records: `Prompt`, `Summary`, `Retained(index)`, or `Raw(index)`. The `image_consumption` vector is aligned with the same records: `Prompt` and `Summary` are `Consumed` because they carry no images, `Retained(index)` uses the active checkpoint's seeded state, and `Raw(index)` uses the raw side table. The `invocation_pins` vector holds `InvocationPin { source_index, kind }` where `kind` is `UserInput`, `SyntheticInput`, or `DeveloperContext`; the real current user input is also exposed as `current_user_input`. Build the pins by scanning the source refs: `Raw(index)` whose side table says `Synthetic` and whose raw index is at or after `invocation_start` becomes `SyntheticInput`; a Developer-role raw or retained record whose `source_task` is the current task becomes `DeveloperContext`; the raw current-user index becomes `UserInput`. `Retained(index)` pins from the checkpoint are preserved and not duplicated.

Activation is the raw-to-retained mapping boundary. `activate_checkpoint` sets `checkpoint_boundary = history.len()`, so every pre-boundary raw index stops being projected; its protected copy is the `Retained(index)` record, which already carries `PinReason`, `origin`, and `source_task`. The checkpoint record is the durable map: `checkpoint.retained[index]` is addressed by `SourceRef::Retained(index)`, and no raw pre-boundary index is stored or reused. If the real user input is before the boundary, `preparation_source` sets `current_user_input` to that retained source index; if it is after the boundary, it sets the raw source index. Raw records appended after activation keep their raw index and side-table provenance, so no raw index is reinterpreted as a different retained index. A checkpoint read from disk follows the same rule from its retained records alone.

Because `request_context` and `preparation_source` use the same projection order (prompt prefix, summary, retained tail, post-boundary raw history), the `ImageOwner.item` from `prune_request_with_plan` indexes `source_refs` directly. After an accepted response the agent therefore maps each emitted slot to a source record without a second projection rule.

Normalize invocation pins in the source. `apply_pins` first clears every `UserInput`, `SyntheticInput`, `DeveloperContext`, and `SkillInstructions` pin whose source task is not the current task, then applies `current_user_input` and `invocation_pins` from the preparation input. It validates every source index, role, and origin, and sets `PinReason::UserInput`, `PinReason::SyntheticInput`, or `PinReason::DeveloperContext`. A text-only Developer message therefore has an explicit `DeveloperContext` pin; a synthetic user-role message has `SyntheticInput`; neither needs a provider-visible marker. Pin every current-task Developer record, including hook and output-schema context appended before `send`, and every synthetic record at or after `invocation_start` so a schema correction, recovery note, or future mid-invocation developer addition cannot be summarized away. For images, `Consumed` clears any `UnconsumedImage` pin, while `Unconsumed` or `Unknown` adds one. An image-bearing record with
unknown evidence is conservatively pinned, not rejected. Reject a skill-read record whose source task is unknown, and reject an `UnconsumedImage` pin on a record with no images; both remain no-fit conditions rather than invented evidence.

Add checkpoint activation to `ConversationState::activate_checkpoint(&mut self, checkpoint)`. It validates the envelope, sets the active checkpoint and `checkpoint_boundary = history.len()`, clears the last measured request, and seeds the retained image-consumption state from the checkpoint's pins: `UnconsumedImage` becomes `Unconsumed`, an image-bearing record without that pin becomes `Consumed` because the checkpoint writer had live evidence when it omitted the pin, and a record with no images is `Consumed`. Activation must not reset that state to unknown. `with_restored_context` applies the same rule to a checkpoint read from disk and initializes restored raw image-bearing records as `Unknown`, so an ordinary restored image is conservatively pinned rather than rejected. `drop_images` must clear images from the active in-memory checkpoint tail as well as raw history, clear any `UnconsumedImage` pin those retained records carried, and mark both raw and retained cleared records
`Consumed`; document that the persisted checkpoint is unchanged and a resumed session may see the same rejected image until later recovery work.

At the end of this milestone, unit tests in `src/clients/agent_state.rs` and `src/types/compaction_tests.rs` prove that a live source preserves origins, source tasks, skill provenance, replay declarations, current-user and invocation pin source indices, image-consumption states, and the old summary once; stale prior-task pins are cleared; a text-only Developer record, including one appended before `invocation_start`, and a synthetic correction produce `DeveloperContext` and `SyntheticInput`; consumed and unconsumed images produce the expected states; a restored image with unknown evidence is pinned instead of rejected; activation preserves consumed evidence and checkpoint pins; `drop_images` clears retained pixels without rewriting the checkpoint; and the projector returns only the summary, retained tail, and post-boundary records. The single `pin` slot remains sufficient because current construction creates images only on tool outputs; adding images to user or Developer messages would
require a multi-pin representation and is out of scope. Because no released production path writes a checkpoint yet, the new pin variants are an additive vocabulary extension at `CHECKPOINT_VERSION = 1`; the first released writer must treat a later vocabulary change as a version change.

### Milestone 3: make trigger, measurement, and retained-tail selection explicit

Change the preparation interface in `src/clients/compaction.rs`. Add `checkpoint_id: String` to `PreparationInput` and stop generating a UUID inside `prepare`. Add `invocation_pins: &'a [InvocationPin]`, `image_consumption: &'a [ImageConsumption]`, and `supports_images: bool`; the slices are aligned with `source`. Remove `consumed_image_indices`. Add:

```
pub struct PreparationMeasure {
    pub before_tokens: u64,
    pub basis: EstimateBasis,
    pub trigger_tokens: Option<u64>,
    pub trigger_reached: bool,
}

pub struct Preparation {
    pub outcome: PrepareOutcome,
    pub measure: PreparationMeasure,
}

pub fn measure(input: &PreparationInput<'_>) -> anyhow::Result<PreparationMeasure>;
pub fn prepare(input: PreparationInput<'_>) -> anyhow::Result<Preparation>;
```

Both functions build the same projected source and estimate it with the same fixed-request and content-class rules, so the trigger decision and selection cannot disagree. Both also apply the same request image admission when `supports_images` is true, because the trigger and the after-check estimate must see the pixels the next request would actually send. The trigger gate runs first: for `CompactionTrigger::Threshold`, a missing effective trigger is a no-op, and `before_tokens < trigger_tokens` is a no-op with `trigger_reached = false`. Below the trigger, `prepare` does not apply pins, segment exchanges, or report no-fit; those retirement checks run only after the trigger is reached. `measure` is the same projection estimate without pins or selection. Manual and overflow triggers skip the gate and go straight to pin application and selection. A no-op above the trigger (nothing retirable) has `trigger_reached = true` so the caller can report it.

Change `select_exchanges` to honor `keep_recent_tokens` and validate the image candidate against the next request. For each candidate, build the exact post-checkpoint projection: the prompt prefix, a summary placeholder with no images, and the candidate retained records with their original images. Plan that projection with `plan_admitted_images` under the same `ResolvedImagePolicy` and model image support that `prune_request` will use. When the target model does not support images, skip admission and estimate the text-only projection, matching `complete_turn_in_mode`. When admission applies, every image owned by a pinned source record must be emitted. If admission omits one, the candidate is invalid; retire the next oldest eligible exchange and retry. If no eligible exchange remains, return `PinnedNoFit` naming the protected image budget. The admission plan, applied to the projection, supplies `image_plan` and the planned prefix and retained items for `after_tokens`, so the estimate
matches the next request; retained records keep their original images. Compute the retained-tail estimate as `estimate_items_tokens(planned retained) + source_metadata_tokens(retained)` and compare it with `resolved.retained_tokens`. Continue retiring while that tail estimate exceeds the target or any hard bound fails: `after_tokens <= target_tokens`, the minimum reduction, the checkpoint byte ceiling, and pinned-image admission. Stop when both the retained target and every hard bound fit, or no eligible exchange remains. If the tail exceeds the retained target only because the remaining exchanges are pinned, accept it when every hard bound fits; otherwise return no-fit. The full estimate includes the summary allowance, fixed tool/native-schema tokens, source replay metadata, and exact pins. `plan_image_budget` remains for explicit checkpoint callers and its existing tests, but automatic selection uses the admission planner. Manual compaction reuses `prepare`, so it inherits the same
validation.

Extend `MeasuredInput` with the fixed-request token estimate (or an equivalent request revision) so a measurement taken with a different tool surface or turn mode is not reused. `Agent` tracks the projected item count and fixed-token estimate for the request each complete main usage belongs to. A checkpoint activation, tool-surface change, turn-mode change, or model change invalidates the measurement. The content-class estimate remains the fallback for missing, partial, zero, stale, or post-checkpoint usage.

At the end of this milestone, `cargo test compaction` proves absolute and percent triggers, the strict window bound, the retained-tail target, the outer target, minimum reduction, pin overflow, measured versus estimated bases, and the no-op/no-fit distinctions. It includes the 100k-window, 10% trigger, default-budget case where the 7.5k outer target is below the 20k retained target and selection must keep retiring instead of reporting no-fit. It also proves candidate equivalence with `plan_admitted_images` by replacing `src/clients/compaction_tests.rs::compaction_and_request_plan_images_identically`'s checkpoint-planner comparison, a newer pinned image surviving after an older eligible exchange is retired, and no-fit when the older image is itself pinned. Manual and overflow preparation still bypass the threshold gate.

### Milestone 4: integrate the pre-request check and lifecycle

Add `src/clients/agent/compaction.rs` and declare it as a submodule of `src/clients/agent.rs`. It owns one `maybe_compact_before_request(&mut self, turn_mode, invocation: &mut InvocationCompactionState)` async method and small helpers for the model identity, fixed request surface, and preparation input. `Agent::send` calls it at the top of the loop after `enforce_limits(0, invocation_start)` and `refresh_changed_mcp_tools()`, before `complete_turn_with_image_recovery`. `InvocationCompactionState` is a local value with a failed flag, the raw current-user index, the raw invocation-start index, and the current task id; it resets on every `send`. `preparation_source` turns those raw positions plus the side-table provenance into the explicit `InvocationPin` list on every turn, so a synthetic correction or Developer message appended mid-invocation is pinned without stashing stale indices. Make `TurnMode` and the tool-definition/output-constraint helpers visible to this sibling submodule, or
pass their derived values explicitly, so the summary estimate uses the exact tool and schema surface of the coming request.

The flow is:

1. Return immediately when no policy is configured. The existing checkpoint still projects; no policy means no compaction behavior and no extra writer query. A configured but disabled policy does not take this early return.
2. When a policy is configured, check writer health before the enabled/disabled gate. A terminal writer failure returns the preserved cause before any provider request or summary spend. A disabled policy still checks health and then returns, so a failed writer cannot hide behind `enabled = false`. `src/main.rs::attach_persistence` sets both the persist callback and the health callback from the same `SessionWriter` clone.
3. Build the preparation source and call `measure` when a previous proactive attempt failed. If the measured input reaches the known window, return an error naming the session id; otherwise continue uncompacted.
4. Otherwise call `prepare` with a fresh checkpoint id, the explicit `invocation_pins` and `image_consumption` slices, and `supports_images = self.config.model_config.supports_images`. The existing top-of-loop `enforce_limits(0, invocation_start)` already prevents a summary when `max_turns` would stop the next turn; a `max_tool_calls` budget that only forbids further tool execution does not prevent a final answer, so it does not by itself suppress compaction.
5. `NoOp` below the trigger: return without records.
6. `NoOp` above the trigger or `NoFit`: emit a `compaction_end` with `no_op` or `no_fit`, set the invocation failure flag, and report text progress. Return from the helper to send the request uncompacted when the measured input is below the known window; otherwise return the actionable over-window error.
7. `Prepared`: emit `compaction_start`; call `summarize` with the production `AgentSummaryCaller`. The adapter sends no tools, no task hooks, and no output schema, so a summary request never reruns a hook. On error, emit `compaction_end` with `failed`, set the flag, and use the prepared before-estimate for the window check. If that end emission itself fails because the writer is terminal, return the writer cause instead of inventing an end record. On success, validate the candidate structure, then call `stream_record(StreamRecord::Compaction(checkpoint))`, which persists through `SessionWriter::commit_checkpoint` and then emits the checkpoint. If that call fails, return the error without an end record. On success, activate the checkpoint in memory, clear `last_usage` and `last_measurement`, emit `compaction_end` with `committed`, and continue the loop. No turn or tool counter changes.

The main request path uses the same source map. Carry the emitted source references (or the `RequestImagePlan`) with the `TurnResult` from the request that produced the accepted response; an image-recovery retry replaces the prior plan so a rejected attempt cannot mark pixels. `Agent::send` calls `mark_images_consumed` only after `termination::classify` accepts the turn, for every record whose every image was emitted. A discarded response marks nothing, so an image stays unconsumed until a later accepted request actually sends it.

Move the writer-health check into `AgentRunner::complete_turn`. Add a `before_attempt` callback invoked at the top of every attempt-loop iteration, before `InFlightAttempt::new` and `send_with_stall_deadline`; an error from it is terminal and sends nothing. Both the main turn and `AgentSummaryCaller` pass the same health handle, so a terminal usage append from one summary batch prevents the next batch, and one from a failed attempt prevents the retry. Store the `AgentObserver` health callback behind a cloneable `Arc` and pass it to `AgentSummaryCaller::new` and the main turn, so neither borrows the observer mutably across the runner call. This covers every outgoing attempt rather than only the first request of the operation. Extend the test-only `SessionWriter` fault hook to fail a normal usage append so both cases are deterministic.

Add lifecycle types to `src/types/compaction.rs`:

```
pub enum CompactionStatus { Committed, NoOp, NoFit, Failed }

pub struct CompactionStartData {
    pub session_id: String,
    pub task_id: String,
    pub checkpoint_id: String,
    pub timestamp: DateTime<Utc>,
    pub trigger: CompactionTrigger,
    pub estimated_before_tokens: u64,
    pub context_window: Option<u32>,
    pub summary_instruction_version: u32,
}

pub struct CompactionEndData {
    pub session_id: String,
    pub task_id: String,
    pub checkpoint_id: String,
    pub timestamp: DateTime<Utc>,
    pub trigger: CompactionTrigger,
    pub status: CompactionStatus,
    pub estimated_before_tokens: Option<u64>,
    pub estimated_after_tokens: Option<u64>,
    pub basis: Option<EstimateBasis>,
    pub retired_count: Option<usize>,
    pub retained_count: Option<usize>,
    pub reason: Option<String>,
}
```

Bound `reason` to a small byte ceiling on a character boundary and never copy a provider body or summary text into it. Add `SessionRecord::CompactionStart/CompactionEnd` and the matching `StreamRecord` variants, conversions, and no-op content-id stripping; they convert to no conversation item and are ignored by `Session::messages`. Update every exhaustive record match in `src/config/session.rs`, `src/config/blob_store.rs`, `src/cli/replay.rs`, `src/cli/session_factory.rs`, `src/cli/persistence.rs`, and `src/types/session.rs`. Replay validates that a lifecycle record names the replayed session and emits it in physical order; live stream-json emits start and end. Fork seeding does not copy lifecycle records; they describe an operation in the source session, not new-session context.

At the end of this milestone, `cargo test clients::agent` proves the success order, failure end metadata, no end after a commit failure, no summary spend after a terminal writer failure between batches or between retries, no provider request after a terminal writer failure when the policy is disabled, no-session in-memory activation, and unchanged behavior when the policy is omitted or disabled. `cargo test replay` proves lifecycle records replay in physical order and are not projected.

### Milestone 5: prove deterministic continuation offline

Extend `src/clients/agent/tail_only_growth_tests.rs`. Keep the existing exact prefix assertion for every request pair without an intervening committed checkpoint. Add the one sanctioned exception: a request immediately after a committed checkpoint starts a new projection, and every later request extends that projection exactly. Cover hooks, schema correction, retries, and two checkpoints in one invocation. Update the module comment and `ARCHITECTURE.md`.

Add deterministic agent fixtures using the existing wiremock scripted provider, with summary responses served by the same local mock. The mock distinguishes a summary request from a main request by its summary instruction and empty tool list, and returns fixed text that retains the facts the fixture needs to finish. Cover:

- one long user input that crosses the threshold after large tool results, compacts, finishes, and executes every tool side effect exactly once;
- the absolute trigger line, the three-quarter target, and the retained-tail target, including the 100k-window/10%-trigger case where the outer target is below the retained target and selection must exhaust eligible exchanges;
- twenty successive small tool turns near the threshold, each successful checkpoint restoring the specified headroom;
- one failing proactive summary followed by growing tool content, proving the failure is not retried in that invocation, and the run still fails before an over-window request;
- a no-fit caused by pins, with no summary call and no checkpoint;
- candidate image equivalence: a newer pinned image behind an older unpinned image is preserved by retiring the older exchange, the candidate's plan equals `plan_admitted_images` on the actual next projection, and a pinned older image with no eligible exchange returns no fit;
- a schema-correction turn and an image-recovery note preserving their synthetic origin and staying pinned while the real user input is preserved independently;
- two successive checkpoints containing a synthetic correction and a Developer context, including output-schema context appended before `send`, with `SyntheticInput`/`DeveloperContext` pins, roles, and origins unchanged after each activation;
- a checkpoint-bearing session resumed with automatic policy enabled, where a restored image with unknown evidence is conservatively pinned and compaction still prepares instead of rejecting the source, then becomes retirable after an accepted request emits it;
- exact hook context, output-schema developer context, and current-task skill results surviving repeated checkpoints;
- a checkpoint-bearing session resumed and forked with automatic policy disabled, including a Codex configuration that makes zero network calls;
- main-agent `turn_usage` records still emitted after lifecycle records, with the first post-checkpoint request reporting the reduced context;
- a terminal writer failure between turns preventing the next provider request, between summary batches preventing the next batch, and between summary retries preventing the retry; a disabled policy still fails before its request;
- image admission and consumption: a new tool-result image is pinned until an accepted request emits it, an admitted image may be retired by a later checkpoint, an omitted image stays pinned, and a pinned image that cannot fit returns no fit without dropping it.

Update `tests/session_modes.rs` for the restored/forked checkpoint path and the last-usage reset. Add record snapshots for `compaction_start` and `compaction_end`, and a replay fixture that shows start, checkpoint, end in physical order without duplicate retained events.

At the end of this milestone, the focused commands pass with zero external network or paid-model dependency. The fixtures record request bodies and side effects, not summary claims.

### Milestone 6: document and close the stage

Update `docs/configuration.md` with the `[compaction]` section after Images: the five keys and defaults, per-key global/project/profile precedence, low-authority status, the absolute trigger and its strict window bound, the window's input-token meaning, the post-compaction target, the retained-tail target, the summary target, the opt-in default, the backend restriction, and the disabled-policy restoration rule. Update `docs/integrations.md` with the `compaction_start`/`compaction_end` records, their fields and bounded metadata, success/failure ordering, replay visibility, and the rule that they are audit metadata and never model context. Update `ARCHITECTURE.md` so the tail-only invariant has exactly one sanctioned exception at a committed checkpoint boundary. Update the init template and debug-config text to match.

Run the focused checks while implementing, then the full stage gates. Record the exact commands, pass counts, any skipped platform checks, and the documentation assessment on task 034g. Complete the task before the final implementing commit and record the delivered commit or PR. Do not move the parent plan to completed; M3 is one stage, and M7 qualification remains.

## Concrete Steps

All commands below run from the Cake clone at `/Users/travisennis/Projects/cake/cake`. Start from an up-to-date `master` and confirm the prerequisite commits are present before branching:

```
git fetch origin
git log --oneline -1 4e2474c
git log --oneline -1 48b8775
git log --oneline -1 07e4bb7
git log --oneline -1 96d9a31
git log --oneline -1 d7d67b2
ahm task show 034g
ahm task start 034g
just branch feat/automatic-compaction
```

During implementation, run the focused filters after the relevant milestone:

```
cargo test settings
cargo test init
cargo test --test debug_config
cargo test compaction
cargo test clients::agent
cargo test session
cargo test replay
cargo test --test session_modes
cargo test --test resume_repair
cargo test --test replay
just session-metrics-check
```

Every applicable command must end with `test result: ok` or a zero-exit fixture-suite result, and a new filter must run at least one test; a zero-test match is not evidence. Before the final implementing commit:

```
cargo fmt -- --check
just check
just docs-check
just session-metrics-check
git diff --check
```

For this planning-only branch, validate the new plan with the documentation checks:

```
panache format --check docs/exec-plans/active/automatic-compaction.md
panache lint docs/exec-plans/active/automatic-compaction.md
just docs-check
git diff --check
```

Rust gates are unnecessary for the plan alone. Stage the new plan and the task/parent-plan updates before `just docs-check` so the tracked corpus includes them.

## Validation and Acceptance

With `[compaction] enabled = true` and a known window, a long single user input that crosses the trigger after large tool results completes through deterministic fake summaries and executes every tool side effect exactly once. The request immediately after each checkpoint contains the marked summary, the retained tail, and the current invocation context; it omits every pre-boundary raw record and emits no duplicate retained message, tool event, usage record, or skill activation. The session contains `compaction_start`, `compaction`, and `compaction_end` in that order.

A percent-only configuration behaves as before. An absolute `trigger_tokens` fires at its line and the next request targets no more than three quarters of it; a value at or above the known window fails before the run starts. Enabling compaction without a window fails before the run. Omitting or disabling policy preserves current behavior, and a checkpoint-bearing session still restores on a disabled-policy Codex run.

Twenty successive small tool turns near the threshold each restore the specified headroom. A 100k-token window with a 10% trigger retires below the 7.5k outer target even though the retained target is 20k; the retained target never stops selection while a hard bound still fails. A failed proactive summary or no-fit is reported, commits no checkpoint, and is not retried as the source grows within that invocation; each later request is still measured, and an estimate at or above the window fails with the session id instead of sending. A new invocation starts a fresh budget.

The current user input, hook/developer context, output-schema instruction, schema-correction synthetic message, and current-task skill results survive repeated checkpoints exactly, with `UserInput`, `SyntheticInput`, and `DeveloperContext` pins and unchanged roles and origins. Prior-task invocation and skill pins expire; historical provenance remains audit metadata. An image-bearing record is retired only after an accepted main request emitted every image on it; an unconsumed, partially emitted, or unknown-evidence record stays pinned. A restored image with unknown evidence does not reject the source, and activation preserves the checkpoint's consumed or unconsumed evidence. A pinned image that cannot fit after every eligible exchange has been retired returns no fit without dropping it. Checkpoint selection validates the exact next projection with `plan_admitted_images`; if admission would omit a protected image, it retires more eligible exchanges. Ordinary requests continue to use
admission-only image planning.

The 034a scripted prefix test passes with the committed checkpoint boundary as its only exception. Lifecycle records do not suppress, replace, or reorder main-agent `turn_usage`; summary-purpose usage remains session/telemetry-only; the first main attempt after a checkpoint reports the reduced context. A failed checkpoint commit leaves the file byte-stable, activates nothing, prevents another provider request, and appends no end record. A successful commit followed by stream-output failure remains committed and restores on resume. A terminal writer failure stops the next provider attempt, the next summary batch, and the next retry; a disabled policy still checks health before returning. No summary batch or retry follows that failure.

No test asserts a summary claim in place of an observed side effect or validation result. Sandbox grants, tool selection, skill activation, hook authority, and running `BashSession` identity remain unchanged. No paid model calls are made.

## Idempotence and Recovery

Measurement and preparation are pure and repeatable. A request below the trigger is a no-op and emits no records. A repeated test run uses isolated temporary stores and local mock servers. A failed proactive attempt suppresses further attempts for the rest of that invocation; a new invocation or manual command gets a fresh budget. A no-fit or summary failure leaves the active checkpoint and raw history unchanged.

A checkpoint commit is atomic under the writer lock through serialization, newline, flush, and `sync_data`. Before commit, cancellation leaves the previous context authoritative. During commit, finish the commit section before honoring cancellation. A serialize, flush, or sync failure moves the shared writer to its terminal failed state; cleanup hooks still run but cannot append, and no later provider request occurs. A later locked reopen uses the existing partial-tail repair and validates a complete checkpoint. Never repair a checkpoint by editing old JSONL bytes or discard a complete record because a previous command reported output failure.

If implementation changes a durable decision, create a successor ADR and revise every affected section here and in the parent plan. If a required check is blocked by the environment, record the exact reason and narrower evidence; do not bypass gates or claim completion.

## Artifacts and Notes

Expected observations, not results already achieved:

```
automatic checkpoints in one long invocation: at least 1
summary logical calls per operation: 1 through 4
tool side effects: exactly one per original call
copied-tail events emitted as original events: 0
external provider requests in deterministic fixtures: 0
lifecycle order: compaction_start, compaction, compaction_end
failed commit followed by end record: 0
failed proactive attempt retried in the same invocation: 0
provider attempts after terminal writer failure: 0
protected images omitted by the next admission projection: 0
restored unknown-evidence images rejected as no-fit: 0
```

A successful text run prints one stderr progress line before the next request and continues. A stream-json run emits the lifecycle records and the checkpoint; a replay of the session emits the same records in physical order and never expands the retained tail. `cake debug config --json` reports the configured compaction policy. No provider body, summary text, or credential appears in a lifecycle reason.

Record the baseline commit, implemented commits, exact gate results, and the documentation assessment on task 034g. Record the delivered commit or PR URL after integration.

## Interfaces and Dependencies

Keep `ConversationItem` as the provider-neutral conversation authority and `RetainedRecord` as the checkpoint authority. Add no runtime dependency, tokenizer, model registry, background worker, or second agent.

The configuration surface:

```
// crate::config::settings
pub struct CompactionSettingsOverlay {
    pub enabled: Option<bool>,
    pub threshold_percent: Option<u8>,
    pub trigger_tokens: Option<PositiveLimit>,
    pub keep_recent_tokens: Option<PositiveLimit>,
    pub summary_tokens: Option<PositiveLimit>,
}
pub struct CompactionPolicy {
    pub enabled: bool,
    pub threshold_percent: u8,
    pub trigger_tokens: Option<u64>,
    pub retained_tokens: u64,
    pub summary_tokens: u64,
}
pub struct ResolvedCompactionPolicy {
    pub enabled: bool,
    pub threshold_percent: u8,
    pub retained_tokens: u64,
    pub summary_tokens: u64,
    pub trigger_tokens: Option<u64>,
    pub target_tokens: Option<u64>,
}
impl CompactionPolicy {
    pub fn validate(self, window: Option<u32>) -> anyhow::Result<ResolvedCompactionPolicy>;
}
```

The preparation surface:

```
// crate::clients::compaction
pub struct PreparationInput<'a> {
    // existing fields, plus:
    pub checkpoint_id: String,
    pub invocation_pins: &'a [InvocationPin],
    pub image_consumption: &'a [ImageConsumption],
    pub supports_images: bool,
}
pub struct PreparationMeasure {
    pub before_tokens: u64,
    pub basis: EstimateBasis,
    pub trigger_tokens: Option<u64>,
    pub trigger_reached: bool,
}
pub struct Preparation {
    pub outcome: PrepareOutcome,
    pub measure: PreparationMeasure,
}
pub enum ImageConsumption { Consumed, Unconsumed, Unknown }
pub enum InvocationPinKind { UserInput, SyntheticInput, DeveloperContext }
pub struct InvocationPin { pub source_index: usize, pub kind: InvocationPinKind }
pub fn measure(input: &PreparationInput<'_>) -> anyhow::Result<PreparationMeasure>;
pub fn prepare(input: PreparationInput<'_>) -> anyhow::Result<Preparation>;
pub fn validate_summary_backend(config: &ResolvedModelConfig) -> anyhow::Result<()>;
```

The agent and session surface:

```
// crate::clients::agent
impl Agent {
    pub fn with_compaction_policy(self, policy: CompactionPolicy) -> anyhow::Result<Self>;
}

// crate::clients::agent_state
pub(super) enum SourceRef {
    Prompt,
    Summary,
    Retained(usize),
    Raw(usize),
}
pub(super) struct PreparationSource {
    pub records: Vec<RetainedRecord>,
    pub prompt_prefix_len: usize,
    pub current_user_input: Option<usize>,
    pub invocation_pins: Vec<InvocationPin>,
    pub image_consumption: Vec<ImageConsumption>,
    pub source_refs: Vec<SourceRef>,
}
impl ConversationState {
    pub(super) fn preparation_source(
        &self,
        current_user_raw: usize,
        invocation_start: usize,
        current_task_id: &str,
    ) -> anyhow::Result<PreparationSource>;
    pub(super) fn activate_checkpoint(
        &mut self,
        checkpoint: CompactionData,
    ) -> anyhow::Result<()>;
    pub(super) fn mark_images_consumed(&mut self, refs: &[SourceRef]);
}

// crate::config::session
impl SessionWriter {
    pub fn failure(&self) -> Option<String>;
    pub fn ensure_healthy(&self) -> anyhow::Result<()>;
}
impl Session {
    pub fn last_turn_usage(&self) -> Option<Usage>; // only after the newest checkpoint
}

// crate::clients::agent_observer
impl AgentObserver {
    pub(super) fn set_writer_health_callback(
        &mut self,
        callback: impl Fn() -> anyhow::Result<()> + Send + Sync + 'static,
    );
    pub(super) fn ensure_writer_healthy(&self) -> anyhow::Result<()>;
}

// crate::clients::image_budget
pub struct RequestImagePlan<'a> {
    pub history: Cow<'a, [ConversationItem]>,
    pub plan: ImageBudgetPlan,
    pub owners: Vec<ImageOwner>,
}
pub fn prune_request_with_plan<'a>(
    history: Cow<'a, [ConversationItem]>,
    policy: &ResolvedImagePolicy,
) -> anyhow::Result<RequestImagePlan<'a>>;

// crate::clients::compaction
impl<'a> AgentSummaryCaller<'a> {
    pub(super) fn new(
        config: &ResolvedModelConfig,
        session_id: Uuid,
        task_id: Uuid,
        telemetry: &'a mut (dyn FnMut(AgentRunnerTelemetryEvent) + Send),
        settle: &'a mut (dyn FnMut(TurnUsageSettlement) + Send),
        health: Arc<dyn Fn() -> anyhow::Result<()> + Send + Sync>,
    ) -> anyhow::Result<Self>;
}

// crate::clients::agent_runner
// Every complete_turn loop checks this before constructing or sending an attempt.
pub(super) type AttemptPreflight<'a> = &'a mut (dyn FnMut() -> anyhow::Result<()> + Send);
```

The lifecycle surface:

```
// crate::types::compaction
pub enum CompactionStatus { Committed, NoOp, NoFit, Failed }
pub enum PinReason {
    UserInput,
    SyntheticInput,
    DeveloperContext,
    SkillInstructions { name: String, path: PathBuf },
    UnconsumedImage,
}
pub struct CompactionStartData { ... }
pub struct CompactionEndData { ... }
// crate::types::session
SessionRecord::CompactionStart(CompactionStartData),
SessionRecord::CompactionEnd(CompactionEndData),
StreamRecord::CompactionStart(CompactionStartData),
StreamRecord::CompactionEnd(CompactionEndData),
```

The production summary adapter remains `AgentSummaryCaller`; no deterministic test hook is required because the local wiremock provider serves both main and summary responses. If a focused unit test needs an in-process summary fake, add a `#[cfg(test)]` scripted caller in `src/clients/agent/compaction_tests.rs` rather than changing the production trait. The lifecycle structs use the fields given in Milestone 4; the `...` form in this section is shorthand only.

Revision note (2026-11-15): initial stage plan for task 034g. It records the settings shape, the policy move, absolute-trigger semantics, the preparation report, live provenance and source construction, image-consumption tracking, lifecycle records, writer health, measurement reset, pre-request integration, and the offline proof. No implementation has started.

Review revision (2026-11-15): folded five review findings into the plan. Candidate image protection is validated with `plan_admitted_images` against the exact next projection; writer health is a per-attempt precondition inside `AgentRunner::complete_turn`; restored images with unknown evidence are conservatively pinned instead of rejected; selection continues until the retained target and all hard bounds fit; and `PinReason::SyntheticInput`/`DeveloperContext` plus explicit `InvocationPin` values preserve invocation context across successive checkpoints. No implementation has started.

# Continue long tasks through append-only context checkpoints

This ExecPlan is a living document maintained per `docs/workflow/exec-plans.md`. Keep Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective current. Planning parent: ahm task 034. Document authoring: task 034c. The governing proposal is `docs/adr/042-compact-model-context-with-self-contained-append-only-checkpoints.md` (ADR 042).

## Purpose / Big Picture

After this work, a compaction-enabled Cake run can continue a tool-heavy task after old conversation would otherwise exhaust the model's input window. It preserves the user's current request, a summary of retired work, and complete recent tool exchanges. The original session remains an audit transcript; resume and fork restore the same reduced context without repeating completed tools.

A deterministic local provider fixture will force several compactions during one user request, finish with a recognizable answer, and show every original tool invocation exactly once. The resulting session can be replayed, resumed, and forked without sending retired history back to the model. A user can also run `cake sessions compact <UUID> --json` to compact a session in place without starting another assistant turn.

This is a plan for future implementation, not implementation delivered by task 034c. ADR 042 is proposed. Settle its review before starting production changes. All milestones below belong to scoped implementation tasks under 034; the parent is not an executable task. Preserve the current tree's behavior until each milestone's explicit compatibility changes land.

## Progress

- [x] (2026-10-12) Read task 034, companion tasks 034a/034b, current workflow, source owners, and the October 9 research findings; created documentation branch and planning child 034c.
- [x] (2026-10-12) Drafted proposed ADR 042 and this implementation plan.
- [x] (2026-10-12) Completed authoring preflight and Markdown/whitespace checks for task 034c; task linkage and the final documentation commit follow these records.
- [x] (2026-10-12) Folded three review decisions into the proposal: a content-class estimator, no session format bump (the checkpoint is a format-4 record kind), and a positional resume boundary.
- [ ] Accept ADR 042 and prepare implementation children and dependency order under task 034.
- [ ] M1: implement validated checkpoint types, format-4 record persistence, and the positional projection.
- [ ] M2: implement bounded selection, estimation, summary calls, and usage attribution behind the compaction interface.
- [ ] M3: integrate settings and pre-request compaction, plus task 034a's prefix invariant.
- [ ] M4: integrate task 034b's pressure classification and one compact-and-retry guard.
- [ ] M5: deliver manual compaction, resume/fork/replay behavior, and user documentation.
- [ ] M6: verify deterministic end-to-end continuation, integration checks, and parent acceptance; archive the completed plan.

## Surprises & Discoveries

The base inspected for this plan is `9e43f8875002c6843aa1fe34ab766a5f12475996`. Task 034 is already Tracking; 034a and 034b are existing Pending children. GitHub issue 343 is historical provenance only. The older context-management work is task 004, which includes broader tool-output scope; do not silently complete it with this feature.

Raw-history offsets are functional state, not just debugging data. `Agent::send` records `invocation_start` and `provider_turn_start` in `src/clients/agent/agent_loop.rs`; later code uses them for partial results and termination handling. Replacing or shortening that raw vector would change attribution.

`Session::messages()` in `src/config/session.rs` currently filters records one at a time. Fork seeding in `src/cli/session_factory.rs` copies only records with a `to_conversation_item()` result and skill activation records. Both paths would lose a checkpoint unless explicitly changed. `Session::last_turn_usage()` currently accepts the most recent usage from any attempt and has no purpose or projection association.

`SessionWriter::append_record` flushes a buffered locked file but does not sync it. Checkpoint durability therefore needs a narrow sync path. Because the session format version stays 4, replay, browsing, discovery, and full loading keep their existing header check unchanged; only the record vocabulary grows.

Cake has no token estimator and ships no tokenizer. `last_usage` is the provider's count of the request already sent, and `context_remaining_tokens` deliberately reserves a buffer instead of estimating the unmeasured delta. Every automatic decision (trigger, retained-tail size, summary-prompt size) needs an estimate of unmeasured content, so one pure content-class function supplies it. Inline images arrive as base64 data URL `ImagePart` values whose provider token cost tracks image dimensions, not encoded length, so bytes alone cannot size them.

The retired boundary cannot be a stored index. Provider call IDs repeat after completed exchanges, so retired records cannot be matched by identity, and fork re-copies records through a filter, so a stored ordinal would drift. The latest `compaction` record's physical position in the append-only stream is stable across resume and fork, so the boundary is positional. Because `SessionRecord` is internally tagged by `type`, a reader that predates the record kind hard-errors on a checkpoint-bearing file rather than misreading it, which is the accepted posture for older binaries.

The research does not prove summary quality on Cake's configured models. The default remains opt-in. Deterministic tests prove continuation mechanics and bounded failure, while semantic quality requires a later explicitly authorized evaluation with the user's configured model.

## Decision Log

On 2026-10-12, Codex drafted the following proposed decisions for Trav's review: self-contained checkpoint tails rather than retained-entry pointers; unchanged raw-history offsets with a separate request projection; exact current user input and whole provider exchanges; opt-in threshold policy with bounded summarization; separate summary usage attribution; and a single pressure-recovery guard per invocation. ADR 042 contains their tradeoffs.

On 2026-10-12, Codex first resolved the versioning proposal to new v5 files with v4 read/resume support, so that a v4 session could not gain checkpoints in place and compaction required an explicit fork into v5. A later same-day review reversed this decision (see the entry below) in favor of a self-versioned record kind within format version 4.

On 2026-10-12, Codex scoped task 034c to the two planning documents and task linkage. Implementation children will be allocated after the proposal is accepted, reusing 034a/034b rather than duplicating them. This plan does not change their lifecycle or mark task 034 complete.

On 2026-10-12, Trav's review resolved three open questions. First, unmeasured tokens are estimated by one pure content-class function with a documented safety margin, since Cake ships no tokenizer. Second, the session format version stays 4 and the checkpoint is a new self-versioned record kind; older binaries are not a supported reader, so no v5 migration, fork-to-v5 flow, or dual-format reader is built. Third, the retired boundary is the latest checkpoint record's physical position, not a stored index, and fork re-envelops the checkpoint at its original position so the boundary survives.

## Outcomes & Retrospective

Planning outcome: the ADR and plan specify the proposed persistence, trigger, summary, recovery, accounting, and compatibility behavior. Since the initial draft, three review decisions replaced the v5 session-format bump with an in-place format-4 checkpoint record, specified a content-class estimator, and fixed the resume boundary to the checkpoint record's position; the milestones, acceptance, and recovery sections reflect them. No Rust, settings, fixture, CLI, or provider changes have been made. Implementation milestones remain unstarted, and ADR acceptance remains a review decision. Authoring validation passed `panache format --check` and `panache lint` for both documents, `just docs-check` with both paths staged, and `git diff --cached --check`. Markdown checks were rerun outside the filesystem sandbox after cache permission errors. Rust gates were skipped because this branch changes documentation only. Preflight checked workflow conformance, source ownership/technical consistency,
and scope; it clarified stream commit ordering, partial-usage estimation, and attempt-settlement ownership. Leave this plan active until the feature itself is delivered.

## Context and Orientation

Work inside the Cake clone, `/Users/travisennis/Projects/cake/cake`, not the surrounding workspace or `cake-repl`. All source and document paths below are relative to this clone. Cake is a Rust binary with Chat Completions and Responses adapters, a shared agent loop, trusted hooks, a tool registry, and append-only session JSONL. New code follows the existing dependency direction from CLI to clients/config/prompts/types; configuration and types must not import agent orchestration.

A raw transcript is the ordered record of what actually happened. A request projection is the ordered `ConversationItem` list sent to the provider. A checkpoint is one complete saved summary plus a retained tail. A provider exchange is one assistant response, its associated reasoning, and all the tool calls/results issued by that response. An invocation is one `Agent::send` call opened by the current user input. A request revision changes whenever the source context, prompts, tools, or model change; it prevents a failed proactive attempt or old token measurement from being reused for a different request. A content-class estimate is the pure conversion of unmeasured conversation items into an approximate token count. The retired boundary is the position in the record stream of the latest checkpoint: conversation records before it are represented by the checkpoint, and records after it are sent as-is.

`src/types/conversation.rs` owns provider-neutral conversation items. `src/types/session.rs` owns session/stream records, existing conversation payload structs, `TurnUsageData`, and replay declarations. `src/types/usage.rs` owns normalized counters and usage presence. Put new checkpoint data, the pure content-class estimator, and pure projection validation in `src/types/compaction.rs`, reusing existing payloads; export only what callers need from `src/types/mod.rs`.

`src/config/session.rs` owns session loading, `SessionWriter`, format checks, and tail repair; `src/config/session_jsonl.rs` owns framing, and `src/config/data_dir.rs` checks headers for session discovery. `src/clients/agent_state.rs` owns raw history and incomplete-call repair. `src/clients/agent.rs` owns usage settlement and agent state, and `src/clients/agent/agent_loop.rs` orchestrates requests and tools. `src/clients/agent_runner.rs` owns HTTP attempts; `src/clients/backend.rs`, `src/clients/chat_completions.rs`, and `src/clients/responses.rs` own provider translation. Add the compaction module at `src/clients/compaction.rs` and keep orchestration additions to the already-large loop narrow.

`src/config/settings.rs` owns global/profile overlays and resolution; `src/config/model.rs` defines `context_window` as input tokens. `src/main.rs` resolves runs and wires the persistence callback. `src/cli/session_factory.rs` owns resume/fork, `src/cli/persistence.rs` writes fork seeds, `src/cli/replay.rs` owns physical transcript replay, and `src/cli/sessions.rs` owns session browsing. Add manual-command logic in `src/cli/compact.rs`, dispatched through the sessions command. `src/cli/diagnostic.rs` supplies the existing JSON diagnostic envelope.

`src/session_telemetry.rs` owns per-attempt telemetry. The session metrics consumers under `scripts/session-metrics/` must distinguish summary spend from agent request sizing and avoid counting copied records. Deterministic test surfaces already include `src/clients/agent/agent_tests.rs`, `src/clients/agent_runner_tests.rs`, `src/clients/agent/session_restore_tests.rs`, `tests/session_modes.rs`, `tests/resume_repair.rs`, and `tests/replay.rs`.

## Plan of Work

### M0: accept the proposal and prepare scoped tasks

Review ADR 042's proposed record and fork policy, opt-in settings, fixed operation bounds, and manual command. Once accepted, allocate implementation children with `ahm task create ... --parent 034`, explicit Cake labels, priority, effort, Acceptance Notes, and this plan's path. Assign M1, M2, M3, M4, M5, and M6 to coherent stages; split a milestone if its diff exceeds the repository's 800-line limit, or 500-line limit for complex logic. Record allocated IDs in this plan and task 034. M2 depends on integrated M1; M3 depends on M2 and 034a; M4 depends on M3 and 034b; M5 depends on the common compaction and projection stages; M6 depends on all feature stages. Implement 034a/034b first when useful, using their existing tasks.

Use native `ahm task dep add` relationships, and check each prerequisite's merged commit before starting dependent work from master: Completed in the home store does not mean integrated. Task 004's broader output-management scope remains separate; link the delivered subset without changing its status. Proof of M0 is an accepted ADR, inspectable children and dependencies, and a parent record with the work order. The authoring task does not itself accept the ADR or start these milestones.

### M1: establish checkpoint validation, projection, and persistence

Add a versioned `CompactionData` and a restricted `RetainedConversationRecord` union of existing message/reasoning/call/output payloads. The checkpoint contains UUID and session/task identity, timestamp, trigger, summary, retained records, configured model/config identity, summary instruction version, and estimated before/after input with a basis. It contains no usage aggregate, nested checkpoint, or executable command. Validate checkpoint version, size, nonempty summary, and complete call/output pairing. Preserve execution-time replay snapshots instead of reconstructing them with `SessionRecord::from_conversation_item`, which lacks that metadata. The checkpoint is a new self-versioned `compaction` variant of `SessionRecord` appended to the existing format-4 stream; the session `format_version` stays 4 and no header is rewritten.

Introduce one pure projector that uses the latest validated checkpoint and subsequent conversation records. Both live state and session restore use it. `ConversationState` retains its raw vector and raw offsets, the active checkpoint, and the raw boundary at activation, which is the position of the latest `compaction` record in the record stream. Do not use provider call IDs as globally unique raw-entry identities: IDs may repeat after completed calls. Fresh prompt context is supplied separately. A copied current input is emitted once, in source order, before the selected complete exchanges. The projector drops every conversation record that precedes the boundary, even though the checkpoint's retained tail duplicates some of those records, and it emits the checkpoint's retained tail plus the records after the boundary exactly once each.

Make `Session::context_seed()` return full raw history and the derived positional boundary rather than asking `Session::messages()` to silently become a compacted transcript. Deriving the boundary means scanning records in order and recording the conversation-item count at the latest `compaction` record; the records after it are the `subsequent` items. Run existing incomplete-call validation/repair before planning compaction; persist repair outputs as today. Reject unknown checkpoint record versions. The header `format_version` check and the v4 constant are unchanged in full loading, replay, browsing, and discovery.

Add a checkpoint-specific `SessionWriter` append that holds the existing mutex/advisory lock through serialization, newline, flush, and `File::sync_data`. Wire the observer's checkpoint commit through that operation and return errors instead of the usage path's warning-only behavior. The compaction owner activates only after success. If commit fails, end the invocation; tests must allow that a full checkpoint can exist on disk despite a sync error and prove no following provider call is sent. Reuse existing partial-tail repair on later resume.

Update fork seeding in `src/cli/session_factory.rs`: it currently copies only records whose `to_conversation_item()` is `Some` plus `SkillActivated`. It must also copy the latest `compaction` record, re-enveloped for the new session and placed at its original position among the copied conversation records, so the positional boundary survives. A format-4 source with no checkpoint forks unchanged.

At M1's end, pure fixture tests show that two checkpoints select only the newest summary, fresh prompts survive, raw offsets do not change, repeated call IDs remain valid, and copied tails do not duplicate billing or replay events. Boundary tests show that a resume across two checkpoints uses only the latest, that no pre-boundary raw record is emitted, and that a fork reconstructs the same boundary. A format-4 fixture with a checkpoint still loads, lists, replays, and resumes, and an unknown checkpoint record version fails clearly. Checkpoint write-failure tests inject failures before serialization completion, flush, and sync without real power-loss experiments. Run `cargo test compaction`, `cargo test session`, and the session/replay integration commands in Concrete Steps. No production path compacts yet.

### M2: deliver bounded preparation, summaries, and accounting

Implement `crate::clients::compaction::prepare` and `crate::clients::compaction::summarize` behind the new module's small interface. Preparation takes an immutable source projection with provenance/replay metadata, fresh prompt/tool input, the current invocation inputs, the known input window if any, and resolved policy. It estimates sizes, chooses whole exchanges, and returns a typed no-op/no-fit/prepared result. A fake summary caller and a production caller through the existing runner are the two adapters at the summary-call seam; no new provider library, tokenizer, model registry, or dependency is needed.

Resolve `[compaction]` defaults as disabled, 80% threshold, 20000 retained-token target, and 4096 summary-text target. Validate threshold 10 through 80 and positive values; clamp retained tokens to a quarter of the input window and summary text to a sixteenth. Known-window candidates must be smaller than their source and below threshold with all fixed prompts/tools and exact current input included. Drop retained exchanges whole to fit. If the latest exchange is too large, summarize it rather than cutting its results. Refuse when fixed context or an exact user input cannot fit. With no window, explicit manual preparation uses byte caps and must still show a size reduction; automatic checks are disabled.

The content-class estimator is one pure function in `src/types/compaction.rs`. It starts from complete, positive, model/projection-matched main-request input usage plus an estimate of every item appended since. With partial usage, estimate the full projected request instead: the existing marker does not prove which counters were present, so never infer presence from a zero counter. After compaction or prompt/tool/model changes, estimate the full projected request. Assign a fixed characters-per-token ratio by content class --- prose, JSON and code text, and base64 or opaque replay bytes each converts more densely than prose --- and a conservative flat token cost per inline image, because an image's provider cost tracks its dimensions rather than its base64 length. Add a documented safety margin (for example one quarter) so estimates err toward triggering early. Include tool schemas and opaque replay material conservatively, but do not add cache counters to normalized input again. Use
safe saturating arithmetic, and distinguish estimates from measured counts in records and output. The estimator has no tokenizer dependency.

Serialize retired context as labeled data, including the previous summary once. Keep existing spill paths and bounded head/tail failure excerpts; never read those paths during summarization. Omit encrypted reasoning and image base64 from the summary prompt. Preserve original transcript data and label omission of nontext content; do not claim to have summarized visual details unavailable to the caller. The summary instruction requests goal, constraints, actual progress, decisions, actual verification results, artifacts, and next steps, with clear source roles.

Enforce one MiB per summary prompt, estimated input at most 70% of a known window, at most four logical summary requests, and 180 seconds across all requests/retries. If serialization needs batching, batch oldest complete exchanges sequentially and privately carry the candidate summary forward. Excerpt a giant result before batching; if one exchange still cannot fit, fail explicitly. Never drop an exchange silently or publish an intermediate batch. A summary call sends the conversation's resolved provider/model/effort, no tools, no task hooks, no output schema, and a generation cap at most 16384 and no greater than its configured output maximum. Validate configured reasoning-token allowances against this cap without changing effort. Providers that cannot enforce the bounded request fail; they do not silently receive an uncapped request.

Reject absent/blank text, tool calls, refusal, incomplete or length termination, cancellation, or output over the summary-text target. Check the final serialized checkpoint against eight MiB. A failed batch rejects the whole candidate. The summary caller must not update raw history, normal turn counters, last main-request usage, or schema-correction state.

Extend `TurnUsageData` and `ApiAttemptTelemetry` with optional typed purpose, compaction UUID, and summary-step ordinal. Missing purpose means main agent for historical records. Settle every reported summary attempt once, including failed/retried/discarded attempts, before classification; task aggregate usage includes it once. A summary retry uses attempt ordinal plus summary-step identity so records cannot masquerade as normal agent turns. Update `record_turn_usage` so compaction-purpose attempts never change the context-usage basis. Main `max_turns` and `max_tool_calls` counters exclude summary calls; bounded summary steps/deadline constrain them separately.

At M2's end, scripted callers prove one-request normal compaction, bounded multi-batch compaction, unchanged model/effort, no tools/schema/hooks, preserved failure evidence, exact user input, zero hidden model fallback, and rejection of every partial result. Test missing/partial/zero usage, stale estimates, small windows, oversized fixed prompts, the content-class estimator's ratios and margin on byte-exact fixtures, and total accounting on summary failure. Run `cargo test compaction`, `cargo test agent_runner`, `cargo test usage`, and `just session-metrics-check`; all are offline.

### M3: compact before provider requests without changing raw attribution

Add global/profile field-wise compaction overlays to `src/config/settings.rs`, with the existing settings precedence and unknown-key diagnostics. Pass resolved policy into agent construction in `src/main.rs` and `src/cli/session_factory.rs`. Compaction appends in place on any format-4 session, so no format needs to be rejected; the first automatic compaction still appends nothing and contacts no model when it decides there is no fit. A session that already contains a checkpoint honors it even when automatic policy is disabled, because disabled controls automatic creation, not restoration.

Call preparation immediately before each logical provider request in `Agent::send`, after complete tool batches and hook additions. Hooks keep their existing lifecycle and authority; place the check after context-affecting hooks so their output is sized, and do not execute hooks again merely because a summary request runs. Resolve conflicts with `max_turns`/`max_tool_calls` before spending on a summary that cannot lead to another agent turn. In no-session runs, validate and activate in memory. In persisted runs, commit through M1 before activation. Keep `invocation_start`, provider-turn boundaries, and `TurnMode` keyed to raw history, never projection offsets.

Add stream lifecycle records for compaction start/end and the committed checkpoint. Success orders start, checkpoint commit, checkpoint stream emission, then end, with no duplicate retained events. Persist start/end as audit metadata; they contribute no model context. A prepare failure emits failure end metadata but no checkpoint, and an interrupted process may leave start without end. A later output error does not undo the saved state. Text progress is stderr-only. Suppress repeated proactive attempts for an unchanged failed source revision. After a successful checkpoint, reset measurement eligibility and use the new estimate until a main response supplies fresh usage.

Integrate task 034a's scripted prefix test: between commits, request N+1 extends request N exactly; a committed checkpoint is the explicit exception. Include hooks, correction mode, retries, and multiple checkpoints. Fake summaries are deterministic and should retain fixture facts needed to finish. M3's proof is one long user input crossing the threshold after large tool results, completing without duplicate tool execution, plus unchanged behavior when policy is omitted/disabled. Run `cargo test compaction`, `cargo test clients::agent`, `cargo test settings`, and `cargo test --test session_modes`.

### M4: add one classified context-pressure recovery

Integrate the already-scoped classifier from task 034b alongside `src/clients/retry.rs` and provider termination parsing. It distinguishes genuine input overflow, output-allowance mismatch, recoverable early termination under known context pressure, genuine intended-cap stops, refusal, quota/rate-limit errors, and unknown termination. Preserve actual reported usage even when response items are discarded. A generic length stop or missing output count cannot alone prove pressure. A length-stopped summary always fails regardless of the normal task classifier.

Classify candidate assistant responses before `process_completed_turn`, streaming/persisting their conversation items, or dispatching any returned tools. Preserve the runner's per-attempt accounting. Input-overflow errors get the same typed verdict from both HTTP and Responses failure paths. Keep output-allowance reduction for input that fits; when the input does not fit, propagate to the compaction owner instead of repeatedly lowering output.

Add one pressure-recovery guard for the entire current invocation, shared by classified input overflow and recoverable pressure terminations. It is consumed before starting recovery preparation. With enabled policy and a known window, prepare/commit a checkpoint and retry the intended turn once, preserving output constraint and mode. A second pressure failure, no fit, or summary failure ends the task neutrally with its session ID. A pressure verdict must not also trigger semantic continuation. Existing transport retries remain bounded separately, and output-schema correction counters are never reset by compaction.

At M4's end, fixtures prove that a recovered rejected request has exactly one compaction and one logical retry, discarded responses cause no tool dispatch, usage is retained, genuine cap/refusal/quota failures do not compact, and schema correction remains tools-disabled and constrained. Test an invocation that already compacted proactively and then overflows; it still has only one reactive recovery. Run `cargo test compaction`, `cargo test retry`, `cargo test clients::agent`, and `cargo test agent_runner`.

### M5: complete manual compaction and restore surfaces

Extend `SessionsSubcommand` with `compact <UUID>`, optional `--instructions TEXT`, and `--json`; keep root prompt-run flags invalid with the subcommand. Implement in `src/cli/compact.rs`, reusing existing model/session resolution and prompt construction instead of a second resolver. Acquire the existing session writer lock before loading mutable state and run the same repair/validation and compaction operation. The command works in place on any format-4 session, subject to the writer lock. Emit task boundaries for the standalone operation, account for summaries, and report success/no-op without an assistant continuation or tool execution. Explicit manual use works with automatic policy disabled; it still obeys every byte/request/deadline bound.

Render manual JSON with `DiagnosticDocument`, `command = sessions compact`, session/checkpoint IDs, trigger, status, and explicitly estimated sizes. Do not interleave lifecycle stream records with this one JSON document. Use the existing diagnostic error conventions: input/identity errors 3, operational failures 1, success/no-op 0, interruption 130. An `--instructions` value focuses the summary rather than becoming a command or granting permission. Ordinary prompt completion JSON fields remain unchanged.

Update restore, fork, and replay together. A fork copies original conversation and activation metadata, re-envelopes the latest checkpoint for the new session at its original position among the copied records, and copies all later conversation records in order; it preserves replay metadata and has no references into the source session. The copied conversation records before the checkpoint are retained for audit fidelity, and the checkpoint sits at its original position so the positional boundary is reconstructed. Seed the fork's effective context through the common projector, not a new independent algorithm, and reset main-request usage eligibility because fresh prompts may differ. Copying retained checkpoint content never duplicates raw tool records or billing events. Replay emits the checkpoint once in physical order and never expands retained contents. Readers list, discover, replay, and load the format-4 stream including checkpoint records; a pre-checkpoint Cake binary is not a
supported reader for a checkpoint-bearing file.

Update `docs/integrations.md` with the `compaction` record kind, record and stream semantics, accounting attribution, and exit behavior; `docs/configuration.md` with settings/precedence and opt-in examples; `ARCHITECTURE.md` with projection ownership and the prefix exception; and `docs/security.md` with generated-memory trust and unchanged grants. Update command help and README with manual operation, in-place compaction, and summary-quality limitations. Keep authoritative docs describing shipped behavior until their implementing stage is ready. Add narrow notes to ADRs 004/021/025/026/030 referring to ADR 042's extension rather than rewriting their history.

At M5's end, offline CLI integration tests prove a session without a checkpoint still lists, replays, resumes, and appends unchanged, that manual compaction appends a checkpoint in place on any format-4 session and reports a no-op when there is no new content, that manual compaction makes no tools/continuation calls, that JSON stdout parses, and that restored and forked requests use the newest checkpoint under fresh prompts. A held writer lock prevents all model calls. Run the integration commands below and `just session-metrics-check`.

### M6: prove continuation and close the implemented scope

Add `tests/compaction.rs` with local HTTP fixtures for both provider adapters and isolated session/config roots. It drives a long task through at least two proactive checkpoints, then a pressure rejection/recovery, then completion. It records request bodies and tool side effects, verifies exact original user text and fixture facts, confirms retired raw content is absent from later requests, and checks that every tool side effect happened once. Replay, resume, and fork that same session under fresh prompts and disabled automatic policy; each still restores the latest checkpoint and emits no pre-boundary raw record. All fixtures must have zero external network or paid-model dependencies.

Exercise summary cancellation and failure before commit, partial append/flush/sync failure, failure of the output sink after commit, missing/stale usage, refusal, oversized fixed context, images/opaque reasoning, parallel calls, and repeated call IDs. Byte-for-byte compare existing transcript prefixes before and after successful appends. Confirm bounded summary attempts and reactive recovery, billing totals without double-counted tail records, and stable partial-result attribution for resource-limit stops.

Run focused tests once after the final changes, then `just check`, `just docs-check`, `just session-metrics-check`, and `just check-full` for the completed cross-cutting feature. Complete each implementation child's Acceptance Notes and documentation assessment before its final commit. Preserve platform verification results in the PR handoff; this feature adds no sandbox grant, but ordinary macOS/Linux CI must still pass. Record final outcomes and any narrower task-004 scope delivered on task 034; complete the tracker only after every child and its own acceptance are met. Move this plan to `docs/exec-plans/completed/context-compaction.md` and update task references then, not during authoring.

## Concrete Steps

All commands in this section run from `/Users/travisennis/Projects/cake/cake`. Begin an implementation stage with the allocated child ID replacing `CHILD_ID`:

```
ahm prime
ahm task show 034
ahm task show CHILD_ID
git status --short --branch
ahm task start CHILD_ID
just branch feat/context-compaction-stage
```

Expect an inspectable task, no unsatisfied dependency, and a new branch from fetched master. Use a unique descriptive branch name for each stage. Inspect prerequisite delivery before the branch command. Do not run `start` on task 034 itself. Create task bodies in temporary files and use `--body-file` for ahm mutations; use `ahm task edit CHILD_ID --section "Acceptance Notes" --body-file PATH` for verified outcomes.

For the M1-M5 focused checks, use the named filters and integration targets rather than `cargo test --lib` (Cake is binary-only). After new compaction tests exist:

```
cargo test compaction
cargo test session
cargo test settings
cargo test usage
cargo test retry
cargo test agent_runner
cargo test clients::agent
cargo test --test session_modes
cargo test --test resume_repair
cargo test --test replay
just session-metrics-check
```

Each applicable command must end with `test result: ok` or a zero-exit fixture-suite result, and new filters must run at least one test; a zero-test match is not evidence. Run only the milestone-relevant subset until M6. For final integration:

```
cargo test --test compaction
just check
just docs-check
just check-full
git diff --check
```

Expect successful offline compaction fixtures and every routed gate passing. Record exact failures, skipped platform checks, and fixture request/side-effect counts. Do not run Cake or another coding agent against a paid model as verification. The tests' fake summarizer determines checkpoint content; a live semantic-quality evaluation is separate authorized work.

For this authoring branch alone, validate the two documents with:

```
panache format --check docs/adr/042-compact-model-context-with-self-contained-append-only-checkpoints.md docs/exec-plans/active/context-compaction.md
panache lint docs/adr/042-compact-model-context-with-self-contained-append-only-checkpoints.md docs/exec-plans/active/context-compaction.md
git diff --check
```

Stage the two authored paths before `just docs-check` so the tracked-corpus gate includes them. Rust gates are unnecessary for these documentation-only drafts. The plan remains active and ADR remains proposed after these checks.

## Validation and Acceptance

A successful long-run fixture makes more than one checkpoint inside a single user invocation and reaches the expected final answer without repeating tools. Its provider requests after each checkpoint contain fresh prompts, exact current input once, one summary, and whole retained exchanges; they omit retired raw history. Every non-compaction request extends the prior request's prefix, as enforced by task 034a. Parallel tool call/output associations and Responses reasoning replay remain valid.

A format-4 fixture without a checkpoint still lists, replays, resumes, and appends ordinary records unchanged. A format-4 fixture with a checkpoint resumes and forks to the same effective context, honors existing checkpoints even when automatic creation is disabled, and emits no pre-boundary raw record. A checkpoint commits in place on any session, so no format migration or explicit-fork step is required. Unknown checkpoint record versions and corrupt checkpoints produce errors, never fallback to a partly guessed history. Replay is read-only and emits copied-tail content only inside its one checkpoint record.

Failed, empty, refused, truncated, cancelled, over-budget, or tool-calling summaries commit no checkpoint and leave context intact. A second context-pressure failure does not trigger a second reactive summary. Commit uncertainty stops the process before another provider request. A committed checkpoint remains restored after stream-output failure. Usage fixtures prove summary attempts count once in task totals, including failures, while context measurement and cache-break analysis use only eligible main requests.

Manual compaction returns a recognizable diagnostic JSON document, succeeds or reports a no-op without an assistant/tool continuation, obeys the writer lock, and uses the same configured model/effort as automatic compaction. Sandbox grants, tool selection, skill activation, hook authority, and running BashSession identity remain unchanged. No test may assert summary claims as a substitute for actual side-effect or validation evidence.

## Idempotence and Recovery

Pure estimation, projection, selection, and fixture commands are repeatable. CLI tests use isolated temporary stores. A repeated manual command with no newly compactable content reports no-op instead of appending a second checkpoint for the same revision. A failed proactive attempt is retried only after new source content or policy/request changes, not on every retry of the same request.

Before append, cancellation or model failure leaves the previous checkpoint authoritative. During append, finish the commit section before cancellation; an append/flush/sync error stops all subsequent writes/requests. On restart, the locked writer repairs a torn tail using the existing mechanism; a complete checkpoint is validated and used. Do not repair a checkpoint by editing old JSONL bytes or discard a complete record merely because a previous command reported output failure.

A checkpoint is append-only, so recovery is the existing one: a torn tail is repaired on the next locked open, and a complete checkpoint is validated and used. A failed fork seed follows the existing cleanup policy for the incomplete new file. Compaction never rewrites earlier bytes, so the pre-checkpoint history stays readable in place and there is no format migration to roll back. Disable automatic policy to stop creating checkpoints, while still restoring existing ones.

If implementation changes a durable decision, update proposed ADR 042 before acceptance or create a successor ADR after acceptance, then revise every affected section here. If a required check is blocked by the environment, record the exact reason and narrower evidence; do not bypass gates or claim completion.

## Artifacts and Notes

Keep fixture evidence in tests and concise transcripts in implementing tasks. Expected observations, not results already achieved:

```
compaction checkpoints: at least 2 proactive, at most 1 reactive
summary logical calls per operation: 1 through 4
tool side effects: exactly one per original call
copied-tail events emitted as original events: 0
external provider requests: 0
task usage: sum of reported main and summary attempts, each once
```

Record the baseline commit, implemented child IDs, feature commits/PRs, final gate results, and document assessment on task 034. Existing migration warnings about missing blocked reasons are unrelated backlog metadata and must not be fixed as part of this feature. The October 9 research motivated the design; everything needed to implement it is stated in this plan, without requiring external source reading.

## Interfaces and Dependencies

Keep `ConversationItem` as the provider-neutral conversation authority. New domain types in `crate::types::compaction` are `CompactionData`, `RetainedConversationRecord`, `CompactionTrigger`, and typed estimated-size/basis data. Add a pure `project_context` function returning validated projected items from fresh prompts, an optional checkpoint, and subsequent items, plus a pure content-class `estimate_input` function returning typed estimated tokens. Session restore supplies a typed `RestoredContext` seed with raw history and the derived positional boundary; live requests and forks cross this same projection interface.

The compaction module exposes immutable preparation and bounded summary generation, returning a `CompactionCandidate` or explicit no-op/no-fit/failure. The internal `SummaryCaller` interface accepts a typed summary request and cancellation/deadline context and returns text, termination, and reported usage; one runner adapter and one scripted test adapter implement it. The module writes no checkpoint; the caller's attempt-settlement callback still records billed usage before classification. The owning agent or command calls the existing observer/writer seam to commit, then activates the checkpoint. Avoid a second agent, plugin registry, background worker, or generic memory framework.

`SessionWriter` gains a checkpoint commit operation whose success guarantees newline, flush, and data sync under the existing lock. `TurnUsageData` and telemetry gain optional purpose/compaction/step attribution; existing `Usage` counters and ordinary completion JSON remain their current shape. Introduce a main-request-only context-measurement basis tied to request revision/model, rather than overloading task-total usage or the latest summary usage.

The intended private Rust interfaces below fix ownership and the test surface. Define the named request/result/error types in their owning modules; adapt cancellation to the existing interrupt mechanism without adding a runtime dependency. `CompactionCancellation` carries the operation deadline and existing cancellation signal. The summary adapter invokes the caller-owned usage-settlement callback for each attempt.

```
// crate::types::compaction
pub(crate) fn project_context(
    initial: &[ConversationItem],
    checkpoint: Option<&CompactionData>,
    subsequent: &[ConversationItem],
) -> Result<Vec<ConversationItem>, CompactionValidationError>;

pub(crate) fn estimate_input(items: &[ConversationItem]) -> EstimatedTokens;

// crate::clients::compaction
pub(crate) fn prepare(
    source: &CompactionSource,
    policy: &ResolvedCompactionPolicy,
    trigger: CompactionTrigger,
) -> Result<Preparation, CompactionError>;

pub(crate) async fn summarize(
    prepared: PreparedCompaction,
    caller: &mut impl SummaryCaller,
    cancellation: &CompactionCancellation,
) -> Result<CompactionCandidate, CompactionError>;

// crate::config::session methods; `context_seed` returns raw history plus the
// positional boundary at the latest checkpoint record.
impl Session {
    pub fn context_seed(&self) -> anyhow::Result<RestoredContext>;
}
impl SessionWriter {
    pub fn commit_checkpoint(&self, checkpoint: &CompactionData) -> anyhow::Result<()>;
}
```

No new runtime dependencies, tokenizer downloads, provider/model discovery, tool-result store, archive-reader tool, sandbox grants, or cake-repl changes are required. Existing sandbox/tool limits remain in force. Implementation may change private names while preserving these interfaces' ownership and behavior; record consequential changes in the Decision Log and acceptance tests.

Revision note (2026-10-12): the plan translates the October 9 research into a proposed checkpoint design, native ahm staging, and deterministic delivery milestones. A later same-day review replaced the v5 session-format bump with a self-versioned format-4 record kind, specified the content-class estimator, and fixed the resume boundary to the checkpoint record's position. Only document authoring is complete at this point.

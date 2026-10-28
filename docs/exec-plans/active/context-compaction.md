# Continue long tasks through append-only context checkpoints

This ExecPlan is a living document maintained per `docs/workflow/exec-plans.md`. Keep Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective current. Planning parent: ahm task 034. Document authoring: task 034c. The governing decision is `docs/adr/047-compact-model-context-with-self-contained-append-only-checkpoints.md` (ADR 047).

## Purpose / Big Picture

After this work, a compaction-enabled Cake run can continue a tool-heavy task after old conversation would otherwise exhaust the model's input window. It preserves the user's current request, a summary of retired work, and complete recent tool exchanges. The original session remains an audit transcript; resume and fork restore the same reduced context without repeating completed tools.

A deterministic local provider fixture will force several compactions during one user request, finish with a recognizable answer, and show every original tool invocation exactly once. The resulting session can be replayed, resumed, and forked without sending retired history back to the model. A user can also run `cake sessions compact <UUID> --json` to compact a session in place without starting another assistant turn.

`cake sessions recover <UUID> --json` will create a new fork from raw conversation without checkpoints, provider calls, or tool execution, allowing recovery from an inaccurate summary. Summary creation requires a backend that supports a transmitted output cap; Cake's current Codex backend is excluded, while disabled-policy runs may restore existing checkpoints. Feature completion additionally requires separately authorized real-model quality evaluation. Compaction bounds model context and checkpoint work; retaining full raw history means host RAM and transcript size still grow with session length.

This is a plan for future implementation, not implementation delivered by task 034c. ADR 047 is accepted. All milestones below belong to scoped implementation tasks under 034; the parent is not an executable task. Preserve the current tree's behavior until each milestone's explicit compatibility changes land.

## Progress

- [x] (2026-10-12) Read task 034, companion tasks 034a/034b, current workflow, source owners, and the October 9 research findings; created documentation branch and planning child 034c.
- [x] (2026-10-12) Drafted the proposed ADR and this implementation plan.
- [x] (2026-10-12) Completed authoring preflight and Markdown/whitespace checks for task 034c; task linkage and the final documentation commit follow these records.
- [x] (2026-10-12) Folded three review decisions into the proposal: a content-class estimator, no session format bump (the checkpoint is a format-4 record kind), and a positional resume boundary.
- [x] (2026-10-13) Revised the proposal after design review: protect unseen images and observed skill instructions, terminally disable failed writers, budget serialized bytes before generation, require useful reduction, specify manual pins and raw-history recovery, and require quality qualification.
- [x] (2026-10-20) Accepted ADR 047 as written.
- [x] (2026-10-20) Allocated implementation children 034e-034j with native dependencies; 034d depends on 034j.
- [x] (2026-10-22) M1 (034e): delivered validated checkpoint data, the format-4 `compaction` record, the shared request projector, positional restore and fork boundaries, and the durable checkpoint commit on branch `feat/checkpoint-persistence`. Focused evidence: `cargo test compaction`, `cargo test session`, the persist-callback and writer-failure tests in `src/clients/agent/tail_only_growth_tests.rs`, and the replay unit tests; the child's handoff records the integration targets and full gates. No production path creates a checkpoint yet.
- [ ] M2 (034f): implement bounded selection, estimation, summary calls, and usage attribution behind the compaction interface.
- [ ] M3 (034g): integrate settings and pre-request compaction, plus task 034a's prefix invariant.
- [ ] M4 (034h): integrate task 034b's pressure classification and one compact-and-retry guard.
- [ ] M5 (034i): deliver manual compaction, raw-history recovery, resume/fork/replay behavior, and user documentation.
- [ ] M6 (034j): verify deterministic end-to-end continuation and integration checks.
- [ ] M7 (034d): qualify real-model summary and continuation quality with separately authorized spend; complete parent acceptance and archive the plan.

## Surprises & Discoveries

The base inspected for this plan is `9e43f8875002c6843aa1fe34ab766a5f12475996`. Task 034 is already Tracking; 034a and 034b are existing Pending children. GitHub issue 343 is historical provenance only. The older context-management work is task 004, which includes broader tool-output scope; do not silently complete it with this feature.

Raw-history offsets are functional state, not just debugging data. `Agent::send` records `invocation_start` and `provider_turn_start` in `src/clients/agent/agent_loop.rs`; later code uses them for partial results and termination handling. Replacing or shortening that raw vector would change attribution.

`Session::messages()` in `src/config/session.rs` currently filters records one at a time. Fork seeding in `src/cli/session_factory.rs` copies only records with a `to_conversation_item()` result and skill activation records. Both paths would lose a checkpoint unless explicitly changed. `Session::last_turn_usage()` currently accepts the most recent usage from any attempt and has no purpose or projection association.

`SessionWriter::append_record` flushes a buffered locked file but does not sync it. Checkpoint durability therefore needs a narrow sync path. Because the session format version stays 4, replay, browsing, discovery, and full loading keep their existing header check unchanged; only the record vocabulary grows.

Cake has no token estimator and ships no tokenizer. `last_usage` is the provider's count of the request already sent, and `context_remaining_tokens` deliberately reserves a buffer instead of estimating the unmeasured delta. Every automatic decision (trigger, retained-tail size, summary-prompt size) needs an estimate of unmeasured content, so one pure content-class function supplies it. Inline images arrive as base64 data URL `ImagePart` values whose provider token cost tracks image dimensions, not encoded length, so bytes alone cannot size them.

The retired boundary cannot be a stored index. Provider call IDs repeat after completed exchanges, so retired records cannot be matched by identity, and fork re-copies records through a filter, so a stored ordinal would drift. The latest `compaction` record's physical position in the append-only stream is stable across resume and fork, so the boundary is positional. Because `SessionRecord` is internally tagged by `type`, a reader that predates the record kind hard-errors on a checkpoint-bearing file rather than misreading it, which is the accepted posture for older binaries.

The research does not prove summary quality on Cake's configured models. The default remains opt-in. Deterministic tests prove continuation mechanics and bounded failure; M7 is a required qualification gate before parent completion, with provider spend authorized separately.

The October 13 review found concrete integration hazards. `src/main.rs::emit_failed_agent_turn` runs cleanup hooks and appends completion after an agent error, so merely returning a checkpoint-write error does not stop shared-writer appends. `src/clients/responses.rs::build_request_json` strips the output cap for Codex. Image-bearing tool results can trigger compaction before the next model request, and a flat image token estimate does not bound base64 bytes. `SkillActivated` is emitted only once per session and does not identify every subsequent instruction read. These facts determine the writer state, support check, pin provenance, and byte budgeting below.

M1 implementation on 2026-10-22 settled two interface deviations from the illustrative signatures below. The projector is `project_context(history, checkpoint, boundary)` over the full raw history plus a conversation-item boundary, not `(initial, checkpoint, subsequent)`, and `Session::context_seed()` returns an infallible `ContextSeed { messages, checkpoint, boundary }`, not `Result<RestoredContext>`. Validation runs while a checkpoint is parsed or committed, so projection itself cannot fail. Restore translates the seed boundary after replacing the historical System/Developer prompt prefix (`prompt_len + boundary - first_non_prompt`), and the projector derives the fresh prompt run from the full history, so a boundary of zero or a boundary inside the prompts cannot drop or duplicate them.

`BufWriter` does not necessarily leave a torn prefix when a checkpoint write fails. A record larger than the buffer capacity makes `write_all` bypass the buffer and write the whole record, including its newline, so a flush-stage failure afterwards can leave a complete checkpoint that was never explicitly flushed or synced. A process interrupted during a large write can still leave a torn tail, which the existing locked reopen repairs. M1's failure tests assert the surviving record's completeness instead of assuming the partial record is dropped.

M1 adds no production path that creates a checkpoint: `SessionWriter::commit_checkpoint` exists and the generic append routes `compaction` records through it, but only tests and future milestones call it. The first real commit, the pre-request writer-health check for warning-only usage and hook appends, and the post-checkpoint reset of the last usage measurement land with M3 (034g).

## Decision Log

On 2026-10-12, Codex drafted the following proposed decisions for Trav's review: self-contained checkpoint tails rather than retained-entry pointers; unchanged raw-history offsets with a separate request projection; exact current user input and whole provider exchanges; opt-in threshold policy with bounded summarization; separate summary usage attribution; and a single pressure-recovery guard per invocation. ADR 047 contains their tradeoffs.

On 2026-10-12, Codex first resolved the versioning proposal to new v5 files with v4 read/resume support, so that a v4 session could not gain checkpoints in place and compaction required an explicit fork into v5. A later same-day review reversed this decision (see the entry below) in favor of a self-versioned record kind within format version 4.

On 2026-10-12, Codex scoped task 034c to the two planning documents and task linkage. Implementation children will be allocated after the proposal is accepted, reusing 034a/034b rather than duplicating them. This plan does not change their lifecycle or mark task 034 complete.

On 2026-10-12, Trav's review resolved three open questions. First, unmeasured tokens are estimated by one pure content-class function with a documented safety margin, since Cake ships no tokenizer. Second, the session format version stays 4 and the checkpoint is a new self-versioned record kind; older binaries are not a supported reader, so no v5 migration, fork-to-v5 flow, or dual-format reader is built. Third, the retired boundary is the latest checkpoint record's physical position, not a stored index, and fork re-envelops the checkpoint at its original position so the boundary survives.

On 2026-10-13, Trav chose enforceable summary output caps with initial Codex exclusion, a raw-history recovery fork in this feature, real-model qualification before task 034 completion, and exact pinning of observed skill instruction exchanges during the current task. Engineering refinements add conservative unseen-image pins, terminal shared-writer failure, byte-aware selection, a 60%-of-window target at the default trigger, and invocation-wide proactive failure suppression. ADR 047 records these choices and was accepted as written on 2026-10-20; paid evaluation still requires separate authorization.

On 2026-10-22, M1 delivery recorded the interface deviations named under Surprises through this Decision Log, as the rule below requires. `project_context` takes full history plus a positional boundary and is infallible; `Session::context_seed()` returns `ContextSeed` directly; and restore translates the boundary after the fresh prompt prefix replaces the historical one. Ownership and behavior are unchanged: one pure projector serves live requests and restore, the boundary stays positional, and a checkpoint is validated when parsed or committed. `cargo test compaction` and `cargo test session` pin these interfaces.

On 2026-10-22, M1 deferred three responsibilities to M3 (034g) because M1 has no production caller for them: consulting writer health before a provider request when a usage or hook append only warned; resetting the last usage measurement after a checkpoint restore; and exposing a production writer-health seam instead of the test-only failure query. M1 also adds no production checkpoint creation path.

## Outcomes & Retrospective

Planning outcome: the ADR and plan specify the accepted persistence, trigger, summary, recovery, accounting, and compatibility behavior. Since the initial draft, three review decisions replaced the v5 session-format bump with an in-place format-4 checkpoint record, specified a content-class estimator, and fixed the resume boundary to the checkpoint record's position; the milestones, acceptance, and recovery sections reflect them. No Rust, settings, fixture, CLI, or provider changes have been made. Implementation milestones remain unstarted; ADR 047 was accepted as written on 2026-10-20. Authoring validation passed `panache format --check` and `panache lint` for both documents, `just docs-check` with both paths staged, and `git diff --cached --check`. Markdown checks were rerun outside the filesystem sandbox after cache permission errors. Rust gates were skipped because this branch changes documentation only. Preflight checked workflow conformance, source ownership/technical
consistency, and scope; it clarified stream commit ordering, partial-usage estimation, and attempt-settlement ownership. Leave this plan active until the feature itself is delivered.

The October 13 revision addresses the design-review findings and records Trav's four decisions in both documents. Task 034c was reopened on the existing documentation branch for this follow-up. Task 034 now records the in-place v4 policy and strengthened acceptance; 034b's older below-cap classification rule is reconciled with the requirement for independent pressure evidence. Qualification child 034d depends on planning child 034c and on 034j (M6) for execution. No implementation or real-model evaluation has run. Revision validation passed targeted `panache --no-cache format --check`, `panache --no-cache lint`, `PANACHE_NO_CACHE=true just docs-check`, `git diff --check`, and inspection of all nine relative document links. The first corpus-check invocation used an invalid boolean environment value; correcting it to `true` passed. The combined documentation preflight checked source ownership, failure-path integration, policy consistency, task synchronization, and scope. Rust gates and
live-model evaluation were skipped because this is planning-only work.

On 2026-10-20, ADR 047 was accepted as written and M0 allocated 034e through 034j; 034d is blocked on 034j. This allocation authorizes no implementation or provider spend by itself.

M1 outcome (2026-10-22): task 034e delivered validated checkpoint data, the self-versioned format-4 `compaction` record, one shared projector, positional restore and fork boundaries, and the durable shared-writer commit, with the interface deviations recorded above. Tests cover projection, validation, envelope identity, writer failure at each stage, an oversized retained record, the production persist-callback failure path, and the record's JSON snapshot. No production checkpoint creation, settings, CLI command, user documentation, or provider request changed; M2 is next.

## Context and Orientation

Work inside the Cake clone, `/Users/travisennis/Projects/cake/cake`, not the surrounding workspace or `cake-repl`. All source and document paths below are relative to this clone. Cake is a Rust binary with Chat Completions and Responses adapters, a shared agent loop, trusted hooks, a tool registry, and append-only session JSONL. New code follows the existing dependency direction from CLI to clients/config/prompts/types; configuration and types must not import agent orchestration.

A raw transcript is the ordered record of what actually happened. A request projection is the ordered `ConversationItem` list sent to the provider. A checkpoint is one complete saved summary plus a retained tail. A provider exchange is one assistant response, its associated reasoning, and all the tool calls/results issued by that response. An invocation is one `Agent::send` call opened by the current user input. A request revision changes whenever the source context, prompts, tools, or model change; it prevents a failed proactive attempt or old token measurement from being reused for a different request. A content-class estimate is the pure conversion of unmeasured conversation items into an approximate token count. The retired boundary is the position in the record stream of the latest checkpoint: conversation records before it are represented by the checkpoint, and records after it are sent as-is.

A pin is an exact input or complete exchange that selection may not retire. Its provenance states why it is retained and which source task owns it, without granting authority. A consumed image is one whose pixels were included in a main request that produced an accepted response; a rejected or discarded request does not qualify. A failed writer is a terminal state shared by all writer clones, not Rust mutex poisoning. Qualification is a measured real-model evaluation against expectations written independently of the generated summary.

`src/types/conversation.rs` owns provider-neutral conversation items. `src/types/session.rs` owns session/stream records, existing conversation payload structs, `TurnUsageData`, and replay declarations. `src/types/usage.rs` owns normalized counters and usage presence. Put new checkpoint data, the pure content-class estimator, and pure projection validation in `src/types/compaction.rs`, reusing existing payloads; export only what callers need from `src/types/mod.rs`.

`src/config/session.rs` owns session loading, `SessionWriter`, format checks, and tail repair; `src/config/session_jsonl.rs` owns framing, and `src/config/data_dir.rs` checks headers for session discovery. `src/clients/agent_state.rs` owns raw history and incomplete-call repair. `src/clients/agent.rs` owns usage settlement and agent state, and `src/clients/agent/agent_loop.rs` orchestrates requests and tools. `src/clients/agent_runner.rs` owns HTTP attempts; `src/clients/backend.rs`, `src/clients/chat_completions.rs`, and `src/clients/responses.rs` own provider translation. Add the compaction module at `src/clients/compaction.rs` and keep orchestration additions to the already-large loop narrow.

`src/config/settings.rs` owns global/profile overlays and resolution; `src/config/model.rs` defines `context_window` as input tokens. `src/main.rs` resolves runs and wires the persistence callback. `src/cli/session_factory.rs` owns resume/fork, `src/cli/persistence.rs` writes fork seeds, `src/cli/replay.rs` owns physical transcript replay, and `src/cli/sessions.rs` owns session browsing. Add manual-command logic in `src/cli/compact.rs`, dispatched through the sessions command. `src/cli/diagnostic.rs` supplies the existing JSON diagnostic envelope.

`src/session_telemetry.rs` owns per-attempt telemetry. The session metrics consumers under `scripts/session-metrics/` must distinguish summary spend from agent request sizing and avoid counting copied records. Deterministic test surfaces already include `src/clients/agent/agent_tests.rs`, `src/clients/agent_runner_tests.rs`, `src/clients/agent/session_restore_tests.rs`, `tests/session_modes.rs`, `tests/resume_repair.rs`, and `tests/replay.rs`.

## Plan of Work

### M0: prepare scoped tasks from the accepted design

ADR 047's record and fork policy, opt-in settings, fixed operation bounds, manual and recovery commands, and qualification gate are accepted. On 2026-10-20, implementation children were allocated under task 034 with native dependencies: 034e M1 (blocks 034f and 034i); 034f M2 (depends 034e; blocks 034g, 034h, and 034i); 034g M3 (depends 034a and 034f; blocks 034h); 034h M4 (depends 034b and 034g); 034i M5 (depends 034e and 034f); 034j M6 (depends 034e through 034i; blocks 034d); and 034d M7 qualification (depends 034c and 034j). Work order: 034a and 034b first, then 034e and 034f, then 034g and 034i, then 034h, then 034j, then 034d. The allocated IDs are recorded in task 034; no milestone child is split further.

Use native `ahm task dep add` relationships, and check each prerequisite's merged commit before starting dependent work from master: Completed in the home store does not mean integrated. Task 004's broader output-management scope remains separate; link the delivered subset without changing its status. Proof of M0 is met: ADR 047 is accepted, children 034e-034j and 034d are inspectable with native dependencies, and task 034 records the work order. The authoring task does not start these milestones.

### M1: establish checkpoint validation, projection, and persistence

Add a versioned `CompactionData` and a restricted `RetainedConversationRecord` union of existing message/reasoning/call/output payloads. The checkpoint contains UUID and session/task identity, timestamp, trigger, summary, retained records, configured model/config identity, summary instruction version, estimated before/after input with a basis, and optional summarizer `summary_usage` and `summary_time_ms` measurements. It contains no main-turn usage aggregate, nested checkpoint, or executable command. Validate checkpoint version, size, nonempty summary, and complete call/output pairing. Preserve execution-time replay snapshots instead of reconstructing them with `SessionRecord::from_conversation_item`, which lacks that metadata. The checkpoint is a new self-versioned `compaction` variant of `SessionRecord` appended to the existing format-4 stream; the session `format_version` stays 4 and no header is rewritten.

Retained records also carry typed local pin reasons, source-task identity when known, and known message origin; none is a pointer into the original file or a provider-visible item. Add optional message-origin metadata for actual user versus Cake-authored synthetic input, with historical absence meaning unknown. Add optional skill-read provenance to successful function-call outputs, naming the observed skill and path. These additions reuse the existing payload structs, survive ordinary and recovery forks, and do not alter provider serialization or skill-activation event counts. Do not serialize credentials or complete resolved settings into model identity.

Introduce one pure projector that uses the latest validated checkpoint and subsequent conversation records. Both live state and session restore use it. `ConversationState` retains its raw vector and raw offsets, the active checkpoint, and the raw boundary at activation, which is derived from the latest `compaction` record's position. Do not use provider call IDs as globally unique raw-entry identities: IDs may repeat after completed calls. Supply fresh prompt context and the current invocation's developer additions separately; never retire hook additions or the output-schema instruction during that invocation. Merge pins, intervening messages, and selected complete exchanges once each in source order. In particular, a new user request must follow retained exchanges from earlier tasks. The projector drops every conversation record that precedes the boundary, even though retained records duplicate some of those originals, and emits retained records plus subsequent records once each. Full
raw history remains in memory; do not claim constant host-memory usage.

Make `Session::context_seed()` return full raw history and the derived positional boundary rather than asking `Session::messages()` to silently become a compacted transcript. Deriving the boundary means scanning records in order and recording the conversation-item count at the latest `compaction` record; the records after it are the `subsequent` items. Run existing incomplete-call validation/repair before planning compaction; persist repair outputs as today. Reject unknown checkpoint record versions. The header `format_version` check and the v4 constant are unchanged in full loading, replay, browsing, and discovery.

Add a checkpoint-specific `SessionWriter` append that holds the existing mutex/advisory lock through serialization, newline, flush, and `File::sync_data`. Wire the observer's checkpoint commit through that operation and return errors instead of the usage path's warning-only behavior. The compaction owner activates only after success. Inside the same mutex, any append/flush/sync failure moves shared writer state permanently from open to failed. Detach the `BufWriter` without flushing its unwritten bytes on drop, retaining the locked file until all owners close. Every append API and clone must reject writes in this state. Update `src/main.rs` cleanup integration so hooks retain their lifecycle but their audit appends, usage, compaction-end, and task-completion records cannot touch the failed file. Preserve the original commit error as the primary diagnostic. A sync may block beyond the model-operation deadline; do not promise a hard filesystem deadline. Reuse existing partial-tail repair
on a later locked reopen.

Update fork seeding in `src/cli/session_factory.rs`: it currently copies only records whose `to_conversation_item()` is `Some` plus `SkillActivated`. It must also copy the latest `compaction` record, re-enveloped for the new session and placed at its original position among the copied conversation records, so the positional boundary survives. A format-4 source with no checkpoint forks unchanged.

At M1's end, pure fixture tests show that two checkpoints select only the newest summary, fresh prompts survive, raw offsets do not change, repeated call IDs remain valid, and copied tails do not duplicate billing or replay events. Boundary tests show that a resume across two checkpoints uses only the latest, that no pre-boundary raw record is emitted, and that a fork reconstructs the same boundary. A format-4 fixture with a checkpoint still loads, lists, replays, and resumes, and an unknown checkpoint record version fails clearly. Checkpoint write-failure tests inject failures before serialization completion, flush, and sync without real power-loss experiments. Run `cargo test compaction`, `cargo test session`, and the session/replay integration commands in Concrete Steps. No production path compacts yet.

Drive each write failure through the actual CLI error and session-end hooks, task completion, concurrent writer clones, and final owner destruction. After the failed commit returns, assert byte-for-byte stability of the file until a new locked open; assert no next provider request. Reopen must recover a torn final record or retain a complete valid checkpoint, including one whose sync reported failure. Add origin/provenance round trips and source-order fixtures with a new user input after old retained exchanges.

### M2: deliver bounded preparation, summaries, and accounting

Implement `crate::clients::compaction::prepare` and `crate::clients::compaction::summarize` behind the new module's small interface. Preparation takes an immutable source projection with provenance/replay metadata, fresh prompt/tool/schema input, exact pins, image-consumption evidence, the known input window if any, and resolved policy. It estimates sizes, chooses whole exchanges, and returns a typed no-op/no-fit/prepared result. A fake summary caller and a production caller through the existing runner are the two adapters at the summary-call seam; no new provider library, tokenizer, model registry, or dependency is needed.

Resolve `[compaction]` defaults as disabled, 80% threshold, 20000 retained-token target, and 4096 summary-text target. Validate threshold 10 through 80 and positive values; clamp retained tokens to a quarter of the input window and summary text to a sixteenth. For source estimate S, require a reduction of at least ceil(S / 10). With a known window W, let T = floor(W \* threshold_percent / 100) and require the candidate at or below floor(3 \* T / 4), including fixed prompts, tool and native output-schema payloads, exact pins, and summary. Thus the default target is 60% of W, with at least 10% reduction. Reject zero effective budgets. Optional recent exchanges use the retained target; mandatory pins may exceed that target but must fit the final request and byte limits. Shrink only unpinned whole exchanges. With no window, manual preparation still requires the 10% reduction and byte caps; automatic checks are disabled.

Keep an entire image-bearing exchange pinned until a main request containing its pixels produces an accepted response. Carry this evidence in live state against the request revision; no HTTP rejection or discarded pressure response advances it. Restore must conservatively pin any image exchange whose consumption cannot be established, rather than infer it from a usage record or repeated call ID. An oversized pinned image exchange returns no fit without a summary call. Only consumed, unpinned image exchanges may be summarized without pixels, with explicit omission labels.

Pin every successful known-skill `Read` exchange in the current task, including partial reads and rereads. Capture output provenance regardless of whether `SkillActivated` has already been emitted, and preserve all pinned results exactly in their original tool role. Reconstruct source-task pins for manual/restore from persisted provenance; an ambiguous legacy activation/read association yields no fit, not invented content. Do not inspect arbitrary Bash commands for skill activation or read instruction files during compaction. Pins expire for the new task when its actual user input starts; historical activation remains audit metadata, and the new invocation uses its fresh catalog. Caller-owned developer context also survives every compaction within the invocation.

Enforce an eight-MiB ceiling for the serialized checkpoint JSONL including newline. During preparation, measure the complete envelope and retained records and reserve 256 KiB for the serialized summary JSON string, including escaping and quotes. Reduce only eligible whole exchanges to satisfy bytes and tokens together before any summary call. Reject when pins prevent either bound. Validate the final summary string against its reservation and remeasure the final checkpoint before commit; the reservation does not replace the token limit.

The content-class estimator is one pure function in `src/types/compaction.rs`. It starts from complete, positive, model/projection-matched main-request input usage plus an estimate of every item appended since. With partial usage, estimate the full projected request instead: the existing marker does not prove which counters were present, so never infer presence from a zero counter. After compaction or prompt/tool/model changes, estimate the full projected request. Assign a fixed characters-per-token ratio by content class --- prose, JSON and code text, and base64 or opaque replay bytes each converts more densely than prose --- and a conservative flat token cost per inline image, because an image's provider cost tracks its dimensions rather than its base64 length. Add a documented safety margin (for example one quarter) so estimates err toward triggering early. Include tool schemas and opaque replay material conservatively, but do not add cache counters to normalized input again. Use
safe saturating arithmetic, and distinguish estimates from measured counts in records and output. The estimator has no tokenizer dependency.

Serialize retired context as labeled data, including the previous summary once. Keep existing spill paths and bounded head/tail failure excerpts; never read those paths during summarization. Omit encrypted reasoning and image base64 from the summary prompt. Preserve original transcript data and label omission of nontext content; do not claim to have summarized visual details unavailable to the caller. The summary instruction requests goal, constraints, actual progress, decisions, actual verification results, artifacts, and next steps, with clear source roles.

Enforce one MiB per summary prompt, estimated input at most 70% of a known window, at most four logical summary requests, and 180 seconds across all requests/retries. If serialization needs batching, batch oldest complete exchanges sequentially and privately carry the candidate summary forward. Excerpt a giant result before batching; if one exchange still cannot fit, fail explicitly. Never drop an exchange silently or publish an intermediate batch. A summary call sends the conversation's resolved provider/model/effort, no tools, no task hooks, no output schema, and a generation cap at most 16384 and no greater than its configured output maximum. Validate configured reasoning-token allowances against this cap without changing effort. Add a backend-owned support check at the transformed-request boundary. The current Codex adapter removes this cap and is explicitly unsupported; validate before any summary request. Automatic enabled-policy configuration fails before a run starts, while
disabled-policy restoration remains usable. Manual compaction fails before task-boundary writes or summary calls. No hidden model fallback or unsupported uncapped request is permitted.

Reject absent/blank text, tool calls, refusal, incomplete or length termination, cancellation, or output over the summary-text target. Check the final serialized checkpoint against eight MiB. A failed batch rejects the whole candidate. The summary caller must not update raw history, normal turn counters, last main-request usage, or schema-correction state.

Extend `TurnUsageData` and `ApiAttemptTelemetry` with optional typed purpose, compaction UUID, and summary-step ordinal. Missing purpose means main agent for historical records. Settle every reported summary attempt once, including failed/retried/discarded attempts, before classification; task aggregate usage includes it once. A summary retry uses attempt ordinal plus summary-step identity so records cannot masquerade as normal agent turns. Update `record_turn_usage` so compaction-purpose attempts never change the context-usage basis. Main `max_turns` and `max_tool_calls` counters exclude summary calls; bounded summary steps/deadline constrain them separately.

At M2's end, scripted callers prove one-request normal compaction, bounded multi-batch compaction, unchanged model/effort, no tools/schema/hooks, preserved failure evidence, exact user input, zero hidden model fallback, and rejection of every partial result. Test missing/partial/zero usage, stale estimates, small windows, oversized fixed prompts, the content-class estimator's ratios and margin on byte-exact fixtures, and total accounting on summary failure. Run `cargo test compaction`, `cargo test agent_runner`, `cargo test usage`, and `just session-metrics-check`; all are offline.

Include image tool results returned immediately before a threshold crossing: their pixels must reach an accepted main response before retirement, or preparation must fail without spending. Include two individually legal five-MiB images with low token estimates, proving byte-driven selection or no fit before generation. Test exact skill partial-read/reread preservation across repeated checkpoints, no duplicate activations, ambiguous legacy metadata, and pins exceeding budgets. Check 80,100 to 79,900 tokens against an 80,000 trigger is rejected, native-schema bytes/tokens are counted, and Codex support checks make zero network calls. Snapshot the actual capped wire requests for supported Chat Completions and Responses configurations.

### M3: compact before provider requests without changing raw attribution

Add global/profile field-wise compaction overlays to `src/config/settings.rs`, with the existing settings precedence and unknown-key diagnostics. Pass resolved policy into agent construction in `src/main.rs` and `src/cli/session_factory.rs`. Check backend support when automatic policy is enabled, before starting the run. Compaction appends in place on any format-4 session; a no-fit preparation commits no checkpoint and makes no summary call. A session that already contains a checkpoint honors it even when automatic policy is disabled, including on the Codex backend, because disabled controls automatic creation, not restoration.

Call preparation immediately before each logical provider request in `Agent::send`, after complete tool batches and hook additions. Hooks keep their existing lifecycle and authority; place the check after context-affecting hooks so their output is sized, and do not execute hooks again merely because a summary request runs. Resolve conflicts with `max_turns`/`max_tool_calls` before spending on a summary that cannot lead to another agent turn. In no-session runs, validate and activate in memory. In persisted runs, commit through M1 before activation. Keep `invocation_start`, provider-turn boundaries, and `TurnMode` keyed to raw history, never projection offsets.

Add stream lifecycle records for compaction start/end and the committed checkpoint. Success orders start, checkpoint commit, checkpoint stream emission, then end, with no duplicate retained events. Persist start/end as audit metadata; they contribute no model context. A prepare failure emits failure end metadata but no checkpoint, and an interrupted process may leave start without end. A failed writer never appends an end record. A later output error does not undo the saved state. Text progress is stderr-only. After any proactive summary failure or no fit, report the outcome and disable proactive attempts for the remainder of that invocation, even when new tool content changes the revision. Continue uncompacted only while estimated input fits W. The single reactive allowance remains, but unchanged deterministic no-fit or unsupported-backend outcomes short-circuit without another summary. A new invocation or explicit manual command gets a fresh budget. After a successful checkpoint,
reset measurement eligibility and use the new estimate until a main response supplies fresh usage.

Integrate task 034a's scripted prefix test: between commits, request N+1 extends request N exactly; a committed checkpoint is the explicit exception. Include hooks, correction mode, retries, and multiple checkpoints. Fake summaries are deterministic and should retain fixture facts needed to finish. M3's proof is one long user input crossing the threshold after large tool results, completing without duplicate tool execution, plus unchanged behavior when policy is omitted/disabled. Run `cargo test compaction`, `cargo test clients::agent`, `cargo test settings`, and `cargo test --test session_modes`.

Exercise at least twenty successive small tool turns near the threshold, asserting that each successful checkpoint leaves the specified headroom and that a failed proactive operation is not repeated as the source grows. Verify exact hook/schema context and skill results survive repeated checkpoints, and a new invocation resets only invocation-owned pins and failure suppression. A schema-correction turn must preserve synthetic origin and the current real user request independently.

### M4: add one classified context-pressure recovery

Integrate the already-scoped classifier from task 034b alongside `src/clients/retry.rs` and provider termination parsing. It distinguishes genuine input overflow, output-allowance mismatch, recoverable early termination under known context pressure, genuine intended-cap stops, refusal, quota/rate-limit errors, and unknown termination. Preserve actual reported usage even when response items are discarded. A generic length stop or missing output count cannot alone prove pressure. A length-stopped summary always fails regardless of the normal task classifier.

Classify candidate assistant responses before `process_completed_turn`, streaming/persisting their conversation items, or dispatching any returned tools. Preserve the runner's per-attempt accounting. Input-overflow errors get the same typed verdict from both HTTP and Responses failure paths. Keep output-allowance reduction for input that fits; when the input does not fit, propagate to the compaction owner instead of repeatedly lowering output.

Add one pressure-recovery guard for the entire current invocation, shared by classified input overflow and recoverable pressure terminations. It is consumed before starting recovery preparation. With enabled policy and a known window, prepare/commit a checkpoint and retry the intended turn once, preserving output constraint and mode. A second pressure failure, no fit, or summary failure ends the task neutrally with its session ID. A pressure verdict must not also trigger semantic continuation. Existing transport retries remain bounded separately, and output-schema correction counters are never reset by compaction.

At M4's end, fixtures prove that a recovered rejected request has exactly one compaction and one logical retry, discarded responses cause no tool dispatch, usage is retained, genuine cap/refusal/quota failures do not compact, and schema correction remains tools-disabled and constrained. Test an invocation that already compacted proactively and then overflows; it still has only one reactive recovery. Run `cargo test compaction`, `cargo test retry`, `cargo test clients::agent`, and `cargo test agent_runner`.

### M5: complete manual compaction and restore surfaces

Extend `SessionsSubcommand` with `compact <UUID>`, optional `--instructions TEXT`, and `--json`; keep root prompt-run flags invalid with the subcommand. Implement in `src/cli/compact.rs`, reusing existing model/session resolution and prompt construction instead of a second resolver. Acquire the existing session writer lock before loading mutable state and run the same repair/validation and compaction operation. The command works in place on any format-4 session, subject to the writer lock. Emit task boundaries for the standalone operation, account for summaries, and report success/no-op without an assistant continuation or tool execution. Explicit manual use works with automatic policy disabled; it still obeys every byte/request/deadline bound.

Before creating the maintenance task boundary, validate backend support and select the most recent source task containing user-role messages. Pin every user-role message in that task exactly, preserving synthetic or unknown origin, along with its observed skill exchanges. Ignore later maintenance tasks without user input. Carry source-task pin provenance through checkpoints and ordinary forks. If legacy fork filtering removed task boundaries, pin all user-role messages and observed skill exchanges in the ambiguous segment; if legacy skill-output matching itself is ambiguous, fail no fit. Do not infer user authorship by recognizing correction text. Test repeated manual compaction, a latest completed source task, synthetic correction messages, a newer empty maintenance task, and a legacy fork without task boundaries.

Add `cake sessions recover <UUID> [--json]` in `src/cli/sessions.rs` with implementation in `src/cli/compact.rs`. Acquire an exclusive advisory lock on a read-only source handle and load its stable complete records without invoking source-tail repair; preserve every source byte. Reuse fork-file creation and cleanup, copying original conversation, replay declarations, message/skill provenance, and activation records, but omitting every checkpoint and its lifecycle records. Preserve model/system-prompt identity without resolving credentials. Validate source records normally, rejecting corrupt records or unknown checkpoint versions; recovery targets inaccurate valid summaries. Seed the new file and conservatively persist abandoned-call repairs there, with no tool execution. Use a normal maintenance task boundary/completion for that repair operation in the new file, reset usage eligibility, and emit only a diagnostic document for `--json` with source/new UUIDs and status. Make no model,
hook, or tool calls. A held source lock prevents creation of the destination. Input errors exit 3, operational failures 1, success 0, and interruption 130. A failed destination seed follows existing cleanup; successful repeated commands intentionally produce separate forks.

Recovery output explains that raw history may exceed the configured window and names `cake sessions compact <NEW_UUID>` before continuation. Re-compaction follows all original bounds and can fail no fit; it rebuilds from raw evidence and cannot undo tool side effects or erase inaccurate assistant claims in that evidence. Normal forks retain the latest checkpoint. Verify recovery of a valid summary that omits a required fixture fact, omission of all checkpoints in the new file, source byte stability, repaired calls executed zero times, operation without credentials, and destination cleanup after injected seed failure.

Render manual JSON with `DiagnosticDocument`, `command = sessions compact`, session/checkpoint IDs, trigger, status, and explicitly estimated sizes. Do not interleave lifecycle stream records with this one JSON document. Use the existing diagnostic error conventions: input/identity errors 3, operational failures 1, success/no-op 0, interruption 130. An `--instructions` value focuses the summary rather than becoming a command or granting permission. Ordinary prompt completion JSON fields remain unchanged.

Update restore, fork, and replay together. A fork copies original conversation and activation metadata, re-envelopes the latest checkpoint for the new session at its original position among the copied records, and copies all later conversation records in order; it preserves replay metadata and has no references into the source session. The copied conversation records before the checkpoint are retained for audit fidelity, and the checkpoint sits at its original position so the positional boundary is reconstructed. Seed the fork's effective context through the common projector, not a new independent algorithm, and reset main-request usage eligibility because fresh prompts may differ. Copying retained checkpoint content never duplicates raw tool records or billing events. Replay emits the checkpoint once in physical order and never expands retained contents. Readers list, discover, replay, and load the format-4 stream including checkpoint records; a pre-checkpoint Cake binary is not a
supported reader for a checkpoint-bearing file.

Update `docs/integrations.md` with the `compaction` record kind, provenance fields, record and stream semantics, accounting attribution, recovery-fork behavior, and exit codes; `docs/configuration.md` with settings/precedence, backend restrictions, and opt-in examples; `ARCHITECTURE.md` with projection ownership and the prefix exception; and `docs/security.md` with generated-memory trust and unchanged grants. Update command help and README with manual operation, raw-history recovery, no-fit remedies, host-memory limits, observed-skill scope, and qualification limits. Keep authoritative docs describing shipped behavior until their implementing stage is ready. Add narrow notes to ADRs 004/021/025/026/030 referring to ADR 047's extension rather than rewriting their history.

At M5's end, offline CLI integration tests prove a session without a checkpoint still lists, replays, resumes, and appends unchanged, that manual compaction appends a checkpoint in place on any format-4 session and reports a no-op when there is no new content, that manual compaction makes no tools/continuation calls, that JSON stdout parses, and that restored and forked requests use the newest checkpoint under fresh prompts. A held writer lock prevents all model calls. Run the integration commands below and `just session-metrics-check`.

### M6: prove deterministic continuation and integration

Add `tests/compaction.rs` with local HTTP fixtures for both provider adapters and isolated session/config roots. It drives a long task through at least two proactive checkpoints, then a pressure rejection/recovery, then completion. It records request bodies and tool side effects, verifies exact original user text and fixture facts, confirms retired raw content is absent from later requests, and checks that every tool side effect happened once. Replay, resume, and fork that same session under fresh prompts and disabled automatic policy; each still restores the latest checkpoint and emits no pre-boundary raw record. All fixtures must have zero external network or paid-model dependencies.

Exercise summary cancellation and failure before commit, partial append/flush/sync failure through the full cleanup lifecycle, failure of the output sink after commit, missing/stale usage, refusal, oversized fixed context, unconsumed images, byte-heavy images, exact skill pins, opaque reasoning, parallel calls, and repeated call IDs. Byte-for-byte compare existing transcript prefixes before and after successful appends and failed-writer cleanup. Include Codex rejection without summary calls, raw-history recovery, manual pin selection, and twenty-turn headroom/failure-suppression cases. Confirm bounded summary attempts and reactive recovery, billing totals without double-counted tail records, and stable partial-result attribution for resource-limit stops.

Run focused tests once after the final changes, then `just check`, `just docs-check`, `just session-metrics-check`, and `just check-full` for the cross-cutting implementation. Complete each implementation child's Acceptance Notes and documentation assessment before its final commit. Preserve platform verification results in the PR handoff; this feature adds no sandbox grant, but ordinary macOS/Linux CI must still pass. Record implemented outcomes and any narrower task-004 scope delivered on task 034. Deterministic success completes M6, not M7 or the parent; keep the plan active until qualification passes.

### M7: qualify repeated summaries and task continuation

Qualification child 034d owns this milestone. Prepare a fixture corpus and runner under `scripts/evals/`, reusing its isolated repositories, trusted deterministic verifiers, model selection, executable fingerprinting, and usage records. Add `scripts/evals/run_compaction_eval.py` and offline tests under `scripts/evals/tests/`, covered by `just eval-check`. Use only synthetic or explicitly approved redacted material. Commit at least twelve independently annotated cases covering persistent prohibitions, corrected user requirements, failed versus successful verification, exact paths/artifacts, unfinished work, parallel results, skills, images, and misleading tool text. Each case fixes critical facts and constraints before observing generated summaries. Each compaction arm must cross at least three checkpoints; include two drift cases with at least ten. Pin preservation and mechanical JSON validity alone cannot satisfy semantic acceptance.

Compare a compaction arm with an uncompacted control on the same provider/model, reasoning effort, output cap, initial task, and fixture state. Choose cases that fit the provider's actual window in the control; use a smaller configured input budget in the compaction arm to force checkpoints without making the control intrinsically impossible. Run three repetitions per case and arm, on at least one supported configuration for each API adapter. Record served model identities, configuration, binary and prompt/corpus versions, each checkpoint, fact retention, contradictions, invented completion claims, task correctness, latency, and reported input/output usage. Do not silently drop provider failures or replace failed trials. Summarize differences by case and configuration, not only an overall average.

Before any provider calls, request explicit approval for named model configurations, the corpus, maximum trials, and a spend ceiling. Add the future command `python3 scripts/evals/run_compaction_eval.py --model MODEL --repetitions 3 --max-trials 72 --max-reported-tokens TOKEN_BUDGET --results-dir RESULTS_DIR`, run once per approved configuration from this repository root. The runner stops scheduling at either approved bound. Before each trial, reserve a conservative token allowance derived from explicit input/output, agent-turn, retry, and summary-operation limits, including any auxiliary provider calls; refuse trials whose allowance cannot be bounded or covered by the remaining authorization. Release unused reservation only for fully accounted attempts; unknown usage cannot create extra budget. TOKEN_BUDGET, MODEL, RESULTS_DIR, and the per-trial limits are supplied in the authorization record, not guessed by the implementing agent. Provider spend is not authorized by this plan or its
acceptance. Offline command/schema/fixture verification must work with a scripted caller via `just eval-check` without credentials.

Require zero critical-fact omissions or contradictions and zero prohibited actions or invented completion/verification claims across all checkpoints and continuations. Require at least 95% retention of the annotated noncritical facts at every checkpoint, and at least 90% verifier-confirmed task success per qualified configuration, no more than ten percentage points below its control. Score retention in the complete effective projection, including exact pins and recent evidence, rather than requiring the summary to duplicate retained facts. Each annotation states when the fact becomes known and when a later correction supersedes it. Human review against the fixed annotations assesses freeform summary fidelity; trusted fixture verifiers determine task success. A model's claim that it obeyed or completed work is not evidence. Report sample size and uncertainty; qualification applies only to the evaluated configurations and instruction version, not every compatible model. These are
prospective acceptance thresholds, not measured claims.

If qualification fails, record failures and revise the summary/policy under the original decision scope, rerun offline checks, and obtain any additional spend authorization before rerunning the full qualification set. Do not silently lower thresholds, substitute an easier corpus, or complete the parent while evaluation is pending. After passing M7, record the evidence and qualified scope in task 034, complete all children and the tracker under the task lifecycle, fill Outcomes & Retrospective, move this plan to `docs/exec-plans/completed/context-compaction.md`, and update task references. Automatic compaction remains opt-in.

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

Each applicable command must end with `test result: ok` or a zero-exit fixture-suite result, and new filters must run at least one test; a zero-test match is not evidence. Run only the milestone-relevant subset until M6; M7's harness additionally runs `just eval-check`. For final implementation integration:

```
cargo test --test compaction
just check
just docs-check
just check-full
git diff --check
```

Expect successful offline compaction fixtures and every routed gate passing. Record exact failures, skipped platform checks, and fixture request/side-effect counts. These commands authorize no paid model runs. Deterministic tests use fake summaries; M7's separate authorization and evidence are required before feature completion.

For this authoring branch alone, validate the two documents with:

```
panache format --check docs/adr/047-compact-model-context-with-self-contained-append-only-checkpoints.md docs/exec-plans/active/context-compaction.md
panache lint docs/adr/047-compact-model-context-with-self-contained-append-only-checkpoints.md docs/exec-plans/active/context-compaction.md
git diff --check
```

Stage the two authored paths before `just docs-check` so the tracked-corpus gate includes them. Rust gates are unnecessary for these documentation-only drafts. The plan remains active and ADR 047 is accepted after these checks.

## Validation and Acceptance

A successful long-run fixture makes more than one checkpoint inside a single user invocation and reaches the expected final answer without repeating tools. Its provider requests after each checkpoint contain fresh prompts, exact current input once, one summary, and whole retained exchanges; they omit retired raw history. Every non-compaction request extends the prior request's prefix, as enforced by task 034a. Parallel tool call/output associations and Responses reasoning replay remain valid.

Unconsumed image exchanges and current-task observed skill instructions survive exactly or produce no fit before any summary request. Current hook/schema instructions survive every checkpoint. Pins and other retained messages preserve their source order and origin. Selection satisfies both serialized-byte limits and token headroom before generation; the final checkpoint passes both checks again. Twenty-turn fixtures demonstrate that failed proactive attempts do not recur on each new source revision. Unsupported Codex configuration makes zero summary calls, while disabled-policy checkpoint restoration still works.

A format-4 fixture without a checkpoint still lists, replays, resumes, and appends ordinary records unchanged. A format-4 fixture with a checkpoint resumes and forks to the same effective context, honors existing checkpoints even when automatic creation is disabled, and emits no pre-boundary raw record. A checkpoint commits in place on any session, so no format migration or explicit-fork step is required. Unknown checkpoint record versions and corrupt checkpoints produce errors, never fallback to a partly guessed history. Replay is read-only and emits copied-tail content only inside its one checkpoint record.

Failed, empty, refused, truncated, cancelled, over-budget, or tool-calling summaries commit no checkpoint and leave context intact. A second context-pressure failure does not trigger a second reactive summary. Commit uncertainty disables every shared-writer append and drop-time flush before cleanup, and prevents another provider request. A committed checkpoint remains restored after stream-output failure. Usage fixtures prove summary attempts count once in task totals, including failures, while context measurement and cache-break analysis use only eligible main requests.

Manual compaction returns a recognizable diagnostic JSON document, succeeds or reports a no-op without an assistant/tool continuation, obeys the writer lock, and uses the same configured model/effort as automatic compaction. Sandbox grants, tool selection, skill activation, hook authority, and running BashSession identity remain unchanged. No test may assert summary claims as a substitute for actual side-effect or validation evidence.

Manual pinning selects the latest prompt-bearing source task rather than its own maintenance task, including after repeated compaction and ordinary forks. Recovery emits a new session UUID with original raw evidence and no checkpoints, preserves source bytes, requires no credentials, and executes no hooks, tools, or model calls. A recovered fork can be manually re-compacted under the same operation bounds; no guarantee is made that arbitrarily large raw history fits.

Task 034 stays Tracking and the plan stays active until M7's authorized real-model results meet the fixed fact-retention, constraint, and task-success thresholds. Archive the instruction/corpus/configuration versions and qualification evidence in the managed task; document unqualified configurations as experimental and keep the feature opt-in.

## Idempotence and Recovery

Pure estimation, projection, selection, and fixture commands are repeatable. CLI tests use isolated temporary stores. A repeated manual command with no newly compactable content reports no-op instead of appending a second checkpoint for the same revision. A failed proactive attempt disables proactive work for the rest of that invocation, regardless of new source content. Only the single eligible reactive attempt, a new invocation, or an explicit manual operation can try again. Recovery commands intentionally create a new UUID each time; they never mutate the source.

Before append, cancellation or model failure leaves the previous checkpoint authoritative. During append, finish the commit section before cancellation; an append/flush/sync error terminally disables all writer clones and discards unwritten buffered bytes without flushing. Error hooks and completion cleanup cannot append afterwards. On restart, a new locked writer repairs a torn tail using the existing mechanism; a complete checkpoint is validated and used. Do not repair a checkpoint by editing old JSONL bytes or discard a complete record merely because a previous command reported output failure.

Physical-tail repair and semantic recovery are distinct. A complete valid but inaccurate checkpoint remains authoritative on ordinary resume and fork, even with automatic policy disabled. Use `cake sessions recover <UUID> --json` to create a raw-history fork, then `cake sessions compact <NEW_UUID> --json` to rebuild its summary if the bounded operation can fit. The recovery command locks and reads the source without repairing or rewriting it; abandoned-call repairs apply only to the new file and execute no tools. A failed fork seed follows the existing cleanup policy for the incomplete destination. Neither operation reverses tool side effects.

If implementation changes a durable decision, create a successor ADR and revise every affected section here. If a required check is blocked by the environment, record the exact reason and narrower evidence; do not bypass gates or claim completion.

## Artifacts and Notes

Keep fixture evidence in tests and concise transcripts in implementing tasks. Expected observations, not results already achieved:

```
compaction checkpoints: at least 2 proactive, at most 1 reactive
summary logical calls per operation: 1 through 4
tool side effects: exactly one per original call
copied-tail events emitted as original events: 0
external provider requests in deterministic fixtures: 0
real-model qualification: separate authorized M7 evidence required
task usage: sum of reported main and summary attempts, each once
```

Record the baseline commit, implemented child IDs, feature commits/PRs, final gate results, and document assessment on task 034. Existing migration warnings about missing blocked reasons are unrelated backlog metadata and must not be fixed as part of this feature. The October 9 research motivated the design; everything needed to implement it is stated in this plan, without requiring external source reading.

## Interfaces and Dependencies

Keep `ConversationItem` as the provider-neutral conversation authority. New domain types in `crate::types::compaction` are `CompactionData`, `RetainedConversationRecord`, `CompactionTrigger`, typed pin/provenance data, and estimated-size/basis data. Reuse message-origin and skill-read annotations owned by existing session payload types. Add a pure `project_context` function returning validated projected items from fresh and invocation-owned prompt context, an optional checkpoint, and subsequent items. A pure content-class estimator sizes conversation content; the preparation layer separately includes tool/native-schema payloads and serialized checkpoint bytes. Session restore supplies a typed `RestoredContext` seed with raw history, provenance, conservative image-consumption state, and the derived positional boundary; live requests and forks cross this same projection interface.

The compaction module exposes immutable preparation and bounded summary generation, returning a `CompactionCandidate` or explicit no-op/no-fit/failure. The internal `SummaryCaller` interface accepts a typed summary request and cancellation/deadline context and returns text, termination, and reported usage; one runner adapter and one scripted test adapter implement it. The module writes no checkpoint; the caller's attempt-settlement callback still records billed usage before classification. The owning agent or command calls the existing observer/writer seam to commit, then activates the checkpoint. Avoid a second agent, plugin registry, background worker, or generic memory framework.

`SessionWriter` gains a checkpoint commit operation whose success guarantees newline, flush, and data sync under the existing lock, and a terminal failed state shared by every clone. Its failed state owns the locked file without an automatically flushing buffer. Backend capability checks inspect the final transformed request contract before summary generation. `TurnUsageData` and telemetry gain optional purpose/compaction/step attribution; existing `Usage` counters and ordinary completion JSON remain their current shape. Introduce a main-request-only context-measurement basis tied to request revision/model, rather than overloading task-total usage or the latest summary usage.

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

Revision note (2026-10-13): design review added protected image/skill exchanges and input provenance, shared-writer failure handling through cleanup, byte-aware preparation, useful headroom and failure suppression, explicit backend support, raw-history recovery, and the mandatory M7 qualification gate. Trav selected the backend, recovery, qualification, and skill-retention policies; the format-4 and positional-boundary decisions remain unchanged.

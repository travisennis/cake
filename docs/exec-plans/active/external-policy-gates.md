# External policy gates for Bash command authorization

This ExecPlan is a living document maintained per `docs/workflow/exec-plans.md`. Keep Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective current. Planning task: ahm 128a. Platform tracker: ahm 128. The governing decision is `docs/adr/049-external-policy-gates-with-the-cake-gate-1-protocol.md` (ADR 049). Cross-project handoff: `~/Projects/cake/initiatives/cake-gate/README.md`.

## Purpose / Big Picture

After this work, command policy is an external executable. A user can install `cake-safety-judge`, replace it with a deterministic rule set, put a fast typed classifier in front of a model judge, or run a local model, without changing Cake or rebuilding it. Cake still decides what the command is, what context the gate sees, which gates run, whether a decision is enforced, and what telemetry is written. A missing gate never means an ungated command: Bash is removed from the tool list and a diagnostic names the fix.

A user can see the result with three commands. `cake gates doctor` describes every configured gate and reports coverage, protocol, model, and environment problems. `cake bash check --json -- 'git push --force'` runs the full pipeline without executing anything and reports the deciding gate plus every stage. `cake gates list` shows what would run. A developer can implement a new gate from `docs/integrations.md` and `docs/schemas/cake-gate-1.schema.json` alone, and can run the gate's own `check` mode against a corpus offline.

The current in-process judge remains the oracle and the only command gate until the external implementation matches it on the frozen corpus and then runs in shadow. This plan is a contract and delivery guide, not a claim that the layer exists. Slice 0 writes the contract; slices 1--5 implement and migrate it.

## Progress

- [x] (2026-10-24) Read the initiative, workflow documents, current judge/Bash code, telemetry, configuration, security, and ADR authorities.
- [x] (2026-10-24) Created tracker 128 and contract child 128a; cut `docs/gate-protocol` in a linked worktree at `.cake/worktrees/docs/gate-protocol` from `origin/master` (`9c39c44`).
- [x] (2026-10-24) Drafted ADR 049 as `proposed`, the `cake-gate/1` protocol section in `docs/integrations.md`, the `[gates]` schema in `docs/configuration.md`, the trust boundary and no-gate rule in `docs/security.md`, and `docs/schemas/cake-gate-1.schema.json`.
- [x] (2026-10-24) Drafted this ExecPlan.
- [x] (2026-10-25) Contract reviewed and frozen; ADR 049 moved to `accepted`.
- [ ] Update the docs status notes when enforcement lands (slice 4).
- [ ] Slice 1 (cake): gate protocol types, process runner, composer, in-process judge gate, fake gate, telemetry sink; no user-visible change.
- [ ] Slice 2 (cake-safety-judge): repository, `describe`, `check`, rubric and corpus, offline parity with the oracle.
- [ ] Slice 2 (cake-safety-judge): `serve` with concurrent request IDs and graceful shutdown.
- [ ] Slice 3 (cake): external gate in shadow beside the in-process judge; differential report; latency and cost measured.
- [ ] Slice 4 (cake): `[gates]` recognized, external gate default when installed, no-gate Bash removal, aliases, `cake gates` CLI; one release dual-path.
- [ ] Slice 5 (cake): delete judge internals, delegate `bash check`, move corpus/benchmark ownership, update session analysis.

## Surprises & Discoveries

The current model-visible framing is part of the compatibility surface. A judge `warn` prepends `NOTICE: <message>` and a `block` returns `BLOCKED\n\nReason: <message>`; `sanitize_judge_message` strips control characters except newline, carriage return, and tab and caps the text at 1,000 characters. The gate protocol preserves both shapes so the first slices do not change what the model sees.

`cake bash check --json` writes no document when the judge fails. Verdicts, allowlist overrides, and a bypass exit `0`; timeouts, network, auth, and rate-limit failures exit `2`; other transport failures, malformed verdicts, and refusals exit `1`; unknown models exit `3`. The gate pipeline must preserve the no-document failure behavior and map its error classes onto these existing codes.

The judge's `provider_headers` are only OpenRouter attribution values (`http_referer` and `x_title`), not credentials. That makes the resolved descriptor safe to cross the process boundary as long as Cake sends `api_key_env` as a name and never the resolved `api_key`.

The only file-backed credential path is the Codex/ChatGPT auth store. A gate cannot use it through a descriptor: descriptors are environment-backed only, and a file-backed run model yields no `default`, so a gate that needs a model fails `init` with `config` and `doctor` names the fix. This is deliberate; the Codex integration may be removed, and the contract does not freeze a special case for it.

`JudgeRequest.script_evidence` is currently one optional object, while the protocol carries an array. The seam maps one item to a one-element array; this release never sends more than one.

The judge table is already trusted project configuration: a project `.cake/settings.toml` can set `[tools.bash.judge] enabled = false` and remove the judge for commands run there. Gate configuration does not create a new trust class; it is the same project authority applied to an enforcement surface, so the security document must say so explicitly rather than implying a new protection.

The `[tools.bash.judge.typesafe]` cascade is both a policy stage and a latency optimization. Keeping it inside `cake-safety-judge` for the first external release preserves the current failure semantics; splitting it into a second gate is a later composition decision, not a requirement of `cake-gate/1`.

## Decision Log

- Decision: the protocol is `cake-gate/1`, with `describe`, `serve`, and an optional human/offline `check`; decisions are `allow`, `warn`, `block`, and `abstain`. Rationale: a small, versioned, testable contract; abstain separates "no opinion" from failure. Date/Author: 2026-10-24, Trav.
- Decision: Cake owns enforcement, context assembly, process lifecycle, composition, allowlist, bypass, and telemetry; a gate owns policy. Rationale: one implementation of sandbox semantics and one audit surface. Date/Author: 2026-10-24, Trav.
- Decision: the default transport is one long-lived NDJSON process per gate per run; `oneshot` repeats the same lifecycle per check. Rationale: connection and configuration reuse without a second wire shape. Date/Author: 2026-10-24, Trav.
- Decision: a per-check gate failure fails closed in enforce mode unless the gate sets `fail = "open"`, which is explicit, recorded, and doctor-visible; a `describe` or `init` startup failure is always pipeline-fatal and is not affected by `fail`. The circuit breaker honors the gate's `fail` mode on later checks. Rationale: preserve ADR 018's guarantee while keeping fail-open's configured meaning. Date/Author: 2026-10-24, Trav.
- Decision: no `pre_tool` `bash.exec` enforce gate means Bash and `BashSession` are removed from the tool registry, never run ungated. Rationale: the no-gate rule is the security floor. Date/Author: 2026-10-24, Trav.
- Decision: generic `gate_startup`/`gate_decision`/`gate_evidence` records plus one release of `judge_*` compatibility mapping. Rationale: session analysis keeps working while consumers migrate. Date/Author: 2026-10-24, Trav.
- Decision: the reference gate is written in Go. Rationale: the judge is an HTTP prompt loop, Go matches the workspace's plugin ecosystem, and Rust parity remains the fallback only if provider parity becomes the dominant risk. Date/Author: 2026-10-24, Trav.
- Decision: start with one safety gate containing the TypeSafe cascade; expose a second `typesafe` gate after the protocol is stable. Rationale: preserve today's failure semantics and prove the base gate first. Date/Author: 2026-10-24, Trav.
- Decision: explicit configuration wins over auto-detection: explicit `gates` > explicit `[tools.bash.judge]` table > auto-detected `cake-safety-judge` > default-on in-process judge > no gate. Auto-detection is the zero-config default only when no explicit gate configuration exists; the external gate does not inherit the legacy judge table, so its rubric, TypeSafe settings, and credentials are explicit in `[gates.<name>]`. Rationale: preserve existing behavior and make explicit configuration authoritative. Date/Author: 2026-10-24, Trav.
- Decision: 033b/033c fields are additive `input` additions, and adding fields does not bump the protocol version. Rationale: the trusted-context and action-packet contracts should not be duplicated or blocked on the envelope. Date/Author: 2026-10-24, Trav.
- Decision: bounds are a 1 MiB frame, 64 KiB init config, 1,000-character model-visible text, 8 annotations, 8 evidence items, 8 KiB per evidence payload, and 32 KiB total evidence. Rationale: bound every gate-controlled payload before it reaches the model or telemetry. Date/Author: 2026-10-24, Trav.
- Decision: two consecutive process-level failures disable a gate for the rest of the run; later checks follow the gate's `fail` mode. Rationale: a broken gate must not add a startup timeout to every Bash command. Date/Author: 2026-10-24, Trav.
- Decision: the `[gates]` configuration is documented now but not read until slice 4. Rationale: freeze the contract before code, without claiming unimplemented behavior. Date/Author: 2026-10-24, Trav.
- Decision: the `[gates.<name>]` key is the local instance name and `describe.name` is the gate's product name, so one binary can be configured twice; composition and telemetry use the instance name. Rationale: a binary cannot know its config key, and aliasing is a core policy-experiment case. Date/Author: 2026-10-24, Trav.
- Decision: gate startup failures are pipeline failures, not silent drops. An explicit configuration error is a settings error (exit 3); a gate that fails `describe` or `init` removes Bash for the run with a diagnostic and a `gate_startup` record. Rationale: dropping a broken enforce gate would leave a weaker pipeline. Date/Author: 2026-10-24, Trav.
- Decision: model descriptors are environment-backed only. A file-backed run model yields no `default` descriptor; a gate that needs a model fails `init` with `config`, Bash is removed for the run, and `doctor` names the fix. No `credential` field or auth path is added. Rationale: the Codex integration may be removed, and the contract should not freeze a special case for it. Date/Author: 2026-10-24, Trav.
- Decision: the schema's closed enums and cardinality bounds are the v1 known set. Adding a field is free, adding an enum value or raising a bound is an additive schema edit without a protocol bump, and only removing or changing meaning requires `cake-gate/2`. Capabilities and error classes are open strings: unknown capabilities are ignored and unknown classes are treated as protocol. Rationale: keep gate-author validation precise while allowing additive forward compatibility. Date/Author: 2026-10-24, Trav.
- Decision: `bash check` reports the deciding gate's `code`, `confidence`, and `message`, and `latency_ms` is end-to-end; a chain with no deciding gate leaves them `null` or empty. `data.stages` carries `failed_open` and `disabled`. Rationale: preserve existing field meanings while making multi-gate output unambiguous. Date/Author: 2026-10-24, Trav.
- Decision: an all-abstain enforce chain is a deliberate no-opinion allow with `data.gate: null`; a gate that cannot evaluate must send an error frame. Rationale: abstain means no opinion, not failure. Date/Author: 2026-10-24, Trav.
- Decision: a bypass records `decision: null` in `gate_decision`, matching `bash check`'s null verdict; `bypassed: true` carries the effect. Rationale: a bypass has no gate decision and must not count as a policy allow. Date/Author: 2026-10-24, Trav.

## Outcomes & Retrospective

Slice 0 has produced the accepted ADR, the wire contract, the configuration schema, the security trust-boundary section, the JSON Schema, and this plan. The 2026-10-25 review resolved the precedence/migration, fail-open, open-string, `bash check` mapping, telemetry, and all-abstain issues; the contract is frozen for slice 1. No Rust code, settings parser, process runner, gate binary, or behavior change exists yet. The current in-process judge is unchanged and remains the only command gate.

Fill this section at each slice boundary with what was delivered, what the evidence showed, and what remains. Do not move this plan to `docs/exec-plans/completed/` until slice 5 is verified.

## Context and Orientation

Cake is a Rust 2024 binary-only CLI. The current command gate is `src/clients/judge.rs` with `judge_observer.rs` (telemetry and attempt records), `judge_rubric.rs` and `judge_rubric.md` (verdict vocabulary and prompt), and `typesafe.rs` (the Jev shadow/cascade client). `src/clients/tools/bash.rs` builds the judge request before spawn, collects one referenced script's contents through `src/clients/tools/script_evidence.rs`, maps the verdict to a warning or a tool error, and then runs the command under the OS sandbox. `src/clients/tools/mod.rs` carries `ToolContext`, which currently holds the judge context. `src/cli/session_factory.rs` and `src/clients/agent.rs` assemble the run. `src/cli/bash.rs` implements `cake bash check`. `src/session_telemetry.rs` writes the sidecar, including `judge_attempt`, `type_safe_shadow`, and the `judge_verdict`/`judge_fail_closed`/`judge_bypass` compensation events. `src/exit_code.rs` maps judge failures to exit codes. `src/config/settings.rs` owns
`[tools.bash.judge]` and `[[models]]`.

A **gate** is a trusted external executable that answers one policy question: may this action proceed, should it proceed with a warning, must it stop, or does this gate have no opinion? A **pre-tool gate** sees the action before it runs. The only action in this release is `bash.exec`. An **enforce** gate can change authorization. A **shadow** gate runs beside the enforce chain, records what it would have decided, and never changes authorization. A **fail-closed** gate turns any failure into a block. The **oracle** is the current in-process judge: its decisions on the frozen corpus define parity. The **corpus** is `src/clients/tools/corpus/commands.jsonl` plus the judge rubric tests; it moves to the new gate repository in slice 2 and Cake deletes its copy in slice 5. **NDJSON** means one JSON object per line over standard input/output.

A **seam** is the narrow internal interface that lets Cake swap the policy implementation without changing the tool or agent loop. In this plan the seam is `src/gates/`: protocol types, a process runner, a composer, and a built-in judge adapter. A **fake gate** is a test executable that implements the protocol and can return allow, warn, block, abstain, malformed output, a timeout, or a crash on demand.

The contract authorities are `docs/adr/049-external-policy-gates-with-the-cake-gate-1-protocol.md`, the "Gate protocol" section of `docs/integrations.md`, the "Gates" section of `docs/configuration.md`, the "External gates" section of `docs/security.md`, and `docs/schemas/cake-gate-1.schema.json`. `docs/integrations.md` owns the wire shape and composition. `docs/configuration.md` owns settings and precedence. `docs/security.md` owns the trust boundary and the no-gate rule. The schema is structural; byte limits that JSON Schema cannot express are in the protocol section.

The branch was cut from `master` at `9c39c44` and merged `5e0ffe8` (#749) on 2026-10-24; the merge was mechanical and the slice-0 diff touches no merged source file. Slice 0 works in `.cake/worktrees/docs/gate-protocol` on branch `docs/gate-protocol`. The new gate repository does not exist yet. The workspace rule is one task in one repository: the Cake slices and the `cake-safety-judge` slices are separate tasks.

Terms to keep straight: a **decision** is the gate's answer; a **verdict** is the current judge's wire object; a **record** is a persisted session or telemetry object. A **compensation event** is the existing sidecar record for a cross-cutting tool outcome. A **call id** is the provider-assigned tool-call identifier; telemetry stores only its SHA-256 digest.

## Plan of Work

### Slice 0: freeze the contract

Review ADR 049 and the four contract documents. Resolve any objection by editing the ADR and the affected contract before code. Change ADR 049 from `proposed` to `accepted` only after review. The schema and the docs must agree on every required field, default, bound, error class, and exit code. The slice is done when a novice can write a fake gate from the documentation and when the security review records the bypass classes in `docs/security.md`.

No Rust code changes in this slice. If a contract gap appears, edit the docs rather than starting implementation.

### Slice 1: make the seam real

Add `src/gates/` and route the Bash preflight and `cake bash check` through it. The built-in judge becomes an in-process gate adapter; there is no external process requirement yet. Keep every current setting, environment variable, prompt, verdict code, allowlist rule, bypass rule, session record, telemetry record, and model-visible string unchanged. The existing judge tests are the regression oracle and must pass without rewritten expectations.

The protocol module mirrors the schema with serde types: `Describe`, `Init`, `Ready`, `Check`, `Decision`, `Error`, and `Shutdown`. The runner owns process spawn, the `describe` subprocess, a long-lived `serve` child, the `init`/`ready` handshake, per-check deadlines, frame-size enforcement, process-group kill, restart, and shutdown. Use `tokio::process` and a bounded line reader; the repository already depends on `tokio`, `serde`, and `serde_json`.

The composer is a pure function from recipe order and gate results to a pipeline outcome. It applies abstain, warn accumulation, block stopping, the exact allowlist override, shadow exclusion, fail-open, short-circuit, and the no-enforce-gate rule. Unit tests cover every combination without spawning a process.

The built-in adapter converts the existing `JudgeRequest` into a `Check` frame and the existing `JudgeOutcome` into `Decision`/`Error`. It must preserve the current `NOTICE:` and `BLOCKED` framing, the 1,000-character cap, the allowlist override flag, the bypass event, and the cascade behavior. It keeps the judge's own operation deadline (`timeout_secs + retry_budget_secs`) rather than the external per-check timeout, so current retry behavior is preserved. `ToolContext.judge` is replaced by a `ToolContext.gates` pipeline; the adapter holds the judge configuration internally so the tool code no longer knows about rubrics or providers.

Add a fake gate fixture under `tests/fixtures/gates/`. It is a POSIX shell executable selected by `FAKE_GATE_MODE` and reads frames with `read`. It implements `describe`, `serve`, `init`/`ready`, and `check`; modes return allow, warn, block, abstain, malformed JSON, a delayed response, or a nonzero exit. Tests use it to prove composition, fail-closed behavior, restart, and the process-level circuit breaker without a provider.

Add the telemetry sink for `gate_startup`, `gate_decision`, and `gate_evidence`, with the one-release `judge_*` mapping. In slice 1 the only gate is the built-in judge, so the legacy records remain the primary surface; generic records appear beside them.

### Slice 2: build the reference gate

Create `cake-safety-judge` as a separate repository after the contract is accepted. Use Go with the standard library only for the first version: `encoding/json`, `net/http`, `os/exec` is not needed, and `flag` for the CLI. The tree is `cmd/cake-safety-judge/`, `internal/protocol/`, `internal/gate/`, `internal/judge/`, `internal/rubric/`, `internal/typesafe/`, `corpus/`, `bench/`, `docs/protocol.md`, `justfile`, and `bin/install`/`bin/doctor`.

Start with `describe` and `check`. Port the rubric text and the verdict vocabulary from Cake. Port the corpus and the offline tests. Implement a provider client for the descriptor's `api_type`, starting with Chat Completions and adding Responses before parity sign-off. Match ADR 018's strict verdict parsing: one JSON object, one optional Markdown block with an empty or `json` tag, no trailing data, no repair. Match ADR 020's one bounded recovery for timeout, retryable transport/HTTP failure, undecodable 2xx body, and transport failure during the 2xx body read; never retry a valid verdict, refusal, or malformed verdict.

Implement a deterministic fake provider and a recorded-response provider for offline tests. `check --json -- '<command>'` prints one decision and exits with the same class mapping Cake documents. The corpus harness compares the gate's decision to the oracle over every case and writes a JSON report with counts by case, decision, and code.

Do not add `serve` until the `check` decisions match. Then add the NDJSON loop with one goroutine per check or a bounded worker pool, matching IDs, out-of-order responses, graceful `shutdown`, and a clean exit on closed stdin. Validate every fixture against `docs/schemas/cake-gate-1.schema.json` or a checked-in copy of it.

### Slice 3: shadow evaluation

Add `[gates]` parsing to Cake only as far as the shadow comparison needs. The dual-path recipe is `[tools.bash] gates = ["builtin-safety", "safety"]`, where `builtin-safety` is a reserved name for the in-process judge (defined in `docs/configuration.md`) and `safety` is `mode = "shadow"`. The reserved name is temporary and is removed in slice 5. The external gate receives the same `Check` input as the built-in gate; both are recorded as `gate_decision` with the same `call_id` digest.

Compare on the frozen corpus first, then on live sessions with explicit authorization. The differential report classifies each pair as exact agreement, safe divergence (both allow, or oracle warn vs gate allow with no safety loss), unsafe divergence (oracle block vs gate allow), or gate failure. The acceptance bound is zero unsafe divergence. Record latency distributions for the in-process and external paths, the gate process startup cost, and estimated token spend. Do not give the external gate authority in this slice.

Update `docs/integrations.md` and the session-analysis instructions with the paired-record lookup. The report can be a small offline script that reads two NDJSON sidecars or two corpus result files; it must not call a provider by default.

### Slice 4: flip the default

Add the real `[gates]` configuration, validation, and the `cake gates list`/`cake gates doctor` commands. Add `--no-gate` and `CAKE_GATE=off`; map `CAKE_JUDGE=off` for one release. Resolve the default pipeline in this order: explicit `[tools.bash] gates`, an explicit `[tools.bash.judge]` table (the `builtin-safety` gate or a bypass), auto-detected `cake-safety-judge` on `PATH`, the default-on in-process judge, then no gate. The final no-gate state removes Bash and `BashSession` from the registry and writes one startup diagnostic; an existing user with no judge table and no external gate keeps today's behavior. `cake init` may print a commented gate example but does not write an active gate block.

Add the `doctor` checks: command resolution, `describe` protocol and fields, transport and capability consistency, `init`/`ready`, model resolution, missing environment variable names (not values), and phase/action coverage. `doctor` must not call a provider unless the user passes an action after `--`. `list` is informational and exits `0` when the settings load, even when a gate is broken; `doctor` exits `0` healthy, `1` configuration/coverage, `2` gate process failure, and `3` settings load failure.

Map `[tools.bash.judge]` to the reserved `builtin-safety` gate for the dual-path release and warn when both it and `[tools.bash] gates` are present. An explicit judge table wins over auto-detection; the external gate does not inherit it, so its rubric, TypeSafe settings, and `TYPESAFE_AI_API_KEY` belong in `[gates.safety].config` and `[gates.safety].env`. Keep the emergency bypass and allowlist working through the mapping. Update the docs status notes from "not yet enforced" to current behavior, and add the gate section to `ARCHITECTURE.md`'s host/model-command boundary.

### Slice 5: delete the in-process implementation

Remove `src/clients/judge.rs`, `judge_observer.rs`, `judge_rubric.rs`, `judge_rubric.md`, `typesafe.rs`, the judge tests, and the corpus and benchmark code from Cake. `cake bash check` becomes a gate client: it builds the same `Check` frame, runs the configured pipeline, and renders the existing document fields from the pipeline outcome. The `builtin-safety` reserved gate disappears.

Move corpus, benchmark, SLO, and rubric ownership to `cake-safety-judge`; the gate repository already has copies from slice 2, so Cake deletes rather than moves them again. Update the `analyzing-cake-sessions` skill and `scripts/session-metrics/` to read `gate_decision`/`gate_evidence`, while keeping readers for old sidecars. Remove the one-release `judge_*` emission only after the compatibility window.

Update `ARCHITECTURE.md`, `docs/security.md`, `docs/configuration.md`, `docs/integrations.md`, and the domain glossary. Add a glossary entry only if a real conflation occurs; the glossary bar is evidence, not anticipation. Verify `cargo build` without the judge modules, `cake gates doctor` green, and a corpus run through the external gate.

## Concrete Steps

All Cake commands run from `/Users/travisennis/Projects/cake/cake` unless a worktree path is named. Slice 0 verification, from the contract worktree:

```sh
cd /Users/travisennis/Projects/cake/cake/.cake/worktrees/docs/gate-protocol
python3 -m json.tool docs/schemas/cake-gate-1.schema.json > /dev/null
just docs-check
git diff --check
git status --short --branch
```

Expect the JSON parse to be silent, the Markdown checks to pass, and the branch to be `docs/gate-protocol` with only the ADR, the three documents, the schema, and this plan changed. `just pre-push-docs` is the pre-push route for a documentation-only push; `just docs-check` covers the tracked corpus. If the Markdown formatter requests a change, run `panache format` on the changed files and rerun the check.

Slice 1, after the code lands:

```sh
cd /Users/travisennis/Projects/cake/cake
cargo test gates
cargo test judge
cargo test bash
just check
```

The expected result is that the existing judge, Bash, and CLI tests pass with unchanged expectations, and the new gate tests cover allow, warn, block, abstain, malformed, timeout, crash, and fail-open. `cake bash check --json -- 'git status --short'` still prints the pre-gate document fields with no new required fields and exits `0`.

Slice 2, from the new repository:

```sh
cd /Users/travisennis/Projects/cake/cake-safety-judge
go test ./...
go vet ./...
go run ./cmd/cake-safety-judge describe
go run ./cmd/cake-safety-judge check --json -- 'git status --short'
```

Expect `describe` to print one object with `"protocol": "cake-gate/1"`, `"actions": ["bash.exec"]`, and the expected transports and capabilities. Expect `check` to print one decision object and to use the same exit classes as the documented `bash check` mapping. With a fake provider, `go run ./cmd/cake-safety-judge corpus parity --offline` writes a report with zero unsafe divergences. A live corpus run requires explicit provider authorization and a real credential.

Slice 3, from Cake:

```sh
cd /Users/travisennis/Projects/cake/cake
cargo run -- bash check --json -- 'git status --short'
jq -c 'select(.type == "gate_decision")' ~/.cache/cake/session-telemetry/<session-id>.ndjson
```

With `[tools.bash] gates = ["builtin-safety", "safety"]` and `[gates.safety] mode = "shadow"`, the document reports the built-in decision and `data.stages` includes the shadow stage. The sidecar has two `gate_decision` records for one call with the same `call_id` digest. The differential report over the frozen corpus must show zero unsafe divergences before slice 4 starts.

Slice 4, from Cake:

```sh
cargo run -- gates list
cargo run -- gates doctor
cargo run -- bash check --json -- 'git status --short'
CAKE_GATE=off cargo run -- bash check --json -- 'git status --short'
```

Expect `list` to show the resolved pipeline, `doctor` to exit `0` with a healthy gate, a normal check to report the deciding gate, and the bypass run to report `bypassed: true`, `gate: null`, and exit `0`. Then remove the external binary from `PATH` in a scratch environment and confirm the startup diagnostic removes Bash instead of running it. Do not spend provider tokens in `doctor` without the explicit action argument.

Slice 5, from Cake:

```sh
cargo test
cargo build
rg -n 'judge_rubric|type_safe_shadow|judge_attempt' src
cargo run -- gates doctor
```

Expect the build and tests to pass with no judge modules, no production references to the deleted symbols, and a green doctor report for the external gate. Corpus and benchmark checks run in the gate repository.

## Validation and Acceptance

Slice 0 is accepted when ADR 049 and the four contract documents are reviewed, the failure-outcome table covers every startup and per-check condition, the schema parses, the documented examples match the schema's required fields and conditionals, and a novice can write a fake gate from the docs. A reviewer should be able to answer: what does Cake send, what may a gate return, what happens on failure, what appears in telemetry, what happens with no gate, and how do `bash check` and exit codes stay compatible. The security review must record the bypass classes and confirm that no secret value crosses the boundary.

Slice 1 is accepted when every existing judge and Bash test passes without changed expectations, the new composition tests cover every decision and failure combination, the fake gate proves allow/warn/block/abstain/malformed/timeout/crash, and `cake bash check` output is unchanged. There must be no user-visible behavior change and no new setting.

Slice 2 is accepted when the standalone gate validates against the schema, its offline tests pass without a provider, and the corpus parity report has zero unsafe divergences from the oracle. `serve` is accepted when concurrent IDs, out-of-order responses, malformed frames, and shutdown are tested.

Slice 3 is accepted when the external gate runs in shadow on the frozen corpus and on authorized live sessions with zero unsafe divergences, the divergence classes are documented, and latency and cost are measured. A single unsafe divergence blocks slice 4.

Slice 4 is accepted when the explicit pipeline, auto-detection, and no-gate removal all work; `CAKE_GATE=off`, `--no-gate`, and `CAKE_JUDGE=off` are recorded and equivalent; `[tools.bash.judge]` maps for one release; `gates doctor` names every failure; and the compatibility tests pass. No path may run Bash ungated when no enforce gate exists.

Slice 5 is accepted when Cake builds and tests without the judge modules, `bash check` delegates to the gate, the corpus and benchmark live in the gate repository, and session analysis reads both `gate_*` and old `judge_*` records.

## Idempotence and Recovery

Documentation edits are safe to repeat. `ahm adr create` is not: ADR 049 already exists, so subsequent edits use the file directly. Do not allocate another ADR number for a revision; update 049's body or status.

If the contract branch is abandoned, `just worktree-rm docs/gate-protocol` after switching away; the branch can be recreated from `origin/master`. Commit documentation work in small Conventional Commits and do not merge without review.

Slice 1 lands behind the existing settings, so it can be reverted as one branch without a user-visible migration. The built-in judge remains available throughout slices 1--4, so a failed external gate experiment can return to `[tools.bash.judge]` and the in-process path. Slice 4's default flip is reversible with `[tools.bash] gates = ["builtin-safety"]` during the dual-path release and with `CAKE_GATE=off` as the emergency bypass; the bypass is recorded and is not a recommended steady state.

Slice 2 creates a new repository with no data migration. The corpus is copied from Cake, not moved, until slice 5 deletes the Cake copy. If parity fails, keep the external gate in `check`-only mode and do not add `serve` or shadow authority.

Slice 3's differential report is read-only over corpus results and sidecars. Re-running it is safe. Live-session comparison requires provider spend and explicit authorization; it is not part of an offline check.

Slice 5 is destructive to Cake source but not to sessions or telemetry. Old sidecars and session files remain readable. Delete the judge modules only after one release of dual-path evidence; if a regression appears, restore the previous release from git rather than editing old sessions.

## Artifacts and Notes

A minimal check request and decision, matching the documented examples:

```json
{"protocol":"cake-gate/1","type":"check","id":"01J...","phase":"pre_tool","action":"bash.exec","input":{"command":"git status --short","cwd":"/repo","reason":null,"repo_digest":"git repo, branch main","script_evidence":[]},"session":{"session_id":"9f1c...","task_id":"4b2e...","call_id":"call_...","turn":3},"options":{"diagnostic":false}}
```

```json
{"protocol":"cake-gate/1","type":"decision","id":"01J...","decision":"warn","code":"rg-replace-footgun","message":"Use rg -n so the replace preview can be inspected.","confidence":0.78,"annotations":[{"placement":"prepend","text":"NOTICE: Use rg -n so the replace preview can be inspected."}],"short_circuit":false,"evidence":[]}
```

A healthy doctor report should name each gate, its resolved command, its transport, its mode, and its phase/action coverage, with no `unavailable` or `missing` lines and exit `0`. A missing-gate report should say that Bash is disabled, name `cake-safety-judge` or `[tools.bash] gates`, and mention `cake gates doctor`; no command runs.

Expected `bash check --json` additive shape after slice 4:

```json
{
  "schema_version": 1,
  "command": "bash check",
  "status": "ok",
  "summary": { "verdict": "allow" },
  "checks": [],
  "data": {
    "verdict": "allow",
    "code": null,
    "confidence": null,
    "message": "",
    "overridden": false,
    "bypassed": false,
    "latency_ms": 12,
    "stage": "judge",
    "probability": null,
    "observation": { "outcome": "absent", "probability": null, "failure_class": null, "elapsed_ms": null },
    "gate": "safety",
    "stages": [
      { "gate": "safety", "version": "0.1.0", "mode": "enforce", "decision": "allow", "code": null, "latency_ms": 12, "overridden": false, "failure_class": null }
    ]
  }
}
```

## Interfaces and Dependencies

Cake's new crate-visible interfaces, with stable paths:

- `crate::gates::protocol`: serde types mirroring the schema: `Describe`, `InitFrame`, `ReadyFrame`, `CheckFrame`, `DecisionFrame`, `ErrorFrame`, `ShutdownFrame`, `GateDecision`, `GateErrorClass`, and bounded text helpers.
- `crate::gates::runner::GateProcess`: `async fn describe`, `async fn start`, `async fn check`, `async fn shutdown`; owns the child, the frame reader/writer, deadlines, and restart policy.
- `crate::gates::composer::GatePipeline`: `async fn check(&self, request: GateRequest) -> PipelineOutcome`; `PipelineOutcome` carries the final decision, the annotations, and one `StageOutcome` per gate.
- `crate::gates::judge_gate::BuiltinJudgeGate`: implements the same gate interface over `evaluate_command_observed`, preserving the current judge behavior and legacy telemetry.
- `crate::config::gates`: `GateSettings`, `ResolvedGates`, validation, defaults, precedence, and the `[tools.bash] gates` composition.
- `crate::session_telemetry`: `GateStartupTelemetry`, `GateDecisionTelemetry`, `GateEvidenceTelemetry`, and the sink methods; the existing `JudgeAttemptSink` and compensation types stay for the compatibility release.
- `crate::cli::gates`: `gates list`, `gates doctor`, and the `--no-gate` wiring.
- `src/clients/tools/mod.rs`: `ToolContext.gates` replaces `ToolContext.judge`; the tool layer sees only decisions, annotations, and failure classes.

The new Go repository's packages:

- `internal/protocol`: wire types and bounds, generated or hand-written from the schema.
- `internal/gate`: `describe`, `serve`, `check`, frame IO, concurrency, and shutdown.
- `internal/judge`: provider request/response, strict verdict parser, bounded recovery, and refusal handling.
- `internal/rubric`: the embedded rubric and verdict vocabulary, with a version/hash recorded in decisions.
- `internal/typesafe`: the optional cascade stage, added only after base parity.
- `internal/corpus`: JSONL loading, parity comparison, and the report.
- `bench`: the offline SLO harness; no provider calls by default.

Dependencies: Cake uses the existing `serde`, `serde_json`, `tokio`, and `sha2` crates; the gate protocol needs no new Cake dependency. The Go gate uses the standard library for v1, which keeps supply-chain surface and build time low. Model provider access is HTTP through the descriptor's `base_url` and `api_key_env`; no SDK is required.

The interface is intentionally narrow: a gate never receives a sandbox resolver, a filesystem handle, a tool executor, or a secret value. It receives the action packet Cake assembled, returns a decision, and may return bounded evidence. Cake retains every enforcement decision and every persisted record.

## Revision notes

2026-10-25: the contract review changed the default pipeline to explicit configuration first with the default-on in-process judge as the fallback, defined `fail` as a per-check setting with startup failures always pipeline-fatal and the circuit breaker honoring `fail`, opened capabilities and error classes for additive evolution, added `gate_startup` and the `disabled`/`failed_open` stage fields, defined the `bash check` deciding-gate and latency mapping, and recorded the all-abstain consequence. The branch merged `5e0ffe8` before the final push.

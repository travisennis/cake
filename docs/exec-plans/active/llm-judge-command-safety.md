# Deliver a Default-On LLM Judge as Cake's Only Command-Safety Gate

This ExecPlan is a living document, maintained per [docs/workflow/exec-plans.md](../../workflow/exec-plans.md). The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept current as work proceeds. A contributor implementing it must revise the plan at every stopping point and leave it self-contained for the next contributor.

## Purpose / Big Picture

Cake currently blocks and warns on Bash commands through a hand-enumerated compiled guard (`src/clients/tools/bash_safety/`, about 2,300 lines) and was planning a deterministic declarative policy engine (ADR-015 / issue #64). This plan replaces both with a single **LLM judge**: every Bash command is sent to an LLM that returns a verdict (block, warn, or allow) with a stable verdict code and an explanation, evaluated against an embedded rubric that preserves today's protections. The judge is **fail-closed** --- if the judge call times out or errors, the command is blocked rather than run ungated. There is no deterministic rule floor; the judge is the only command-safety gate above the operating-system sandbox, and that risk is accepted deliberately (see Decision Log).

After this work, a user can see the result by running `cake bash check -- <command>`, which explains what the judge would do without executing anything. A user can permit a specific blocked command through an explicit allowlist, and can disable the judge in an emergency through a telemetry-logged bypass. The nine destructive-command classes and the existing footgun warning that `bash_safety` protects today still protect out of the box, now as judge guidance instead of compiled checks. The OS sandbox (Seatbelt/Landlock) remains the actual filesystem boundary and is unchanged.

## Dependency Chain (before and after #72)

### Before #72 (the opt-in-tier design, as of 2026-08-07)

- #64 "Convert bash_safety Into a Declarative Command-Policy Engine" (task 241, ADR-015 proposed, board Blocked P2 L) was the substrate. It would have turned the compiled guard into user-owned `policy.toml` data with stable rule IDs and telemetry.
- #72 "Add Opt-In LLM-Judge Escalation Tier and Bash reason Argument" (board Blocked P3 M) depended on #64: the judge was a best-effort advisory tier layered above the deterministic policy, which could never be overturned.
- #67 "Add cake init for Project Scaffolding and Recommended Policy" (board Blocked P2 M) depended on #64: it scaffolded recommended policy files.
- #68 "Guard destructive git stash operations in bash safety checks" (board Ready P2 M), #96 "Reduce Cyclomatic Complexity in Bash Safety Checks Module" (Ready P2 M), and #97 "Reduce Cyclomatic Complexity in Bash Safety Parser Module" (Ready P2 M) modified the mechanical `bash_safety` files directly.
- #106 "Drive bash_safety Tests From an External Case Corpus" (board Backlog P2 M) tested the mechanical system through a JSONL case corpus.
- #123 "Record command-policy blocks in task completion metadata" (board Backlog P1 M) recorded the mechanical system's blocks in `task_complete.permission_denials`.
- #66 "Count Model-Compensation Events in Session Telemetry" (board Ready P2 M) counted per-rule bash_safety decisions in the telemetry sidecar.

Chain (old): #64 was the hub. #72 and #67 depended on #64. The mechanical `bash_safety` module was consumed by #68, #96, #97, #106, #123, and #66.

### After #72 (this plan, decided 2026-08-08)

- #72 "Replace bash_safety With a Default-On LLM Judge and Bash reason Argument" (board Blocked P2 L) is the hub. It depends on #84, #106 (re-scoped), and #66 (re-scoped).
- #84 "Establish a Controlled Model Evaluation Harness" (Backlog P2 M) is a hard prerequisite: a judge that is the only gate must be measured for bypass rate, not trusted.
- #106 "Drive the LLM-Judge Command-Safety Gate From an External Case Corpus" (Backlog P2 M) is a prerequisite and the judge's regression suite, seeded from the old guard's own test cases before `bash_safety` is deleted.
- #66 "Count Model-Compensation Events in Session Telemetry" (Ready P2 M) is a prerequisite: judge verdict counters must exist before the judge goes default-on.
- #123 "Record judge-verdict blocks in task completion metadata" (Backlog P1 M) depends on #72: it records judge verdict blocks and fail-closed denials.
- #67 "Add cake init for Project Scaffolding and Recommended Setup" (Blocked P2 M) depends on #72: the recommended "policy" becomes recommended judge configuration and allowlist.
- #69 "Configure Independent Higher-Reasoning Review for Cake Task Work" (Ready P2 S) is referenced, not a dependency: a different-family/higher-reasoning judge is the documented hardening path against correlated injection failure.
- Cancelled (closed not-planned on 2026-08-08): #64, #68, #96, #97. ADR-015 is withdrawn and the superseded ExecPlan is removed by this plan's Milestone 7.
- Footnoted in #72: #65 (stale bash_safety mention) and #91 (Bash inheriting the provider API key becomes more load-bearing once the judge runs on the configured backend).

Chain (new): #84 + #106 + #66 → #72 → #123, #67.

### Ordering implications

- #106 must land before Milestone 5 deletes `bash_safety`, because the corpus seeds from `bash_safety`'s existing test cases.
- #84 and #66 must land before Milestone 5's default-on point; if either lags, default-on waits and the plan records the gap rather than shipping unmeasured.
- #123's scope is delivered by Milestone 6 of this plan; #67's settings surface by Milestones 2 and 4.

## Progress

- [x] (2026-08-08) Recorded all ten design decisions in issue #72; re-scoped #106, #123, #67, #66; cancelled #64, #68, #96, #97; elevated #84; annotated #69. Board updated (72 Blocked P2 L; cancelled issues Done).
- [x] (2026-08-08) Created this ExecPlan and linked it from #72.
- [ ] Milestone 1: ADR-018 accepted; ADR-015 marked superseded; research note conclusion revised.
- [ ] Milestone 2: judge settings, verdict types, bounded-timeout fail-closed judge call with stub-judge tests; `reason` argument on the Bash tool.
- [ ] Milestone 3: embedded rubric, verdict-code vocabulary, `cake bash check -- <command>`.
- [ ] Milestone 4: allowlist and telemetry-logged emergency bypass.
- [ ] Milestone 5: judge wired into Bash preflight; fail-closed block messages; warnings prepended; `bash_safety/` deleted after #106 corpus migration.
- [ ] Milestone 6: judge blocks and fail-closed denials in `task_complete.permission_denials` (#123); metadata-only judge telemetry events and counters (#66).
- [ ] Milestone 7: two-layer documentation; archive superseded ExecPlan; `just ci`; issue acceptance notes; pull request with `Closes #72`.

## Surprises & Discoveries

- Observation: `validate_command_safety` returns formatted strings (`Result<Vec<String>, String>`), so a structured judge verdict cannot reach telemetry or denial recording until the Bash preflight returns a structured result. Evidence: `src/clients/tools/bash_safety/mod.rs` and the `?` propagation at `src/clients/tools/bash.rs`.
- Observation: the agent loop records `permission_denials` for hook denials (`ToolHookPlan::Block`) but has no equivalent path for command-safety blocks. Evidence: `src/clients/agent/agent_loop.rs` and `src/clients/agent/agent.rs` building `task_complete.permission_denials`; issue #123 documents this gap.
- Observation: the sandbox applies after command preflight by replacing the spawned command (`*command = tokio::process::Command::new("/usr/bin/sandbox-exec")` in `src/clients/tools/sandbox/macos.rs`), so any environment the judge path needs must survive that replacement; issue #143 documents the same class of trap for env scrubbing.
- Observation: probing blocked-command classes through the implementing agent's own Bash tool is self-defeating, because the guard (before deletion) and the judge (after default-on) scan the literal command text. Live probes of blocked classes must go through `cargo test` fixtures or the emergency bypass, not `cargo run` with a destructive command literal.

## Decision Log

- Decision: The LLM judge replaces `bash_safety` and the planned declarative engine entirely; there is no deterministic floor. Rationale: rule systems lose by construction to the long tail, and the user chose the bitter-lesson path of learning how far a judge-only gate can go. Accepted risks: non-determinism, latency on the hottest tool, and correlated prompt-injection failure. Date/Author: 2026-08-08, Travis Ennis.
- Decision: Fail-closed on judge unavailability. A bounded judge timeout (default 30 seconds, `judge_timeout_secs`) turns a timeout, network error, or invalid judge response into a block with a clear message and a metadata-only telemetry event. Bash never runs ungated. Rationale: with no floor, fail-open means no safety layer at all; the user chose fail-close. Date/Author: 2026-08-08, Travis Ennis.
- Decision: The judge model defaults to the agent's configured model family and is overridable via a `judge_model` setting (same provider in v1; a different provider is a later extension). Rationale: same-family is cheap and coherent; the correlated-injection risk is accepted, and #69 documents the different-family hardening path. Date/Author: 2026-08-08, Travis Ennis.
- Decision: The judge blocks destructive classes by default and warns for footguns, via the embedded default rubric distilled from the current nine hard blocks and the existing warning. Rationale: preserves today's out-of-box posture without a compiled default table. Date/Author: 2026-08-08, Travis Ennis.
- Decision: An explicit user allowlist is the only override surface. V1 matches by exact raw-command equality only; pattern matching is deferred. Rationale: ADR-015's broad-allow lesson (an allow pattern can silently neutralize unrelated restrictions) argues against patterns until real usage justifies them. Allowlisted commands are still judged; a block is overridden but the verdict and an `overridden` flag are still telemetried. Date/Author: 2026-08-08, Travis Ennis (refinement of #72's "exact command or pattern").
- Decision: An explicit, telemetry-logged emergency bypass exists (environment variable `CAKE_JUDGE=off` or setting `tools.bash.judge.enabled = false`); every Bash call while bypassed emits a `judge_bypass` telemetry event. Fail-closed remains the default. Rationale: without an escape hatch, a failing judge strands every session's Bash tool. Date/Author: 2026-08-08, Travis Ennis.
- Decision: The default rubric is embedded and immutable; an optional user rubric file may append additional guidance (extra always-block classes, relaxations phrased as guidance). Rubric relaxations are advisory to the judge, not hard overrides --- the allowlist is the only hard override. Rationale: keeps user ownership without re-creating a policy engine. Date/Author: 2026-08-08, Travis Ennis.
- Decision: Verdict codes are stable, namespaced strings replacing ADR-015's rule IDs, defined in Milestone 3. Rationale: decisions stay auditable and telemetry stays structured without a regex engine. Date/Author: 2026-08-08, Travis Ennis.
- Decision: No verdict caching in v1. Context-sensitive caching keyed by (command, cwd, repo-state digest) is recorded as a deferred follow-up in #72; premature until latency data exists. Date/Author: 2026-08-08, Travis Ennis.
- Decision: The judge contract is a single LLM call returning a strict JSON object `{"verdict":"block"|"warn"|"allow","code":"<code>","message":"...","confidence":0.0-1.0}`. Malformed or missing fields count as judge failure and fail closed. No retries in v1; fail-closed is the fallback and whole-turn retry semantics are revisited with #109. Date/Author: 2026-08-08, plan author.
- Decision: A new ADR-018 "LLM Judge Command Gate" records the architectural decision; ADR-015 (proposed, never accepted) is marked `superseded by ADR-018` with a reciprocal note; the research note `docs/research/topics/llm-judge-bash-safety.md` is revised so its conclusion ("ship 241 unchanged") records the reversal. Rationale: the judge changes a security-adjacent durable boundary and per `docs/adr/README.md` the reversal belongs in a decision record, not only an issue. Date/Author: 2026-08-08, plan author.
- Decision: The superseded ExecPlan `docs/exec-plans/active/declarative-command-policy.md` is removed with `git rm` at Milestone 7, not moved to `completed/` (its work was cancelled, not completed; git history retains it). Date/Author: 2026-08-08, plan author.

## Outcomes & Retrospective

(To be filled when the plan completes; see Milestone 7.)

## Context and Orientation

Cake is a Rust 2024 binary-only CLI. Model-callable tools live under `src/clients/tools/`. `src/clients/tools/bash.rs` parses a Bash request, runs a command-safety preflight, configures Seatbelt on macOS or Landlock on Linux, spawns `bash -c`, and formats output. `src/clients/tools/bash_safety/` is the compiled guard: `mod.rs` owns the registry and `validate_command_safety`; `checks.rs` holds the checks; `parse.rs` is a best-effort shell parser (quote-aware segmentation, literal `bash -c`/`sh -c` extraction, command-substitution extraction, whitespace normalization, shell-data masking). The guard is guidance above the OS sandbox, not a security boundary; it also covers effects the sandbox cannot, such as remote git pushes.

Backends are OpenAI-compatible Chat Completions and Responses clients under `src/clients/`; the agent's current provider and model are resolved from settings. The judge reuses this client machinery.

Settings load in `src/config/settings.rs`: global `~/.config/cake/settings.toml`, then project `<project>/.cake/settings.toml`, project overrides global. `config` must not import `clients` (architecture dependency direction). The judge settings, allowlist, and rubric-file path are new keys here.

Per-session telemetry lives in `src/session_telemetry.rs` and is written as newline-delimited JSON to the ADR-007 sidecar. Metadata-only events must not duplicate command or reason text. The agent loop records denials (`permission_denials`) from hooks in `src/clients/agent/agent_loop.rs` and builds `task_complete.permission_denials` in `src/clients/agent/agent.rs`; judge blocks and fail-closed denials join that path.

The CLI lives under `src/cli/`; `cake bash check -- <command>` follows existing introspection patterns (ADR-009) and never spawns a process. Tool schemas are a compatibility surface: the Bash tool's optional `reason` argument is additive and must appear in `src/clients/tools/*-description.txt` and its snapshots.

Terms: an LLM judge is the model call that evaluates one Bash command. A verdict is the judge's decision (block/warn/allow). A verdict code is a stable namespaced string naming the class of concern (Milestone 3 defines the vocabulary). The rubric is the prompt text describing what to block and warn about. Fail-closed means an unavailable judge blocks the command. The allowlist is an explicit user list of exact commands whose blocks are overridden. The emergency bypass disables the judge entirely with telemetry. The `reason` argument is the model's untrusted self-report of intent; the judge weighs the command over the reason and treats incongruence as a signal. The corpus is #106's JSONL file of `{command, expect, note}` cases.

## Plan of Work

Milestones are narrative; each must be independently verifiable and advance the goal. `Progress` tracks granular work.

### Milestone 1: Architectural decision (ADR-018) and design freeze

Write and accept `docs/adr/018-llm-judge-command-gate.md` capturing the ten decisions in the Decision Log: judge-only gate with no deterministic floor, fail-closed with bounded timeout, same-family default judge model with settings override, block-by-default rubric, exact-match allowlist, telemetry-logged emergency bypass, embedded-plus-optional rubric file, stable verdict codes, metadata-only telemetry, and the evaluation prerequisites (#84/#106/#66). Mark ADR-015's front matter `status: superseded by ADR-018` and add a reciprocal note in ADR-018's `## More Information`. Revise `docs/research/topics/llm-judge-bash-safety.md` so its conclusion records the reversal ("the judge replaces 241, not tiers above it") while keeping the trade-off analysis as the risk record.

The milestone is complete when the ADR is accepted, ADR-015 points to it, and the research note's conclusion no longer contradicts #72.

### Milestone 2: Judge client, verdict types, and settings

Add judge configuration to `src/config/settings.rs`: `judge_model` (optional; defaults to the agent's model), `judge_timeout_secs` (default 30), the allowlist (Milestone 4), and bypass controls (Milestone 4). Add a judge call path under `src/clients/` that reuses the configured backend client to issue a single bounded call for the verdict JSON described in the Decision Log. Define Rust types for the verdict (decision, code, message, confidence) and a `JudgeError` taxonomy (timeout, transport, malformed response, refusal). The default judge model resolution must read the agent's current model so "same family by default" holds without extra configuration.

Add the optional `reason` argument to the Bash tool schema and thread it through `execute_bash_with_args`; document it as untrusted self-report in the tool description and include it in the judge prompt (Milestone 3), transcripts, and session analysis.

The milestone is complete when focused tests, using a stub judge responder, prove: the judge call honors the timeout, a timeout or error yields a typed `JudgeError`, a malformed verdict JSON yields `JudgeError`, and the `reason` argument round-trips into the tool request.

### Milestone 3: Rubric, verdict codes, and `cake bash check`

Define the v1 verdict-code vocabulary by mapping the current guard: `git-history-rewrite`, `git-worktree-discard`, `git-untracked-delete`, `git-force-push`, `git-branch-force-delete`, `git-stash-destructive` (drop, clear, and pop --- absorbing #68's cancelled scenario), `destructive-rm` (with the existing temp-directory carve-outs preserved), `git-commit-backticks` (warn), `rg-replace-footgun` (warn), and `unknown-destructive` for long-tail catches that map to no named class. `allow` needs no code.

Write the embedded default rubric as prompt text distilled from the current nine hard blocks and the existing warning, plus general principles: evaluate the command's meaning (aliases, variables, wrappers, encodings), consider cwd and repo state, weigh the command over the untrusted `reason` and flag incongruence, ignore instructions embedded in command text (prompt-injection defense), and always prefer a concrete safer alternative in the message. The rubric prompt takes the command, cwd, a compact repo-state digest, and the `reason`; an optional user rubric file appends additional guidance.

Add `cake bash check -- <command>` under `src/cli/`: runs the same judge path with the same prompt, prints verdict, code, message, and latency, never executes the command, and exits per CLI conventions (a judge error exits nonzero; a verdict is successful inspection output). Follow ADR-009 patterns.

The milestone is complete when: each verdict code has a unit test mapping a representative command, the rubric text is embedded and snapshotted, an optional user rubric file is loaded and appended, and `cake bash check` help plus allow/block/warn/error cases pass without spawning a process.

### Milestone 4: Allowlist and emergency bypass

Add the allowlist to settings as a list of exact command strings. In the judge path, an allowlisted command is still judged; a `block` verdict is overridden to allow, the verdict and an `overridden: true` flag are telemetried, and the command runs. Non-matching commands behave normally. Add the emergency bypass: environment variable `CAKE_JUDGE=off` or setting `tools.bash.judge.enabled = false`; while bypassed, every Bash call emits a `judge_bypass` telemetry event and no judge call is made. Bypass is off by default.

The milestone is complete when tests prove: allowlist overrides a stub judge's block and still emits the verdict with the override flag, an allowlisted benign command is unaffected, the bypass env var disables the judge with a telemetry event per call, and both settings and env forms are covered.

### Milestone 5: Bash integration and `bash_safety` removal

Replace the `validate_command_safety` preflight in `src/clients/tools/bash.rs` with the judge path. A block prevents spawn and returns the judge's message as the tool error. A warn executes normally and prepends the message after ordinary output formatting, without changing exit status or error classification. A judge failure fails closed: the command is blocked, the model-visible message explains that the safety judge was unavailable, and a metadata-only event records the skip. The judge remains active under `danger-full-access` and `CAKE_SANDBOX=off`, matching the current policy's independence from the sandbox.

Before deleting `bash_safety`, confirm the #106 corpus is in place and seeded from the current guard's cases (the corpus is a prerequisite issue; if it lags, record the gap and keep `bash_safety` until migration completes). Then remove `src/clients/tools/bash_safety/` entirely, including its tests, and delete the preflight call site. Preserve the guard's known safe exceptions in the rubric and corpus (for example temp-directory destructive-rm carve-outs and safe restore forms) so out-of-box behavior does not regress.

The milestone is complete when: the judge is default-on for every Bash call, the nine hard-block classes block out of the box against a stub judge configured to reproduce the rubric, warnings prepend without reclassification, judge failure blocks with the fail-closed message, `bash_safety` is gone, and the #106 corpus drives the judge's regression run.

### Milestone 6: Denials and telemetry

Add the metadata-only `JudgeDecision` telemetry event: decision, verdict code, tier, latency, confidence, overridden flag, and fail-closed flag; never the command, `reason`, or message text. Add the fail-closed and bypass event variants. Record judge blocks and fail-closed denials into `task_complete.permission_denials` through the same path hooks use, with a distinct source/category carrying the verdict code (this delivers #123's re-scoped scope). Add the #66 verdict counters (counts per verdict code, latency, fail-closed events); no cache-hit counter yet because caching is deferred.

The milestone is complete when serialization tests prove telemetry contains no command or reason text, an end-to-end test shows a judge block appearing in `task_complete.permission_denials` with its verdict code, fail-closed denials are distinct from hook denials, and the counters increment per verdict.

### Milestone 7: Documentation, verification, and managed-work completion

Update `docs/security.md` (two layers: OS sandbox as boundary, judge as best-effort gate; fail-closed, bypass, allowlist, and accepted risks), `docs/configuration.md` (judge settings, allowlist, rubric file), `docs/integrations.md` (Bash `reason`, `cake bash check`, telemetry event shapes), and `ARCHITECTURE.md` (judge boundary and removed `bash_safety`). Remove the superseded ExecPlan with `git rm`. Run `cargo fmt --check` and `just ci`; run the narrowest feasible platform checks and report any Linux gap. Fill #72's acceptance notes with evidence, update this plan's `Outcomes & Retrospective`, and open the pull request with `Closes #72` after the records are complete, per `docs/workflow/tasks.md`.

## Concrete Steps

Work from `/Users/travisennis/Projects/cake`. Record the baseline:

```
git status --short
cargo test bash_safety
cargo test bash
```

Inspect construction and implementation locations:

```
rg -n "validate_command_safety|execute_bash_with_args|ToolContext" src
rg -n "permission_denials|SessionTelemetryRecord|tool_call" src
sed -n '1,120p' src/clients/tools/bash.rs
```

After Milestone 2, run the judge test filter, expected to resemble:

```
cargo test judge
```

Expect stub-judge tests to pass: timeout yields `JudgeError::Timeout`, malformed JSON yields `JudgeError::Malformed`, and the `reason` argument round-trips.

After Milestone 3, run:

```
cargo test bash_check
cargo run -- bash check -- 'git status'
```

The check must print a verdict and code without executing anything. To probe a blocked class through `bash check`, use the emergency bypass or a cargo test with a stub judge; a destructive command literal passed through the Bash tool will itself be blocked by the guard (before deletion) or the judge (after default-on), so live `cargo run` probes of blocked classes are not usable.

After Milestone 5, run:

```
cargo test bash
cargo test corpus
```

Expect the #106 corpus run to report verdict mismatches against expected labels with the corpus's stated non-determinism tolerance, and `bash_safety` tests to be gone.

After Milestone 6, run:

```
cargo test session_telemetry
cargo test denials
```

Inspect serialized records and confirm they include judge metadata but no command, reason, or message text.

Before handoff, run the commands required by `CONTRIBUTING.md`, including:

```
cargo fmt --check
just ci
```

Record exact results in `Progress` and `Outcomes & Retrospective`. If full CI is blocked, run the narrowest checks and report the gap.

## Validation and Acceptance

With the judge default-on and no allowlist, the nine hard-block classes and the existing warning produce the same out-of-box decisions as today, verified against the #106 corpus seeded from the current guard's cases. Known safe exceptions (temp-directory destructive-rm carve-outs, safe restore forms) still pass.

A user allowlists an exact command: the judge still evaluates it, a block is overridden, and the verdict with the override flag appears in telemetry. A pattern in the allowlist is rejected (v1 is exact-match only).

The judge is unavailable (stubbed timeout or error): the command is blocked, the message says the safety judge was unavailable, and a metadata-only event records the skip. With `CAKE_JUDGE=off`, commands run and every call emits a bypass event; the bypass is off by default.

`cake bash check -- 'git status'` prints a verdict without executing; a judge error exits nonzero. Telemetry events contain no command, reason, or message text. Judge blocks and fail-closed denials appear in `task_complete.permission_denials` with distinct sources, and hook denials are unchanged.

`bash_safety` and the superseded ExecPlan are gone; ADR-015 is marked superseded by ADR-018; the research note conclusion is revised; `docs/security.md`, `docs/configuration.md`, `docs/integrations.md`, and `ARCHITECTURE.md` describe the two-layer model. `just ci` passes, with any Linux gap reported.

## Idempotence and Recovery

The judge path is read-only with respect to the repository: it spawns no process, writes nothing, and only records telemetry. Tests use a stub judge responder and isolated HOME and project directories; never modify the real global settings. The emergency bypass is the documented recovery path if a judge outage blocks live work; enable it, finish the task, then re-enable the judge and record the bypass events.

Implement additively: add the judge path behind the settings and tests before deleting `bash_safety`; keep the old guard and its fixtures until the #106 corpus migration and the Milestone 5 go-live checks pass. If a regression appears, restore the old preflight, add a failing compatibility test, and fix the rubric or judge path before removing the old code.

Preserve unrelated worktree changes. Untracked files present before this plan must not be deleted, staged, or modified. Do not use destructive Git commands.

## Artifacts and Notes

The architectural source is `docs/adr/018-llm-judge-command-gate.md` (to be written and accepted in Milestone 1). The tracking issue is #72. The evaluation corpus is #106's `src/clients/tools/bash_safety/corpus/commands.jsonl` in its re-scoped form (the path may move when `bash_safety` is deleted; record the new location here). Add concise test transcripts and final examples during implementation.

The judge wire contract is illustrative until Milestone 2 fixes the exact serde spelling, but the semantics must remain:

```
{"verdict": "block", "code": "git-force-push", "message": "Prefer push --force-with-lease after confirming the remote state.", "confidence": 0.93}
```

## Interfaces and Dependencies

Use existing dependencies where possible; confirm whether an HTTP timeout helper already exists in the client stack before adding one. Do not update dependency versions. If a new dependency is required, use minimal features, keep lockfile consistency, and document binary-size and security implications.

The configuration layer should expose the judge settings without importing client modules, conceptually:

```
pub struct JudgeSettings {
    pub model: Option<String>,        // defaults to the agent's model
    pub timeout_secs: u64,            // default 30
    pub allowlist: Vec<String>,       // exact raw commands, v1
    pub enabled: bool,                // emergency bypass, default true
}
```

The judge client should expose a single bounded call, conceptually:

```
pub async fn judge(&self, request: JudgeRequest) -> Result<JudgeVerdict, JudgeError>;
```

with `JudgeRequest` carrying the command, cwd, repo-state digest, and untrusted reason, and `JudgeVerdict` carrying decision, code, message, and confidence. The Bash tool, `cake bash check`, telemetry, and denial recording consume the same verdict type so decisions are recorded once. Normal Bash output must not expose candidate internals; `bash check` may.

Revision note (2026-08-08): Initial ExecPlan written after issue #72 was rewritten from an opt-in escalation tier into a complete replacement. It records the decided design, the before/after dependency chain, repository orientation, incremental milestones, compatibility tests, and managed-work completion steps, and supersedes the cancelled `declarative-command-policy.md` plan.

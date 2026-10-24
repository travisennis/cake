---
status: proposed
date: 2026-10-24
decision-makers: Travis Ennis
consulted: 033b, 033c
informed: task 128a
---

# External policy gates with the cake-gate/1 protocol

## Context and Problem Statement

Cake's Bash safety judge is an in-process special case. It owns its prompt, rubric, provider client, retries, verdict vocabulary, TypeSafe cascade, and telemetry records, and it is wired directly into `ToolContext` and the Bash preflight. The implementation is roughly 3.5--4k production lines plus about 9k test and corpus lines. Changing the policy strategy --- using a deterministic rule set, a local model, a typed classifier, or a different provider --- means editing Cake core, rebuilding, and shipping a new binary.

Cake cannot win a feature-for-feature race with other coding agents, but it can be a flexible platform for policy experiments. Policy should be an external, composable, versioned gate. Cake should keep what only it can do: assemble trusted context, enforce decisions, manage process lifecycle, compose gates in order, and frame telemetry. A gate should own only the decision: its rubric, prompts, provider calls, verdict codes, and strategy.

The 2026-10-23 design review chose this direction over moving `judge.rs` into a plugin-shaped module or wrapping the judge in a `PreToolUse` hook. The hook wrapper was rejected because it cannot preserve warn semantics, evidence, fail-closed enforcement, or call-level telemetry. This ADR freezes the replacement contract before any code moves. The current in-process judge remains the oracle and the only command gate until the external gate has passed shadow evaluation.

Backlog records 033b (trusted authorization context) and 033c (structured Bash safety-action contract) own the future request fields for user authorization and the normalized action packet. They are dependencies of the complete platform, not of this envelope: their fields land additively, and this protocol must not duplicate them. Record 033 remains the judge-quality tracker and is not re-scoped here.

## Decision Drivers

- Make command policy an external, composable, versioned extension point without weakening Cake's fail-closed command gate.
- Keep Cake as the single owner of enforcement, sandbox semantics, context assembly, composition, and observability.
- Preserve every current safety property: block, warn, abstain, exact-match allowlist override, emergency bypass, fail-closed failures, and metadata-only telemetry.
- Support a fast-approval stage and shadow evaluation as first-class composition, not as special cases inside one judge.
- Let a gate reuse Cake's resolved model configuration without ever receiving a secret value across the process boundary.
- Give a novice a contract concrete enough to implement a gate and a fake gate from the documentation alone.
- Never silently run Bash ungated: a missing gate disables Bash rather than falling back to execution.
- Keep the current judge measurable and comparable while the external implementation is built.

## Considered Options

1. **Keep the judge in process and refactor its internals.** Lower short-term risk and no new process protocol, but it preserves the core problem: strategy, provider, rubric, and verdict vocabulary stay inside Cake, and every experiment requires a Cake change. Rejected.
2. **Wrap the judge in a `PreToolUse` hook.** Small and uses an existing extension point, but hooks are advisory, fail-open by default, cannot carry warn annotations or gate evidence with the right authority, and cannot preserve the current fail-closed and telemetry guarantees without recreating them inside the hook. A throwaway proof of concept at best. Rejected.
3. **A generic `cake-gate/1` protocol plus a standalone `cake-safety-judge` reference gate.** Adds a new enforcement surface with an explicit trust boundary and lifecycle, keeps Cake in charge of enforcement and observability, and makes policy experiments external. Chosen.
4. **Compile each policy as a Rust plugin loaded into Cake.** Avoids a process boundary and keeps types, but couples every experiment to Cake's toolchain, ABI, and release cycle, and gives an untrusted policy code access to Cake's address space. Rejected.

## Decision Outcome

Chosen option 3: Cake gains a generic external gate layer using the versioned `cake-gate/1` protocol, and the current judge is first wrapped behind that seam and later replaced by a standalone `cake-safety-judge` gate.

### Protocol and ownership

A gate is a trusted executable that implements `describe` and `serve`; `check` is an optional one-shot mode for humans and offline corpora. `describe` runs once per gate per run and is validated before any work. `serve` is a long-lived newline-delimited JSON process over standard input and output. A per-check process (`transport = "oneshot"`) is an alternative for simple scripts and experiments; it uses the same frames and lifecycle, with one process per check.

Cake owns command normalization, per-call working-directory resolution, repository state, script evidence, gate process lifecycle, timeouts, restarts, ordered composition, shadow mode, allowlist and bypass policy, telemetry framing and bounding, and the `cake bash check`/`cake gates` CLI. The gate owns its rubric, prompt, provider and model calls, verdict codes, strategy, configuration, corpus, and benchmarks. A gate must not read the workspace to learn about the action or resolve sandbox paths; Cake assembles the action evidence so sandbox semantics have one implementation. A gate may read its own rubric and configuration files named in its config.

The gate request carries Cake's current Bash preflight inputs: the normalized command after trailing-`&` handling, the resolved per-call working directory, the compact repository digest, the untrusted model-supplied reason, collected script evidence, and the session, task, and call identifiers when they exist. It also carries the gate's configuration table, resolved model descriptors for model names the gate declares, and the names of the environment variables Cake makes available. It never carries a secret value. The trusted authorization projection from 033b and the structured action packet from 033c become additive `input` fields when those contracts land; a gate must ignore fields it does not know, and adding fields does not change the version. Removing or changing the meaning of a field requires `cake-gate/2`.

### Decisions are allow, warn, block, or abstain

`allow` continues to the next gate unless the gate's configuration sets `short_circuit_on_allow` and the decision sets `short_circuit`. `warn` accumulates: the command runs with Cake's existing `NOTICE:` prefix and any annotations prepended to the tool output. `block` stops the chain and becomes the model-visible tool error with Cake's existing `BLOCKED` framing, unless that gate's exact normalized-command allowlist overrides it. `abstain` is a deliberate "no opinion" and changes nothing; a gate that cannot evaluate sends an error frame instead, so a failure is never disguised as abstention.

A gate failure in enforce mode blocks. The failure classes are bounded and include timeout, transport, network, authentication, rate limit, refusal, malformed output, protocol violation, process crash, and configuration. Per-gate `fail = "open"` is an explicit experiment-only setting; a failed-open decision is recorded, never silent. Shadow gates run beside the enforce chain and record their would-be decisions without any authority; a shadow failure is recorded only.

### Transport and lifecycle

`describe` and `init` run before any checks. `describe` is cached for the run; `init` runs once per gate process instance, so a restarted process receives a fresh `init`. Requests carry unique identifiers and may be concurrent; responses may arrive out of order and are matched by identifier. A per-request deadline is enforced by Cake. A timeout, crash, malformed frame, or protocol violation kills the process group, fails that check closed, and restarts the process for the next check. Two consecutive process-level failures disable the gate for the rest of the run, so a broken gate cannot impose a startup timeout on every Bash call. `shutdown` is best effort at session end.

### Context, models, and secrets

Cake resolves model descriptors for the names a gate lists. The reserved name `default` is the run's resolved model; the auto-detected safety gate receives it automatically when it declares `resolved-models`, and an explicitly configured gate receives exactly the names it lists. A descriptor includes the provider strategy, base URL, API type, model name, attribution headers, sampling and reasoning controls, and the *name* of the environment variable that holds the credential. It never includes the credential value. Cake spawns the gate with a minimal base environment plus the environment variables named by the gate's `env` list and the `api_key_env` names of its resolved models. The gate reads keys itself. Gate executables are trusted, like hooks and toolbox tools; the environment allowlist is least disclosure, not a sandbox.

### Telemetry and compatibility

Cake emits generic `gate_decision` records, one per gate that ran for the check, and bounded `gate_evidence` records whose `kind` is declared by the gate. A decision record carries the gate name and version, mode, decision, code, latency, override and bypass flags, failure class when applicable, and the one-way digest of the originating tool-call identifier. Command text, reasons, and messages never enter telemetry unless the gate explicitly places them in evidence; Cake bounds every evidence payload.

For one release Cake also emits the existing `judge_verdict`, `judge_fail_closed`, and `judge_bypass` compensation events, plus `judge_attempt` and `type_safe_shadow` records, when the built-in judge is the deciding gate or when the safety gate supplies evidence with those reserved kinds. This keeps session analysis working while consumers move to `gate_*`. The generic records are additive: they do not change session JSONL, completion JSON, or stream JSON.

### Default policy and compatibility

Cake must not silently run Bash ungated. If no `pre_tool` gate covers `bash.exec`, Bash and `BashSession` are removed from the tool registry and a startup diagnostic explains how to install or configure a gate. A gate that cannot start is treated as no usable enforce gate: a `describe` or `init` failure removes Bash for the run rather than dropping the gate and running a weaker pipeline. An explicitly configured gate that cannot load is a settings error. Auto-detecting `cake-safety-judge` on `PATH` is the zero-config default once the binary is installed; explicit `[tools.bash] gates` configuration wins. During the one-release dual-path period, a configured `[tools.bash.judge]` table is the fallback when no external gate is found; it is removed with the judge internals in slice 5. `CAKE_GATE=off` and `--no-gate` are explicit, recorded bypasses; `CAKE_JUDGE=off` remains an alias for one release. A missing gate is a startup and `cake gates doctor` diagnostic, not a quiet fallback.

`cake bash check` keeps its current JSON document and exit-code contract. The pipeline adds `data.gate` and `data.stages` additively; the existing `data.verdict`, `code`, `confidence`, `message`, `overridden`, `bypassed`, `latency_ms`, `stage`, `probability`, and `observation` fields keep their meanings. A deliberate block is a successful inspection and exits `0`; gate failure classes map to the existing exit classes (request timeout/network/auth/rate limit to `2`, other transport/malformed/refusal/protocol/crash failures to `1`, configuration failures to `3`, and a missing gate to `3`). A gate failure in an enforce pipeline writes no JSON document in `--json` mode, matching today's judge-failure behavior.

### Reference gate and sequencing

The reference gate is written in Go. The judge is an HTTP prompt loop, and Go matches the plugin ecosystem around this workspace (`cake-subagents`, `cake-repl`), so it iterates faster and needs no Rust toolchain or Cake ABI. The cost is reimplementing provider, retry, and refusal handling; the mitigation is that the in-process judge is the oracle and the gate must match it on the frozen corpus before it gets any authority. Rust remains the fallback only if exact provider parity is the dominant risk after slice 2.

The first external gate keeps the TypeSafe cascade inside `cake-safety-judge` to preserve today's failure semantics. A second `typesafe` gate in front of a `safety` gate is the more composable end state; it is deferred until the protocol and the base gate are stable. Edit/Write and post-tool gates are explicitly out of scope until the Bash pre-tool path is stable.

Delivery is staged: (0) freeze this contract; (1) put the current judge behind an in-process gate seam and add a fake gate; (2) build the standalone gate and match the corpus; (3) run it in shadow beside the in-process judge; (4) make it the default when installed, with one release of compatibility aliases; (5) delete the judge internals and delegate `bash check`. The ExecPlan records the acceptance criteria for each slice.

### Consequences

- Good, because policy experiments no longer require a Cake change or rebuild, and a user can compose deterministic, model-based, local, or typed stages.
- Good, because Cake keeps one implementation of sandbox semantics, one enforcement path, and one telemetry framing, while the gate owns only its decision.
- Good, because fail-closed, warn, allowlist, bypass, evidence bounds, and shadow evaluation are protocol-level guarantees rather than per-judge features.
- Good, because the standalone gate can be benchmarked and versioned independently, and its corpus and SLOs move out of Cake core.
- Good, because a missing gate disables Bash instead of silently degrading to execution without policy.
- Bad, because a process boundary adds latency, startup cost, and a new trusted executable that users must install and trust.
- Bad, because a gate can now fail in more ways (crash, protocol violation, restart loop) that Cake must bound and diagnose.
- Bad, because the reference gate reimplements provider, retry, and refusal behavior and can drift from the oracle; shadow evaluation is required before enforcement.
- Bad, because `gate_*` telemetry and legacy `judge_*` records coexist for one release, and session analysis must tolerate both.

## More Information

- Delivery plan and cross-project handoff: `docs/exec-plans/active/external-policy-gates.md` (the workspace-level initiative lives outside this repository).
- Backlog: 128 (this platform tracker), 128a (slice 0 contract), 033 (judge-quality tracker), 033b (trusted authorization context), 033c (structured Bash action packet).
- Existing decisions: [ADR 018](./018-llm-judge-command-gate.md), the LLM judge command gate; [ADR 020](./020-bounded-llm-judge-recovery.md), bounded recovery; [ADR 033](./033-typesafe-judge-shadow-evaluation.md), TypeSafe shadow evaluation; [ADR 034](./034-typesafe-judge-cascade-cutoff.md), the cascade cutoff; [ADR 035](./035-judge-script-observations.md), script observations.
- Contract authorities after acceptance: `docs/integrations.md` (wire protocol, composition, telemetry, `bash check`), `docs/configuration.md` (`[gates]` schema and precedence), `docs/security.md` (trust boundary and no-gate rule), and `docs/schemas/cake-gate-1.schema.json` (frame schema).
- Delivery plan: `docs/exec-plans/active/external-policy-gates.md`.
- The reference gate repository `cake-safety-judge` is created only after this contract is reviewed and frozen.

---
status: accepted
date: 2026-08-08
decision-makers: Travis Ennis
informed: issue 72
---

# LLM Judge Command Gate

## Context and Problem Statement

Cake blocks and warns on model-generated Bash commands through a hand-enumerated compiled guard (`src/clients/tools/bash_safety/`, about 2,300 lines): nine hard blocks, one soft warning, and a bespoke best-effort shell parser. ADR-015 (proposed, never accepted) planned to replace that compiled judgment with a deterministic, user-owned declarative command-policy engine. Any rule-based system loses by construction to the long tail: destructive commands behind variables, aliases, `xargs`, `find -delete`, encodings, or tools no rule enumerates. An LLM can evaluate what a command means and improves for free as models improve, but it is non-deterministic, costs latency and tokens on the hottest tool, depends on whatever backend the user configured, and fails in a correlated way under prompt injection.

This decision replaces both the compiled guard and the planned declarative engine with a single LLM judge: every Bash command is evaluated by a model call against an embedded rubric that preserves today's protections. The judge is the only command-safety gate above the operating-system sandbox; there is no deterministic floor beneath it. That risk is accepted deliberately, and the constraints that made the judge safe only as an opt-in tier (deterministic floor first, warn-default, fail-open) are re-decided here for a gate with no floor.

## Decision Drivers

- Preserve today's out-of-box posture: the nine destructive-command classes still block and the existing footgun warning still warns without user configuration.
- Cover the long tail that rule enumeration cannot, by judging what a command means rather than matching its spelling.
- Keep the operating-system sandbox (Seatbelt/Landlock) as the filesystem security boundary, unchanged.
- Fail safe: an unavailable or misbehaving judge must never let a command run ungated.
- Keep decisions auditable through stable verdict codes and metadata-only telemetry that never duplicates command text.
- Give users an explicit escape hatch so a failing judge cannot strand every session.
- Make the judge measurable for bypass rate before it becomes the only gate.

## Considered Options

- **LLM judge as the only gate (chosen).** Delete `bash_safety` and do not build the declarative engine; the judge evaluates every Bash command and blocks or warns per an embedded rubric, with an explicit allowlist as the only hard override.
- **Declarative command policy (ADR-015).** Ship the deterministic policy engine and keep the judge, if anything, as an advisory tier above it. Rejected because rule systems lose by construction to the long tail: the judge's biggest win (novel destructive patterns) is exactly the territory a deterministic floor misses, and the registry only improves after a human adds a rule post-incident.
- **Judge layered above a deterministic floor (the original opt-in-tier design).** Deterministic policy evaluates first and can never be overturned; the judge only sees undecided commands and warns by default. Rejected for the same reason as the declarative engine alone: the deterministic floor is the long-tail weakness, so layering does not fix the hole the judge is meant to close.

## Decision Outcome

Chosen option: an LLM judge as Cake's only non-sandbox command gate, fail-closed, with no deterministic floor. `bash_safety` and the planned declarative engine are removed rather than layered beneath or beside the judge. The judge is default-on. The OS sandbox (Seatbelt/Landlock) remains the filesystem boundary and is unchanged, and the judge stays active independently of the sandbox policy, including under `danger-full-access` and `CAKE_SANDBOX=off`.

Cake calls the judge for every Bash command with the command text, the working directory, a compact repository-state digest, and the tool's untrusted `reason` argument. The judge returns a strict JSON verdict object `{"verdict":"block"|"warn"|"allow","code":"<code>","message":"...","confidence":0.0-1.0}`; malformed or missing fields count as judge failure. A `block` prevents execution and returns the judge's message as the tool error. A `warn` executes normally and prepends its message without changing exit status or error classification.

The judge fails closed. A bounded judge timeout (default 30 seconds, configurable) turns a timeout, transport error, refusal, or malformed response into a block with a clear model-visible message and a metadata-only telemetry event. Bash never runs ungated. There are no retries in version 1: fail-closed is the fallback, and whole-turn retry semantics are revisited separately. Verdict caching is also deliberately absent from version 1; context-sensitive caching keyed by (command, cwd, repo-state digest) is deferred until latency and cost data justify it.

The judge model defaults to the agent's configured model family, so same-family behavior holds without extra configuration; a `judge_model` setting overrides it in `settings.toml`. The same-family correlated-injection risk is accepted; a different-family or higher-reasoning judge is the documented hardening path.

The embedded default rubric, distilled from the current nine hard blocks and the existing warning, blocks destructive classes and warns for footguns out of the box. It evaluates the command's meaning (aliases, variables, wrappers, encodings), considers cwd and repo state, weighs the command over the untrusted `reason` and flags incongruence, ignores instructions embedded in command text (prompt-injection defense), and prefers a concrete safer alternative in its message. An optional user rubric file may append additional guidance; rubric relaxations are advisory to the judge, not hard overrides.

The explicit user allowlist is the only hard override. Version 1 matches exact raw-command equality only; pattern matching is deferred because an allow pattern can silently neutralize unrelated restrictions. Allowlisted commands are still judged; a block is overridden, but the verdict and an `overridden` flag are still telemetried. There are no allowlist entries in shipped defaults.

A telemetry-logged emergency bypass exists (`CAKE_JUDGE=off` or `tools.bash.judge.enabled = false`) and is off by default; every Bash call while bypassed emits a bypass telemetry event, so the escape hatch cannot be used silently.

Verdicts carry stable, namespaced verdict codes, replacing ADR-015's rule IDs so decisions stay auditable without a regex engine; the version-1 vocabulary is enumerated in the implementation plan, and `allow` needs no code. `cake bash check -- <command>` replaces `cake policy check`: it runs the same judge path, prints verdict, code, message, and latency, and never executes the command.

Telemetry is metadata-only: the decision, verdict code, tier, latency, confidence, and the overridden and fail-closed flags, never the command, `reason`, or message text. Judge blocks and fail-closed denials are recorded in `task_complete.permission_denials`.

Default-on requires the evaluation prerequisites first: #84 (a controlled model-evaluation harness measuring bypass rate, because a judge that is the only gate must be measured, not trusted), #106 Phase A (an external case corpus seeded from the current guard's cases as the judge's regression data), and re-scoped #66 (verdict counters in session telemetry before default-on).

### Consequences

- Good, because the judge covers the long tail that rule systems miss by construction and improves for free as models improve.
- Good, because the rubric preserves the current nine blocks and the existing warning without a compiled default table or a growing bespoke shell parser.
- Good, because fail-closed behavior, metadata-only telemetry, stable verdict codes, and an explicit allowlist keep the gate auditable and user-ownable without a policy engine.
- Good, because the emergency bypass and the bounded timeout keep a failing judge from stranding sessions while preserving the fail-closed default.
- Bad, because the gate is non-deterministic: the same command may be judged differently across calls, models, or days, weakening reproducible decisions and incident forensics.
- Bad, because the judge adds latency and token cost to the hottest tool, and a judge outage or backend change blocks commands instead of degrading gracefully.
- Bad, because a user on a weak local model gets a safety layer worse than the regexes it replaced.
- Bad, because prompt injection can manipulate the judge in a correlated way: the judge is weakest exactly where it is needed most, and the same-family default widens that correlation.
- Bad, because the sandbox bounds filesystem paths only; in-project destruction, remote Git effects, and ambient `GIT_DIR` redirects in linked worktrees sit outside sandbox protection and remain residual risk.

## More Information

- Supersedes [ADR-015](./015-declarative-command-policy.md), `Declarative Command Policy` (proposed, never accepted): the deterministic policy engine it planned is replaced entirely by the LLM judge. The judge is the only non-sandbox command gate; the declarative engine is not built.
- Implements the direction of issue #72, `Replace bash_safety With a Default-On LLM Judge and Bash reason Argument`.
- Reverses the 2026-07-18 conclusion of `docs/research/topics/llm-judge-bash-safety.md`; that note's trade-off analysis is retained as the risk record.
- Execution plan: `docs/exec-plans/active/llm-judge-command-safety.md`.
- Preserves ADR-014, `Sandbox Policy CLI Flag`: the judge is independent of the OS sandbox policy and remains active under `danger-full-access` and `CAKE_SANDBOX=off`.
- Builds on ADR-007, `Per-Session Telemetry Sidecar`, for metadata-only judge decision events.
- Current durable authorities are `docs/security.md`, `docs/configuration.md`, `docs/integrations.md`, and `ARCHITECTURE.md`.

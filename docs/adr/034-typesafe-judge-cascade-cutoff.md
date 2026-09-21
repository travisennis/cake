---
status: accepted
date: 2026-09-21
decision-makers: Travis Ennis
informed: issue 604
---

# Adopt a 0.85 Jev fast-approval cutoff as the first stage of the Bash judge cascade

## Context and Problem Statement

Cake's Bash tool sends every non-empty command to a generative command-safety judge before the operating-system sandbox runs it (ADR 018). That judge is authoritative and fail-closed, and it costs provider latency on every command, including trivially observational ones such as `git status --short`. Issue #602 added an opt-in TypeSafe shadow evaluator, and ADR 033 kept it observational. Issue #604 asked whether Jev's probability is accurate, available, and fast enough to become a fast-approval stage: ask Jev first, approve immediately when its probability clears a cutoff, and fall back to the generative judge otherwise.

The evaluation is recorded on #604. Two independent shadow-on runs over the frozen gold corpus `independent-v2` (`86d3d49c`) at five repetitions each placed the worst single blocked trial at `0.82`, and the lowest cutoff with zero unsafe approvals moved a full grid step between them (`0.83` then `0.82`). At `0.82` the cutoff sits exactly on the worst blocked trial and has no headroom against Jev's repeat spread. `0.85` is the lowest candidate that clears the observed block band.

## Decision Drivers

Unsafe approval of a destructive command is the failure mode that matters, and the observed one below the floor is credential disclosure (`cat ~/.npmrc`). Command authorization, the fail-closed rule, the allowlist, the emergency bypass, and the OS sandbox must keep their current meaning. The fast path must survive a TypeSafe outage without weakening any of that. The measured safety denominator is small, so the accepted bound has to be stated rather than implied.

## Considered Options

1. **Approve at `0.82` or `0.83`**, the lowest cutoffs with zero observed unsafe approvals. Rejected: the floor moved between two runs of the same corpus, and `0.82` coincides with the worst observed blocked trial. There is no margin.
2. **Adopt `0.85`**, the lowest candidate above the pooled block band. Keeps most of the observed observational coverage and buys 0.03 of headroom over the worst of 762 pooled blocked observations.
3. **Do not adopt a cascade; use a deterministic local rule** for observational commands. This is the recorded alternative. It removes generative-judge latency with no remote model and no new disclosure surface, but it needs a hand-maintained list of command shapes, and ADR 018 deliberately removed that kind of compiled rule floor.
4. **Keep the shadow only.** Rejected: it leaves the measured latency win unused and the decision open.

## Decision Outcome

Option 2. Jev becomes the first stage of the Bash safety judge with a fast-approval cutoff of **`0.85`** on its probability, and the existing generative judge remains the authority for every command the cascade does not fast-approve.

Evidence on #604, at `0.85`:

- Zero unsafe approvals and zero high-risk approvals in both independent shadow-on runs: 0/250 and 0/340 tuning, 0/72 and 0/100 held-out blocked observations, zero across all 762 pooled blocked observations from 88 distinct block cases.
- Observational coverage 167/201 (83.1%) tuning and 62/70 (88.6%) held-out in the clean run; 214/250 (85.6%) and 73/85 (85.9%) in the other. Legacy rubric-regression column: 0/334 and 0/105 unsafe, 0/30 warning approvals, 50/230 (21.7%) tuning and 27/105 (25.7%) held-out gold-`allow` coverage.
- Fast path: Jev success latency p50 316 ms / p95 576 ms, against 2798 ms / 5998 ms for the judge alone in the same window. Paired cascade estimate p50 2338 ms / p95 5603 ms tuning, with a fast-path-only leg of p50 305 ms.
- Accepted bound: zero unsafe approvals over 88 distinct block cases is a rule-of-three 95% upper bound of about 3.4%. Repetitions do not tighten it. The corpus over-samples dangerous commands and does not describe production traffic.

### Failure and fallback semantics

Jev approves a command only when its probability is greater than or equal to `0.85` and the observation succeeded. Every other outcome falls back to the existing generative judge: a probability below the cutoff, a timeout at the observation deadline, a transport or protocol failure, a missing credential, or `mode = "off"`. A TypeSafe failure must never approve, must never block, and must never bypass the judge --- the fallback leg keeps its current authority and its fail-closed behavior.

The cascade changes nothing else. `allow`, `warn`, and `block` keep their meanings, including that a warning prints and still executes. The allowlist and the emergency bypass keep their current precedence. Judge and sandbox denial semantics, session records, telemetry, and the sandbox are untouched. A fast approval is not a sandbox bypass: the approved command still runs under the operating-system sandbox.

TypeSafe can only ever *remove* judge latency, never a guard. Configured off, or with no credential, the tool behaves exactly as it does today.

### Security review requirement

Implementation is gated on a security review, recorded on the implementation issue, that verifies at least:

- Fail-closed fallback on every TypeSafe failure class, with tests that a failed observation cannot approve a command.
- The cutoff is a reviewed constant, not user-lowerable configuration; lowering it is an ADR change, not a settings change.
- No path where a fast approval skips the allowlist, the sandbox, or the emergency bypass.
- The disclosure surface stays what ADR 033 recorded: command, cwd, digest, untrusted reason, and rubric, sent only when the cascade is explicitly enabled.
- The blocked-command failure mode is exercised directly: the credential-disclosure cases that set the boundary (`cat ~/.npmrc`, `printenv`, `cat ~/.zsh_history`, `docker inspect --format '{{json .Config.Env}}'`) must fall back, not fast-approve.

### Consequences

Observational commands can skip generative-judge latency, roughly 0.3 s instead of 2.4--2.8 s in the measured window, for the share of traffic the fast path approves. In exchange, up to about 3.4% of gold-blocked commands could be approved unseen, the yield depends on the unmeasured share of real traffic that is observational, and TypeSafe token spend is added on every command while the judge under evaluation runs on a ChatGPT subscription where per-token prices do not apply. Jev sends 2913 input and 20 output tokens per command against the judge's 2388 and 48, so the fast path is cheaper only above an 18.8:1 output-to-input price ratio (69:1 if the judge's cache discount is applied) --- above any published ratio, so the win is latency, not cost.

A cascade also makes TypeSafe availability a latency dependency. It cannot make the tool less safe, because every failure falls back to the judge, but an observation timeout adds up to its deadline to the slow path.

## More Information

Issue #604 holds the runs, the write-up, and the limits of this evidence. The implementation issue for the cascade carries the security review. `docs/exec-plans/completed/typesafe-judge-cascade-evaluation.md` --- still active at the time of writing --- records the measurement plan and the deterministic-rule alternative. ADR 018 remains the command-approval authority, ADR 020 governs bounded recovery, and ADR 033 remains the record for shadow mode and the disclosure surface. `docs/security.md`, `docs/configuration.md`, and `docs/integrations.md` describe the contracts the cascade must preserve.

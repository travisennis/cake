---
status: accepted
date: 2026-10-26
decision-makers: Travis Ennis
informed: task 033p
---

# Always observe TypeSafe for commands that carry script evidence

## Context and Problem Statement

ADR-018 makes the generative judge the Bash command gate. ADR-034 adds the TypeSafe/Jev cascade, and ADR-035 collects one referenced script's contents and suppresses the TypeSafe observation for any command whose preflight collected evidence, because the observation request state carries no script contents. That rule kept "script contents reach both stages or neither stage approves".

In practice the rule makes the review path depend on script syntax. An agent that invokes `bash script.sh` gets no observation and always reaches the judge. An agent that inlines the same script in a heredoc produces no `script_evidence`, so the observation runs and the command can be fast-approved. Cake's script-collection failure message explicitly tells the model to "inline the intended command for judgment", so the workaround is sanctioned. The external `cake-safety-judge` already observes every check. A uniform rule is preferable to a syntax-dependent one.

## Decision Drivers

- One review path for the same command effect; no incentive to choose file versus heredoc based on eligibility.
- Preserve the judge's authority, the fail-closed fallback, the allowlist, and the emergency bypass.
- Keep the TypeSafe disclosure surface unchanged: command, cwd, repository digest, untrusted reason, and rubric; never file contents.
- Keep the measured `0.85` cutoff and its evidence; do not lower or re-scope it.
- Be explicit that the observation is blind to referenced file contents and can approve based on command text alone.
- Avoid new shell parsing, wider collection, or a second policy path.

## Considered Options

1. **Keep ADR-035's skip and make `cake-safety-judge` match.** Rejected: preserves the syntax-dependent path and the heredoc arbitrage.
2. **Always observe and allow fast approval, including script-evidence requests (chosen).** Uniform, matches the external gate, and keeps the cutoff and fallback. Accepts that TypeSafe may approve a command whose referenced file it did not see.
3. **Always observe but never fast-approve when `script_evidence` is present.** Rejected: keeps shadow data and the old guarantee, but retains the special case and still makes approval depend on the evidence channel; it also diverges from the gate.

## Decision Outcome

Chosen option 2. Under `mode = "shadow"` and `mode = "cascade"`, Cake makes the TypeSafe observation for every non-bypassed Bash request, including one whose preflight collected script evidence. `script_evidence` continues to reach only the generative judge; the observation request state is unchanged (command, cwd, repository digest, reason, and rubric). A clean probability at or above `0.85` fast-approves the command; every failure or lower probability falls back to the judge. The allowlist, bypass, sandbox, and fail-closed behavior are unchanged. ADR-035's **Evidence completeness and fast approval** section is partially superseded; its collection, recognition, and trust rules remain.

### Security impact

This widens TypeSafe's authority to commands whose referenced script contents it cannot see. The accepted residual risk is a successful but incorrect observation that approves a command the generative judge would have blocked. The observation still receives the command text, so a heredoc or `-c` body is visible, while a literal file invocation is not. The `0.85` cutoff and its ADR-034 evidence were measured on that earlier corpus; this decision does not add a separate measurement for script-referencing commands, so the rule-of-three bound is not independently established for that population.

Bypass classes considered: file contents unseen by TypeSafe; the agent choosing a syntax that produces no `script_evidence`; TypeSafe ignoring its own "opaque scripts are not eligible" instruction; prompt injection in the command or reason; an oversized or uncollected script; and a script changing after collection. The change does not add a parser, widen collection, or change the disclosure surface. The failure and fallback classes are unchanged: timeout, transport or protocol failure, missing credential, and below-cutoff all reach the judge, and none can approve or block. The bypass is checked first, so no path reaches the sandbox without a gate decision.

### Compatibility

No CLI, settings, persisted-state, gate-protocol, or telemetry-shape change. The only behavior change is that commands with collected script evidence can now fast-approve instead of always reaching the judge. `data.stage`, `data.probability`, and `data.observation` already represent a fast approval, and a fast approval still has no `judge_attempt` record.

## Consequences

- Good, because the same command effect takes one review path regardless of whether it arrived as a file or an inline body.
- Good, because the in-process judge and `cake-safety-judge` now agree on when TypeSafe runs.
- Good, because shadow data now covers script-referencing commands.
- Bad, because TypeSafe can approve a command without seeing the referenced script contents, losing the judge's evidence advantage on the fast path.
- Bad, because a cascade now spends one bounded observation on every script command, including commands that fall back.
- Neutral, because no script contents reach TypeSafe and the disclosure surface is unchanged.

## More Information

- Partially supersedes the **Evidence completeness and fast approval** section of [ADR-035](035-judge-script-observations.md); ADR-035's collection rules and limitations remain in force.
- Relates to [ADR-018](018-llm-judge-command-gate.md), [ADR-033](033-typesafe-judge-shadow-evaluation.md), [ADR-034](034-typesafe-judge-cascade-cutoff.md), and [ADR-049](049-external-policy-gates-with-the-cake-gate-1-protocol.md).
- `cake-safety-judge` already observes every check; this decision makes the in-process judge match it.
- Follow-up, not part of this decision: task 033q covers treating heredoc and `-c` bodies as script content in the rubric, failing closed on unobservable interpreter input, and adding corpus cases for inline scripts, `bash -c "$(cat file)"`, and oversized scripts.

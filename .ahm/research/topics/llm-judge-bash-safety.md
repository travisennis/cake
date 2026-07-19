# LLM-as-Judge for Bash Command Safety

Status: synthesized
Created: 2026-07-18
Updated: 2026-07-18
Related tasks: 241
Related plans: .ahm/exec-plans/active/declarative-command-policy.md
Confidence: medium

## Summary

Explored a bitter-lesson-style alternative to the static `bash_safety` guard:
call an LLM per Bash invocation to judge whether the command is destructive or
malicious, block with a tailored safer-alternative recommendation when it is,
and add a `reason` argument to the Bash tool so the judge can evaluate stated
intent alongside the command.

Conclusion: the judge and the declarative command policy (ADR-015 / task 241)
are not competing designs. The declarative policy is the deterministic,
user-ownable substrate; an LLM judge only works safely as an optional
escalation tier layered above it. Ship 241 unchanged first. The `reason`
argument is worth adding independently of any judge.

## Notes / Evidence

### Current state

- `src/clients/tools/bash_safety/` (~2,300 lines) is hand-enumerated compiled
  judgment: nine hard blocks, one soft warning, and a bespoke best-effort shell
  parser (`parse.rs`) that will never be correct against real shell semantics.
- Task 241 + ADR-015 (proposed) + the `declarative-command-policy` ExecPlan
  convert this to user-owned policy data with stable rule IDs, layering,
  provenance, digests, `cake policy show`/`check`, and metadata-only telemetry.
- The OS sandbox (Seatbelt/Landlock) remains the actual filesystem security
  boundary in every design considered here. The commands where the guard is
  load-bearing are remote effects (`git push --force`) that no sandbox stops.

### Arguments for an LLM judge

- **Long-tail coverage.** Regex checks lose by construction to anything not
  enumerated: destructive commands behind variables, aliases, `xargs`,
  `find -delete`, base64, or novel tools. A judge evaluates meaning and
  generalizes; it improves for free with better models, while the rule
  registry only improves after a human adds a rule post-incident.
- **Context-sensitive verdicts.** `rm -rf ./target` vs `rm -rf ~/.config`
  without maintaining exception tables (temp-dir carve-outs,
  `--force-with-lease`, safe `restore` forms).
- **Tailored remediation.** Static `Tip:` strings become command-specific
  safer alternatives, improving agent recovery.
- **`reason` argument.** Gives the judge intent; incongruence between a
  benign reason and what the command actually does is itself a strong signal.
  Side benefit regardless of judge: articulating a reason tends to improve
  agent behavior (cf. Claude Code's Bash `description` field) and enriches
  telemetry and session analysis.

### Arguments against

- **Latency and cost on the hottest tool.** Bash dominates tool calls. Even a
  small-model judge adds ~300–800 ms and ~500–1,000 tokens per call; hundreds
  of calls per session means minutes of wall clock and real spend. Verdict
  caching by command hash is weak because safety is context-dependent (cwd,
  repo state).
- **No guaranteed judge model.** Cake is backend-agnostic. A user on a weak
  local model gets a safety layer worse than the regexes it replaced.
- **Availability failure mode.** Judge API down: fail-closed makes cake
  unusable offline; fail-open silently removes the safety layer. Either way,
  new config surface and new bug class.
- **Non-determinism.** Destroys everything ADR-015 delivers: reproducible
  decisions, named overrides, testable policy (`cake policy check`), digests,
  incident forensics. Same command may block Wednesday after passing Tuesday.
- **Conflicts with the 2026-07-07 direction (mechanism over judgment).** The
  harness owns capability and *user-chosen policy*, not judgment calls. A
  judge moves judgment into the harness at runtime, unauditable and
  un-overridable — the opposite of policy-as-data. (Making judge authority a
  `policy.toml` knob restores the user-chosen framing.)
- **Correlated failure under prompt injection — the critical one.** If
  hostile repo content manipulated the main agent into a destructive command,
  a judge model (especially same-family, especially one trusting the
  self-reported `reason`) is plausibly manipulated by the same content.
  LLM-as-guard is a bypassable filter, not a boundary. The judge is weakest
  exactly where it is needed most.
- The judge's biggest win (novel destructive patterns) and biggest weakness
  (correlated failure under injection) occupy the same territory — which is
  why it can be a layer above a deterministic floor but not the floor.

## Implications for this project

1. Ship ADR-015 / task 241 unchanged. Its structured evaluation results,
   stable rule IDs, and telemetry plumbing are prerequisites for threading
   any judge verdict through the system.
2. If a judge is added (task 278), it is an opt-in escalation tier:
   deterministic policy evaluates first; hard blocks are never overturned by
   the judge; the judge only sees commands policy did not decide; verdicts
   default to warn (advisory); fail open with a telemetry event; judge model
   is user-configured; whether a judge verdict may block is a `policy.toml`
   decision, not a harness default.
3. Add a `reason` argument to the Bash tool independently (also task 278
   scope, severable). Treat it as untrusted self-report everywhere: the judge
   must weigh the command over the reason and flag incongruence; a benign
   reason must never launder a hostile command.

## Follow-ups

- Task 278 (blocked on 241): opt-in LLM-judge escalation tier + Bash `reason`
  argument.
- When 241's telemetry lands, per-rule fire counts can quantify how much
  destructive-command traffic escapes the deterministic rules — evidence for
  or against the judge tier being worth its cost.

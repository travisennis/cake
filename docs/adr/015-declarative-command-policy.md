---
status: superseded by ADR-018
date: 2026-07-12
decision-makers: Travis Ennis
---

# Declarative Command Policy

## Context and Problem Statement

Cake runs model-generated shell commands through its Bash tool. The operating-system sandbox limits filesystem access, while `src/clients/tools/bash_safety/` separately blocks known-destructive commands inside allowed paths and warns about deterministic command-line footguns. The current Bash safety registry contains nine hard blocks and one soft warning as compiled Rust functions, with messages and workflow opinions fixed at release time. Users cannot relax a rule even when they deliberately authorize the operation, projects cannot add or tailor rules without code changes, and the bespoke shell parser keeps expanding toward incomplete shell interpretation.

Task 241 replaces that compiled judgment with user-owned declarative policy while preserving the existing default behavior. This is security-adjacent but is not a replacement for Seatbelt or Landlock. The operating-system sandbox remains the filesystem security boundary; command policy remains a best-effort workflow guard that can also cover effects outside that boundary, such as remote Git history.

The design must preserve the current nine blocks and existing warning out of the box, make overrides explicit and auditable, avoid turning policy matching into a programmable shell parser, keep session data compatibility, and provide enough introspection to explain layered decisions.

## Decision Drivers

- Preserve current Bash safety behavior when no user or project policy is present.
- Let users and projects relax, tighten, or selectively replace named rules without rebuilding Cake.
- Keep project-over-user-over-embedded precedence consistent with Cake settings and acai.
- Prevent broad allow patterns from silently neutralizing unrelated restrictions.
- Stop growing bespoke shell semantics while retaining the bounded preprocessing needed for compatibility.
- Make configuration failures deterministic and visible before model or tool execution.
- Keep the operating-system sandbox and command policy as independent controls.
- Make layered policy decisions inspectable and measurable without duplicating sensitive command text into telemetry.

## Considered Options

### Override and conflict models

The alternatives were broad matching allows, numeric rule priorities, deny-always-wins behavior, and stable-ID overrides. Broad allows make composition unsafe because an old permission can neutralize a later or narrower block. Numeric priorities make policy resolution difficult to audit. Deny-always-wins prevents the user-owned relaxation that motivates the change. Stable-ID overrides let a policy explicitly reject one inherited judgment while leaving independent rules in force.

### Configuration placement and precedence

The alternatives were embedding policy in `settings.toml`, importing arbitrary policy paths, giving user policy final authority, and allowing only monotonic tightening. Dedicated fixed-location policy files keep security-adjacent rules separate from provider settings. Project precedence matches existing Cake and acai behavior. Imported files and whole-policy replacement add ownership and ordering complexity that version 1 does not need.

### Matching languages

The alternatives were a structured command-and-flag DSL, literal or glob patterns, a full shell-parser dependency, and regular expressions. A structured DSL would continually acquire shell-specific fields; literal matching is too weak; a full parser creates a large dependency and semantic contract. Composable Rust regular expressions over a small set of frozen inspection views provide one bounded matching primitive without backtracking risk.

### Persistence and inspection

The alternatives were adding structured decisions to the main session JSONL, recording commands or matched substrings in telemetry, and providing no inspection command. The existing telemetry sidecar is the appropriate structured measurement surface, while the ordinary Bash result already persists the visible explanation in the transcript. Metadata-only events avoid duplicating potentially sensitive commands. `policy show` and `policy check` make layered behavior testable without executing a command.

## Decision Outcome

Chosen option: replace compiled Bash safety judgments with a versioned, layered declarative command-policy engine, because it transfers policy ownership to users and projects while retaining deterministic default behavior and a narrow best-effort matcher.

Cake ships an embedded policy containing the current nine blocking rules and existing warning. It optionally loads `~/.config/cake/policy.toml`, then `<project>/.cake/policy.toml`; later sources override earlier sources, so project policy has final authority. Every file requires `version = 1`. Cake resolves and validates the complete policy once at invocation startup and uses that immutable snapshot for the invocation. A continue or resume operation is a new invocation and loads current policy. Malformed TOML, missing or unsupported versions, unknown fields, duplicate definitions, invalid patterns, namespace violations, and overrides of unknown IDs fail before any API request or tool execution.

Rule identifiers are durable, globally unique, namespaced compatibility identifiers. Embedded definitions use `cake/*`, global definitions use `user/*`, and project definitions use `project/*`. Cake reserves `cake/*`. New rules may decide `block` or `warn`; `allow` is valid only in an explicit override of an inherited rule ID. An override may selectively replace any effective field except the stable ID and namespace. Unspecified fields remain inherited. There is no whole-policy replacement mode in version 1.

Independent rules always remain applicable. Cake evaluates every rule, deduplicates decisions by rule ID per Bash call, and resolves the final action with `block` stronger than `warn`, which is stronger than `allow`. It reports one primary decision and lists other matching IDs, while telemetry records every matching rule. The primary rule is selected by strongest decision, then highest-precedence source, then earliest rule order within that source, with stable ID as a deterministic final tie-breaker. Overrides retain the inherited rule's order. Repeated matches retain an approximate occurrence count.

Each rule has a required static message and an optional static suggestion. Version 1 has no templates or command-text interpolation. Warnings execute the command normally, do not alter exit or error classification, and are prepended to the completed Bash result. Blocks prevent execution. Prompt language, interactive approval, CLI flags, and sandbox mode do not bypass policy; relaxation requires a policy-file override and a new invocation. Command policy remains active under `danger-full-access` and `CAKE_SANDBOX=off` because it is independent of the operating-system sandbox.

Each rule selects exactly one documented inspection view: `raw_command`, `raw_segment`, or `normalized_segment`. The existing bounded preprocessing for unquoted command separators, literal `bash -c` and `sh -c` bodies, command substitutions, whitespace normalization, and shell-data masking is retained as needed to preserve default behavior, then frozen. Correctness fixes are allowed, but Cake will not add new shell constructs or claim complete shell interpretation.

Matching uses Rust `regex` syntax with inline flags. Patterns compile once at startup under documented limits. A match contains optional nonempty `all`, `any`, and `none` lists. For one candidate, every `all` expression must match, at least one `any` expression must match when `any` is present, and no `none` expression may match. At least one positive `all` or `any` expression is required; empty lists and `none`-only matchers are invalid. A rule fires when at least one candidate satisfies the complete expression.

The implementation must benchmark evaluation with representative policies containing 10, 100, and 1,000 rules. It may use a compiled quick-rejection structure such as a `RegexSet` when benchmarks justify it, but the optimization must be observationally equivalent to evaluating every rule: it cannot change rule matches, primary selection, occurrence counts, or telemetry. The straightforward evaluator remains the reference behavior in equivalence tests.

Cake adds read-only CLI commands `cake policy show [RULE_ID]` and `cake policy check -- <COMMAND>`. `show` displays resolved rules, per-field provenance, and the effective-policy digest. `check` runs the same preprocessing and evaluation as the Bash tool, explains the candidates and matching decisions, and never executes the command. Its output states that the matcher is best-effort policy evaluation rather than shell security analysis. Settings profiles do not affect command policy.

Policy decisions are written as metadata-only records in the existing per-session telemetry sidecar. Events identify the Bash call and turn, stable rule ID, effective decision, definition source, effective contributing sources, approximate match count, and effective-policy digest. They do not include the command, matched text, patterns, messages, or suggestions. The digest is a versioned SHA-256 hash of a canonical serialization of the complete ordered effective policy, including decisions, matchers, messages, and suggestions but excluding paths, timestamps, comments, formatting, and ineffective override history. Inspection retains field-level provenance.

### Consequences

- Good, because current behavior remains unchanged for installations without policy files.
- Good, because users and projects can explicitly tailor individual rules without releases or broad bypass flags.
- Good, because stable IDs, strict validation, provenance, inspection commands, and canonical digests make resolution auditable.
- Good, because a single bounded regex model resists further growth of a bespoke shell language.
- Good, because policy telemetry can measure rule relevance without increasing command-data exposure.
- Bad, because project policy can relax user and embedded protections when a user enters that repository.
- Bad, because users must edit a policy file and start a new invocation even for a one-time authorized command.
- Bad, because regular expressions and bounded preprocessing remain best-effort and cannot recognize every shell-equivalent spelling.
- Bad, because field-level overrides, provenance, canonicalization, and two CLI inspection commands increase implementation and compatibility surface.
- Bad, because rule IDs, schema version 1, inspection views, regex semantics, precedence, and digest canonicalization become durable contracts.

## More Information

- Superseded by [ADR-018](./018-llm-judge-command-gate.md), `LLM Judge Command Gate` (2026-08-08): issue #72 replaced the planned declarative engine with a default-on LLM judge as the only non-sandbox command gate. This ADR was proposed but never accepted, and the declarative engine is not built. The historical decision above is retained as the record of the direction at the time.
- Implements the architectural direction required by task 241, `Convert bash_safety Into a Declarative Command-Policy Engine`.
- Builds on ADR-007, `Per-Session Telemetry Sidecar`, for structured policy decision records.
- Preserves ADR-014, `Sandbox Policy CLI Flag`: command policy remains active independently of the selected filesystem sandbox policy.
- Current durable authorities are `docs/security.md`, `docs/configuration.md`, `docs/integrations.md`, and `ARCHITECTURE.md`.
- Version 1 intentionally omits override reasons and expiration. A future policy schema may add a static `reason` and an `expires_at` timestamp for auditable, time-bounded overrides after real usage establishes the required expiration and clock-handling semantics.

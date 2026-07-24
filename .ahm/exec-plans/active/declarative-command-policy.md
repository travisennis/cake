# Deliver User-Owned Declarative Command Policy

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `ahm context plan`. A contributor implementing it must revise the plan at every stopping point and leave it self-contained for the next contributor.

## Purpose / Big Picture

Cake currently hard-codes Bash command blocks and warnings in Rust. After this work, Cake will preserve those protections by default while letting a user or repository change a named rule in `policy.toml`, add project-specific blocks and warnings, inspect the resolved policy, and test a command against policy without executing it. The operating-system filesystem sandbox remains independent and continues to be the actual filesystem security boundary.

A user can see the result by running `cake policy show`, by checking `cake policy check -- 'git push --force origin main'`, and by invoking Cake in a repository whose `.cake/policy.toml` changes `cake/git-push-force` from `block` to `warn`. With no policy files, the current nine hard blocks and the existing `rg` warning must behave exactly as before.

## Progress

- [x] (2026-07-12) Drafted proposed ADR-015 after resolving the command-policy ownership, layering, matching, telemetry, and inspection decisions with Travis Ennis.
- [x] (2026-07-12) Created this ExecPlan and linked it from task 241.
- [ ] Implement versioned policy types, embedded defaults, strict validation, layering, provenance, and canonical digest generation.
- [ ] Benchmark policy evaluation with 10, 100, and 1,000 rules; add a semantics-preserving prefilter only if measurements justify it.
- [ ] Replace compiled check functions with the declarative evaluator while preserving bounded preprocessing and default Bash behavior.
- [ ] Integrate immutable invocation policy snapshots with Bash execution and metadata-only telemetry events.
- [ ] Add `cake policy show` and non-executing `cake policy check -- <COMMAND>`.
- [ ] Update design, configuration, CLI, sandbox, telemetry, and architecture documentation.
- [ ] Run focused tests, serialization checks, platform-relevant sandbox checks, and `just ci`.

## Surprises & Discoveries

- Observation: The current safety registry already attaches stable internal names to nine blocks and one warning, but those names are not yet compatibility identifiers.
  Evidence: `src/clients/tools/bash_safety/mod.rs` defines `CheckDef { name, severity, check }` and suppresses dead-code on `name`.

- Observation: Bash safety currently returns formatted strings, so structured IDs cannot reach telemetry without changing the evaluator result and threading decisions through tool execution.
  Evidence: `validate_command_safety` returns `Result<Vec<String>, String>` and `execute_bash_with_args` immediately converts a block into a tool error.

- Observation: The ADR workflow reports unrelated historical warnings.
  Evidence: `ahm adr create` and `ahm adr accept 015` reported legacy ADR 002 status wording, task 004 without an acceptance section, and task 117 blocked despite completed dependencies. These are outside task 241.

## Decision Log

- Decision: Use proposed ADR-015 as the design under review; accept it before implementation begins.
  Rationale: Travis approved each substantive choice interactively but chose to retain proposed lifecycle status until work begins.
  Date/Author: 2026-07-12, Travis Ennis and Codex

- Decision: Keep command policy independent from the operating-system sandbox and active under `danger-full-access`.
  Rationale: The sandbox controls filesystem reach while command policy controls configured workflow judgments, including remote effects.
  Date/Author: 2026-07-12, Travis Ennis and Codex

- Decision: Use embedded defaults, then user policy, then project policy, resolved once per invocation.
  Rationale: This matches Cake and acai precedence while preventing mid-invocation self-modification from changing policy.
  Date/Author: 2026-07-12, Travis Ennis and Codex

- Decision: Use stable namespaced IDs and explicit per-ID overrides; independent matches resolve as `block > warn > allow`.
  Rationale: Broad matching allows could accidentally neutralize unrelated or subsequently added blocks.
  Date/Author: 2026-07-12, Travis Ennis and Codex

- Decision: Use composable Rust regular-expression predicates over one frozen inspection view per rule.
  Rationale: A command-and-flag DSL would keep growing shell semantics, while Rust regex is bounded and sufficient for policy data.
  Date/Author: 2026-07-12, Travis Ennis and Codex

- Decision: Add both policy inspection commands in version 1.
  Rationale: Field-level layering requires a supported way to explain effective rules, and a non-executing checker makes policy authoring practical.
  Date/Author: 2026-07-12, Travis Ennis and Codex

- Decision: Benchmark 10-, 100-, and 1,000-rule policies and permit an implementation-only fast prefilter when it preserves reference-evaluator semantics exactly.
  Rationale: User and project rules make policy size unbounded even though the embedded version begins small; measurement should determine whether optimization complexity is warranted.
  Date/Author: 2026-07-12, Travis Ennis and Codex

- Decision: Defer override `reason` and `expires_at` fields to a future schema version.
  Rationale: Auditable, time-bounded exceptions are useful, but version 1 deliberately has persistent named overrides and has not defined expiration, clock, or stale-policy behavior.
  Date/Author: 2026-07-12, Travis Ennis and Codex

## Outcomes & Retrospective

## Context and Orientation

Cake is a Rust 2024 binary. Model-callable tools live under `src/clients/tools/`. `src/clients/tools/bash.rs` parses a Bash request, calls the safety guard, configures Seatbelt on macOS or Landlock on Linux, runs `bash -c`, and formats output. `src/clients/tools/bash_safety/mod.rs` owns the current registry and validation loop. `checks.rs` contains ten compiled checks, while `parse.rs` performs quote-aware segmentation, literal `bash -c` and `sh -c` extraction, command-substitution extraction, whitespace normalization, and masking of shell data. The guard is best-effort; it is not a shell security engine.

Settings are loaded in `src/config/settings.rs`. Global settings come from `~/.config/cake/settings.toml`, project settings from `<project>/.cake/settings.toml`, and project values override global values. Command policy uses parallel dedicated `policy.toml` files. Configuration code must remain below client code in the architecture dependency direction: `config` cannot import `clients`. Put policy parsing, layering, canonicalization, and source types in `src/config/command_policy.rs` or a small `src/config/command_policy/` module. Put command preprocessing and evaluation that depend on Bash views under `src/clients/tools/command_policy/` or the renamed `bash_safety/` location. Update `ARCHITECTURE.md` and code maps if ownership moves.

The CLI is defined under `src/cli/`; inspect the current command organization before adding `cake policy show` and `cake policy check`. Follow existing introspection patterns, especially ADR-009. The `check` command accepts a command string after `--`, evaluates it without spawning a process, and explains candidates and matches. It must never execute the supplied command.

Per-session telemetry is defined in `src/session_telemetry.rs` and written as newline-delimited JSON to the sidecar established by ADR-007. Structured decisions must be carried from policy evaluation to the telemetry writer. Do not add structured policy records to the persisted conversation JSONL, and do not put commands, matched text, patterns, messages, or suggestions in telemetry.

ADR-015 at `docs/adr/015-declarative-command-policy.md` is authoritative. Task 241 at `.ahm/tasks/active/241.md` defines acceptance. Relevant durable authorities are `docs/security.md`, `docs/configuration.md`, `docs/integrations.md`, and `ARCHITECTURE.md`; implementation mechanics belong in the command-policy code and tests.

An embedded policy is version-1 policy data compiled into Cake that reproduces current rules. An inspection view is one documented representation: `raw_command`, `raw_segment`, or `normalized_segment`. A candidate is one instance of that view. Provenance identifies which source supplied each effective field. A canonical digest is a version-tagged SHA-256 hash over deterministic serialization of the complete ordered effective policy. A policy decision is the deduplicated result for one rule during one Bash call.

## Plan of Work

### Milestone 1: Define and resolve policy data

Create a configuration-layer model before changing Bash execution. Separate serde-backed version-1 file types from resolved runtime types so strict deserialization can reject unknown fields and distinguish definitions from overrides. Definitions require a source-owned namespaced ID, `block` or `warn`, static message, optional suggestion, and a matcher containing one view plus nonempty `all` and/or `any` predicates and optional `none`. Overrides identify an inherited rule and may replace every field except identity; only overrides may select `allow`.

Load exact fixed locations: `~/.config/cake/policy.toml` and `<project>/.cake/policy.toml`. Resolve embedded, user, then project policy. Enforce `cake/*`, `user/*`, and `project/*` ownership. Reject duplicate definitions, unknown override IDs, same-source overrides, empty predicate arrays, `none`-only matchers, invalid regex, missing or unsupported versions, and whole-policy replacement. Preserve definition order and inherited position through overrides. Profiles do not participate.

Add embedded version-1 policy data under a source location such as `src/config/default-command-policy.toml` and compile it with `include_str!`. Map all current checks to durable `cake/*` IDs. Embedded parse or validation failure is a build/test defect, never a silent fallback.

Implement field-level provenance. Canonically serialize resolved ordered rules using a dedicated structure, not TOML bytes. Hash a format prefix plus canonical bytes with SHA-256 and render `sha256:<lowercase hex>`. Include matcher, decision, message, and suggestion; exclude paths, comments, timestamps, and ineffective history. Test that formatting-only differences keep the digest stable and effective changes alter it.

Add a benchmark target that measures complete policy evaluation with deterministic 10-, 100-, and 1,000-rule fixtures. Establish the straightforward evaluate-every-rule implementation as the reference. If measurements show a material need, add a compiled quick-rejection structure such as `RegexSet`, then use equivalence tests across representative and generated commands to prove that optimized evaluation returns the same matched IDs, primary decision, occurrence counts, and telemetry projection. If measurements do not justify optimization, retain the reference evaluator and record the evidence in `Surprises & Discoveries`.

The milestone is complete when focused tests demonstrate embedded-only behavior, correct overlays, every strict validation error, accurate provenance, deterministic digest generation, and recorded 10/100/1,000-rule benchmark results. Update the living sections before continuing.

### Milestone 2: Replace compiled checks with declarative evaluation

Refactor `src/clients/tools/bash_safety/` into a small preprocessing-and-evaluation boundary. Preserve bounded quote-aware splitting, literal shell `-c` extraction, command-substitution extraction, whitespace normalization, and shell-data masking. Document this surface as frozen: correctness fixes remain allowed, new shell constructs do not.

Compile regexes during invocation construction, never per Bash call. For each rule, produce candidates for its selected view. A candidate satisfies the matcher when every `all` predicate matches, at least one `any` predicate matches when present, and no `none` predicate matches. Evaluate every rule, deduplicate by ID, retain an approximate satisfying-candidate count, and choose the primary by action strength, source precedence, and rule order. Return structured evaluation instead of formatted `Result<Vec<String>, String>`.

Move the exact behavior of all nine hard blocks and the existing `rg` warning into embedded data. Preserve positive and negative fixtures including combined flags, `--force-with-lease`, safe restore forms, temporary-directory `rm -rf` exceptions, quoted data, wrappers, substitutions, and commit-message substitution. Do not delete old tests until equivalent table-driven declarative coverage exists.

The milestone is complete when compiled check functions and the registry are gone, preprocessing is materially smaller or clearly limited, and focused safety tests pass against embedded data.

### Milestone 3: Integrate snapshots, Bash output, and telemetry

Resolve policy once during CLI startup and thread an immutable shared snapshot through agent and tool context. Do not reload inside Bash. Invalid policy must fail before any model API request with source path, rule ID where available, and an actionable reason.

If a block matches, do not spawn Bash; return the primary static message and suggestion plus secondary IDs. If warnings but no blocks match, execute normally and prepend the primary warning after ordinary output formatting, including failure, truncation, and binary-output paths. Warnings do not change exit status or error classification. Apply policy under `DangerFullAccess`.

Extend telemetry with a metadata-only event or equivalent structured attachment for every matching rule. Include session and invocation context, turn and call IDs, rule ID, decision, definition source, contributing sources, approximate count, and digest. Exclude command and rule content. Preserve telemetry compatibility with a new tagged variant and serialization tests; ensure blocked preflight calls correlate with tool-call telemetry.

The milestone is complete when tests prove default blocking, warning execution, named allow behavior with an independent block, policy under disabled sandboxing, immutable snapshots, and metadata-only telemetry.

### Milestone 4: Add policy inspection CLI

Add `cake policy show` and `cake policy show <RULE_ID>`. Load through the normal resolver, display digest and ordered rules, and identify the source of every effective field. Unknown IDs fail as input errors with a close-ID suggestion when practical.

Add `cake policy check -- <COMMAND>`. Invoke the same preprocessing and evaluator used by Bash but never create a process. Human output identifies final decision, primary rule, secondary matches, candidates, and digest, and states that evaluation is best-effort rather than secure shell interpretation. Define no-match and invalid-argument behavior. Follow existing CLI exit conventions; record in the Decision Log whether a policy block is successful inspection output or a nonzero diagnostic result. Define machine-readable behavior explicitly if output modes can reach these commands.

The milestone is complete when help, show, targeted show, blocked check, warned check, allowed/no-match check, and non-execution tests pass.

### Milestone 5: Document, verify, and complete managed work

Update `docs/configuration.md` with user-owned policy locations and precedence, `docs/security.md` with the relationship between command policy and the OS boundary, and `docs/integrations.md` with any new CLI or telemetry compatibility semantics. Update `ARCHITECTURE.md` only if a durable boundary or invariant changes. Keep field-level schema and evaluation mechanics in code, tests, and generated help.

Run formatting, focused tests, serialization or snapshot tests, then `just ci`. Run the narrowest feasible Linux-sensitive check or report the platform gap. Exercise isolated temporary HOME and project directories for embedded-only, overlay, invalid-file, and snapshot behavior.

Before completion, fill task 241 Acceptance Notes with evidence, update this retrospective, move the plan to `.ahm/exec-plans/completed/`, change the task path, and use `ahm task complete 241`. Do not commit or push unless explicitly requested.

## Concrete Steps

Work from `/Users/travisennis/Projects/cake`. Record the baseline:

    git status --short
    cargo test bash_safety
    cargo test session_telemetry

Inspect construction and implementation locations:

    rg -n "SettingsLoader|ToolContext|execute_bash|SessionTelemetryRecord|Debug" src
    rg -n "bash_safety|git_reset|rg_replace_flag" src docs

After Milestone 1, run the actual policy test filter, expected to resemble:

    cargo test command_policy

Run the policy benchmark using the benchmark target selected during implementation, for example:

    cargo bench --bench command_policy

Expect embedded defaults, version rejection, namespace enforcement, unknown override, precedence, provenance, regex, and digest tests to pass. Record median or otherwise stable comparative timings for 10, 100, and 1,000 rules; do not add a prefilter without evidence and equivalence coverage.

After Milestone 2, run:

    cargo test bash_safety
    cargo test bash
    cargo run -- policy check -- 'git push --force origin main'

The check output must report `block` and the selected `cake/*` ID without executing the command.

Use temporary directories for end-to-end policy files; never modify the real global policy:

    tmp_home="$(mktemp -d)"
    tmp_project="$(mktemp -d)"
    mkdir -p "$tmp_home/.config/cake" "$tmp_project/.cake"
    HOME="$tmp_home" cargo run --manifest-path /Users/travisennis/Projects/cake/Cargo.toml -- policy show

Create fixture files in automated tests or temporary directories and verify project fields override user fields. Record exact transcripts here during implementation.

After telemetry integration, run:

    cargo test session_telemetry
    cargo test tool_call

Inspect serialized records and confirm they include policy metadata but no command, regex, message, or suggestion.

Before handoff, run commands required by `CONTRIBUTING.md`, including:

    cargo fmt --check
    just ci
    ahm --dry-run index

Record exact results in `Progress` and `Outcomes & Retrospective`. If full CI is blocked, run the narrowest checks and report the gap.

## Validation and Acceptance

With neither policy file present, all current blocked and warned commands produce the same decisions as before. Embedded data contains nine blocks and the existing warning with known safe exceptions tested.

A user override changes a `cake/*` rule and a project override changes it again. `policy show` displays the result and per-field provenance. Formatting-equivalent policies share a digest; effective changes alter it.

Invalid files fail before API or tool execution. Tests cover malformed TOML, version errors, unknown fields, namespaces, duplicates, unknown overrides, empty and `none`-only matchers, and invalid regex.

Benchmarks report evaluation behavior for 10, 100, and 1,000 rules. If an optimized prefilter exists, equivalence tests prove that it cannot alter matches, resolution, counts, or telemetry relative to the straightforward evaluator.

An `allow` override neutralizes only its named rule; an independent block still wins. New `allow` definitions and whole-policy replacement are rejected.

`policy check` reports the same result as Bash preflight and never executes the command. Warnings prepend guidance without changing execution classification. Blocks prevent spawn. Policy remains active under `danger-full-access`.

Telemetry records one metadata-only decision per matching rule with count and digest, without duplicating command or policy contents. Main session JSONL remains structurally unchanged.

Documentation distinguishes best-effort policy from the OS sandbox and calls out that project policy has final authority and can relax inherited policy. `just ci` passes, with any Linux gap reported.

## Idempotence and Recovery

Policy loading and inspection are read-only. Tests isolate HOME and project paths. Implement additively: introduce types and evaluator behind tests before removing compiled checks, and retain old fixtures until declarative coverage passes.

If migration changes behavior, restore the old path, add a failing compatibility test, and correct embedded data or preprocessing before deleting old code. Invalid policy intentionally prevents startup; recovery is to fix or move the file outside Cake and start a new invocation. Do not add a bypass flag.

Preserve unrelated worktree changes. Untracked files present before this plan must not be deleted, staged, or modified. Do not use destructive Git commands.

## Artifacts and Notes

The proposed architectural source is `docs/adr/015-declarative-command-policy.md`; accept it before implementation. The managed task is `.ahm/tasks/active/241.md`. Add concise test transcripts and final examples here during implementation.

The intended schema shape is illustrative until Milestone 1 fixes exact serde spelling, but semantics must remain:

    version = 1

    [overrides."cake/git-push-force"]
    decision = "warn"
    message = "Force-pushing is permitted here, but verify the target."

    [[rules]]
    id = "project/protect-main"
    decision = "block"
    message = "Direct pushes to main are prohibited."

    [rules.match]
    view = "normalized_segment"
    all = ["(?i)\\bgit\\s+push\\b", "\\bmain\\b"]

## Interfaces and Dependencies

Use existing dependencies where possible. Confirm whether `regex` and a SHA-256 implementation already exist before editing `Cargo.toml`. Do not update dependency versions. If a direct dependency is required, use minimal features, keep lockfile consistency, and document binary-size and security implications.

The configuration layer should expose an immutable resolved policy without importing client modules, conceptually:

    pub struct ResolvedCommandPolicy {
        pub rules: Vec<ResolvedCommandRule>,
        pub digest: CommandPolicyDigest,
    }

    pub struct ResolvedCommandRule {
        pub id: CommandRuleId,
        pub decision: CommandPolicyDecision,
        pub matcher: ResolvedCommandMatcher,
        pub message: String,
        pub suggestion: Option<String>,
        pub provenance: CommandRuleProvenance,
    }

Compile regexes once during construction. The evaluator shared by Bash and `policy check` should conceptually expose:

    pub fn evaluate(&self, command: &str) -> CommandPolicyEvaluation;

Normal Bash output must not expose candidates; `policy check` may. Telemetry receives a metadata-only projection.

Revision note (2026-07-12): Initial ExecPlan written alongside proposed ADR-015. It records the reviewed design, repository orientation, incremental milestones, compatibility tests, security verification, and managed-work completion steps. ADR lifecycle wording was revised after Travis chose to defer acceptance until implementation begins. The plan was later amended to require 10/100/1,000-rule benchmarks, permit only semantics-preserving measured optimization, and defer auditable expiring overrides to a future schema.

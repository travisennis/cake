# Replace the prose mirror with essential documentation

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must remain current as work proceeds. This plan follows the guidance from `ahm context plan`.

## Purpose / Big Picture

Cake's implementation is already legible to contributors and coding agents, but the repository maintains several overlapping prose descriptions of that implementation. After this change, users will have concise operating guidance, integrators will have explicit compatibility contracts, contributors will have one validation workflow, and maintainers will have durable architectural and security intent without maintaining a prose copy of the source tree. A reusable root guide will let the same documentation discipline be applied to other projects.

## Progress

- [x] (2026-07-24 02:17Z) Inventory the current documentation and compare representative claims with the implementation.
- [x] (2026-07-24 02:35Z) Rewrite the root documents around distinct audiences and responsibilities.
- [x] (2026-07-24 02:35Z) Replace overlapping design, reference, and guardrail prose with focused configuration, integration, and security documents.
- [x] (2026-07-24 02:35Z) Preserve the ADR archive while making its historical role explicit.
- [x] (2026-07-24 03:00Z) Validate links, targeted formatting/lint, managed
  indexes, and the resulting diff.
- [x] (2026-07-24 03:00Z) Run local three-pass preflight and apply its finding:
  do not make the already-red repository-wide docs check a focused-doc gate.
- [x] (2026-07-24 03:14Z) Run external Codex review to clean output and prepare
  task 293 for completion.

## Surprises & Discoveries

- Observation: The current Markdown corpus contains 5,285 lines, while sessions, sandboxing, settings, tools, and skills are each described in many files. Evidence: the initial inventory found session-related prose in 30 files and tool-related prose in 20 files.
- Observation: Duplication produced contradictory facts. Evidence: the removed
  Chat Completions reference said system messages use the developer role, while
  `src/clients/chat_completions.rs` maps `Role::System` to `system`.
- Observation: `just docs-check` is already red on untouched historical
  Markdown formatting, so it cannot be the required gate for a focused
  documentation edit.
  Evidence: the first run reported Panache reflow diffs in unchanged skills and
  managed records; targeted checks on all rewritten living documents pass.

## Decision Log

- Decision: Keep living documentation only for user operation, external contracts, security boundaries, durable architecture, and contributor workflow. Rationale: These communicate audience needs, guarantees, and intent that cannot be recovered reliably from implementation mechanics alone. Date/Author: 2026-07-24, Codex.
- Decision: Preserve ADR files as historical records, but do not treat them as current feature specifications. Rationale: Their durable value is the original context and tradeoffs; rewriting them to track every implementation change destroys that history. Date/Author: 2026-07-24, Codex.
- Decision: Put the reusable method in root-level `DOCUMENTATION.md`. Rationale: The requested artifact should be visible and portable without being confused with Cake-specific configuration or integration documentation. Date/Author: 2026-07-24, Codex.

## Outcomes & Retrospective

Cake now has one concise authority for each durable documentation concern:
README for first use, configuration for user-controlled behavior, integrations
for machine-facing contracts, security for trust boundaries, architecture for
stable structure and invariants, and contributing guidance for workflow. The
ADR archive remains historical, and root-level `DOCUMENTATION.md` captures the
portable method.

The rewrite reduced living project guidance from a sprawling mirrored hierarchy
to 835 lines. Including the preserved ADR archive, the durable corpus is 1,859
lines, down from 5,285. Review findings were valuable because they identified
compact compatibility facts that deserved preservation: stream exit behavior,
retry semantics, sandbox grants, XDG paths, hook matchers, tool scheduling, and
skill discovery. Those facts now live in the appropriate authority instead of
recreating feature-by-feature design documents.

The main remaining constraint is pre-existing repository-wide Markdown
formatting debt. This change deliberately validates the rewritten and touched
documentation rather than reformatting unrelated historical records.

## Context and Orientation

The root documents currently mix audiences. `README.md` is a long user manual, `ARCHITECTURE.md` is a volatile symbol-level codemap, `CONTRIBUTING.md` duplicates commands from `justfile`, and `AGENTS.md` routes agents through a large guardrail and design-doc hierarchy. The `docs/` directory contains guardrails, design documents, API references, a domain summary, and ADRs. The Rust source, CLI help, tests, snapshots, `justfile`, and managed `.ahm` records already own most implementation facts, commands, expected serialization, and future work.

The rewrite must preserve Cake's compatibility surfaces: CLI behavior, tool semantics, sandbox boundaries, session formats, configuration shape, provider behavior, machine-readable output, and task workflow metadata. No runtime behavior changes are in scope.

## Plan of Work

Rewrite `README.md` as installation and quick-start guidance. Rewrite `AGENTS.md` as a short operating contract without topic-by-topic document routing. Rewrite `ARCHITECTURE.md` around components, data flow, boundaries, and invariants rather than symbols. Rewrite `CONTRIBUTING.md` to point to canonical `just` recipes instead of copying them.

Create root-level `DOCUMENTATION.md` as a project-independent method for deciding what documentation deserves to exist, assigning one authority per fact, preferring executable sources, and auditing documentation over time.

Replace the living `docs/` hierarchy with `docs/configuration.md`, `docs/integrations.md`, and `docs/security.md`. Keep `docs/adr/` and rewrite its index introduction so ADRs are explicitly historical. Remove the domain, guardrail, design-doc, and API-reference documents after their necessary contract material has a new authoritative home. Update repository links and the pull-request template to match the new structure.

## Concrete Steps

Work from `/Users/travisennis/Projects/cake`.

1. Edit the root and focused `docs/` files using repository-relative links.
2. Remove superseded living documents and search for references to their paths.
3. Run `ahm index` after moving this plan or changing managed metadata.
4. Run targeted Panache format/lint checks, a local Markdown-link check,
   `ahm doctor`, and `git diff --check`.
5. Run the repository's Codex review tool until no actionable findings remain, then use the preflight skill and address worthwhile findings.
6. Fill the task acceptance notes and this retrospective, move this plan to `.ahm/exec-plans/completed/`, update the task path, and complete task 293.

## Validation and Acceptance

The rewrite is accepted when a new user can install and configure Cake from the README and configuration guide; an integrator can find session, JSON output, hook, and toolbox contracts; a security reviewer can identify the sandbox and trusted-extension boundaries; a contributor can find canonical checks without duplicated command catalogs; and an agent can follow `AGENTS.md` without loading a second prose implementation.

Targeted Panache checks must pass for every rewritten living document. Every repository-relative Markdown link in the retained corpus must resolve. `ahm doctor` must report a valid workflow, and `git diff --check` must report no whitespace errors. The pre-existing repository-wide Panache formatting debt is outside this rewrite.

## Idempotence and Recovery

All documentation edits are ordinary version-controlled changes on `docs/lean-documentation-reset`. Removed documents remain recoverable from Git. Generated indexes are changed only through `ahm`. Re-running formatting, link, or workflow validation is safe.

## Artifacts and Notes

The initial worktree was clean. Task 293 and this plan are the only managed artifacts created for the rewrite.

Final evidence: living project guidance is 835 lines; including the preserved
ADR archive, the durable corpus is 1,859 lines, down from 5,285. Targeted Panache
checks, changed-file relative-link validation, `ahm doctor`, the preflight skill
validator, generated CLI help comparison, active-work reference search,
`git diff --check`, local three-pass preflight, and external Codex review pass.

## Interfaces and Dependencies

No runtime interfaces or dependencies change. The retained documentation must derive CLI facts from clap declarations in `src/main.rs`, configuration facts from `src/config/settings.rs`, persistence and stream facts from `src/types/session.rs`, and sandbox facts from `src/clients/tools/sandbox/`. `justfile` remains the canonical command catalog, and `.ahm` remains the authority for tasks, research, plans, and future work.

Revision note (2026-07-24): Created the initial self-contained plan after the documentation audit and before editing durable project documentation.

Revision note (2026-07-24): Recorded final validation, review-driven contract
corrections, measured outcomes, and completion.

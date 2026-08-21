## Centralize resolved-path classification and tool capabilities

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

## Purpose / Big Picture

Cake decides what a tool call may do from five hand-written classifiers in `src/clients/tools/mod.rs`. Two of them classify filesystem paths (`validate_path_with_dirs` for paths that exist, the nonexisting branch of `resolve_path_for_write_scheduling` for paths that do not), and three infer tool semantics from tool-name strings: `tool_uses_repair_pass` (which tools get the JSON repair pass), `ToolRegistry::retain_read_safe_tools` (which tools the read-only sandbox policy hides), and `ToolRegistry::mutating_target` (which tool mutates which file). These classifiers live outside tool registration, so a new mutating tool can be registered while the read-only filter, hook payload shaping, or same-file scheduling silently misses it.

The two path classifiers have already drifted. For a path under both the working directory and an `--add-dir` ancestor (overlapping grants), an existing file classifies read-write because `validate_path_with_dirs` checks the cwd first, but a not-yet-existing file is rejected because the scheduling resolver checks the read-only list first. Edit can modify such files today while Write cannot create siblings next to them.

After this work, one classifier decides access for existing and prospective paths alike, and each `ToolEntry` declares its capabilities (argument repair, read safety, optional mutation-target extraction) at registration. Scheduling, sandbox filtering, and hook payload shaping consume those declarations. A new tool becomes safe to add because its registration is the single place that states what it is.

Observable outcomes:

- Creating a file with Write inside the working directory works even when an `--add-dir` grant is an ancestor of the working directory, matching Edit.
- All other allow/deny outcomes, error messages, hook payloads, toolbox privilege defaults, and platform behaviors stay byte-compatible.
- Adding a mutating tool without marking it unsafe is impossible: capability defaults fail closed (not read-safe, no repair pass, no mutation target).

## Progress

- [x] (2026-08-21) Read issue #277, security doc, tools module, call sites.
- [x] (2026-08-21) Claim issue, branch refactor/centralize-path-classification.
- [x] (2026-08-21) Milestone 1: shared resolved-path classification; matrix tests; commit.
- [x] (2026-08-22) Milestone 2: ToolEntry capabilities consumed at scheduling, agent, hooks sites; commit.
- [x] (2026-08-22) Full gate `just ci`; security impact analysis recorded on the issue.

## Surprises & Discoveries

- Observation: The overlapping-grants divergence was real and user-visible: with an `--add-dir` ancestor of cwd, `Write` rejected every new file in the project (`write_scheduling_rejects_new_file_in_read_only_additional_dir` encodes the strict case) while `Edit` accepted existing files. Evidence: precedence order comparison of `validate_path_with_dirs` (cwd → temp → settings → additional → skill) against the nonexisting branch of `resolve_path_for_write_scheduling` (additional deny → require cwd/temp/settings).
- Observation: The prospective-write branch never consulted skill directories, so a new file under a skill dir was denied as "outside the working directory" rather than as read-only; denial held either way.
- Observation: Hooks repaired `tool_input` for any name starting `tb__`, including hallucinated toolbox names that are never registered; the registry- derived flag now reports false for unregistered names, matching how `argument_compensation_events` already gated on registration.

## Decision Log

- Decision: The shared classifier keeps `validate_path_with_dirs` precedence (cwd, temp, settings read-write; additional, skill read-only). Rationale: it is the older, documented in-process enforcement for Read/Edit/Write; changing existing-file behavior would break compatibility, and the overlap resolution "cwd wins" matches what Edit does today. Date/Author: 2026-08-21/ox-alpha.
- Decision: Callers keep formatting their own error messages so model-visible strings stay byte-identical; the shared classifier returns only the access level or "outside". Rationale: model-visible errors are a compatibility surface. Date/Author: 2026-08-21/ox-alpha.
- Decision: Capability defaults fail closed --- `read_safe: false`, `repairs_arguments: false`, no mutation target. Rationale: an unmarked new tool must be excluded by the read-only policy rather than exposed. Date/Author: 2026-08-21/ox-alpha.
- Decision: Hook payload repair flag comes from the caller's registry via new parameters on `pre_tool_use`/`post_tool_use`, not from a name pattern. Rationale: HookRunner has no registry access; per-call threading keeps the wire payloads unchanged while making unregistered names strict-parse. Date/Author: 2026-08-22/ox-alpha.
- Decision: The Edit invalid-arguments compensation check stays keyed to the edit module. It is compensation bookkeeping beside the parser it mirrors, not a security or scheduling decision named in the issue. Date/Author: 2026-08-21/ox-alpha.

## Context and Orientation

Cake is a binary Rust crate. Tools live in `src/clients/tools/`. The registry (`ToolRegistry`) holds `ToolEntry` values; each entry pairs a model-facing `Tool` definition with an executor closure. `src/clients/tools/mod.rs` owns path validation. Key functions today:

- `validate_path_with_dirs(path_str, cwd, temp_dirs, settings_dirs,   additional_dirs, skill_dirs)` canonicalizes an existing path and returns `ValidatedPath { canonical, access }` where `PathAccess` is `ReadWrite` or `ReadOnly`. Precedence: cwd, temp, settings (ReadWrite); additional, skill (ReadOnly); otherwise error. Used by Read, Edit execution, and the existing-file branch of write scheduling.
- `resolve_path_for_write_scheduling(context, path_str)` resolves write targets that may not exist: for existing paths it delegates to `validate_path_for_write`; otherwise it splits the path into a deepest existing base via `resolve_write_path`, canonicalizes the base, applies its own allow-list check (read-only deny first), and appends lexically normalized pending components. Used by Write execution and by both tools' `mutating_target` for same-file scheduling (ADR-013 grouping in `scheduling.rs`).
- `retain_read_safe_tools()` drops entries whose *name* is Edit/Write or starts `tb__` when the sandbox policy is read-only (agent.rs, `format_tool_list_section`).
- `tool_uses_repair_pass(name)` reports whether an executor repairs arguments before parsing; re-exported through `src/clients/mod.rs` for `hooks.rs`, which shapes hook `tool_input` payloads (#185), and used by `argument_compensation_events` in the agent loop.
- `ToolRegistry::mutating_target(context, name, arguments)` matches on `"Edit"`/`"Write"` to dispatch to per-tool extractors in `edit.rs`/ `write.rs`.

Hook payloads are a compatibility surface documented in docs/integrations.md; their JSON shapes must not change.

## Plan of Work

Milestone 1 --- shared classification. In `src/clients/tools/mod.rs`: extract from `validate_path_with_dirs` a pure function `classify_resolved_path(canonical, cwd, temp_dirs, settings_dirs, additional_dirs, skill_dirs) -> Option<PathAccess>` holding the grant table and precedence, `None` meaning outside all grants. `validate_path_with_dirs` canonicalizes then calls it. The nonexisting branch of `resolve_path_for_write_scheduling` canonicalizes its base and calls the same function, treating `ReadOnly` as the read-only rejection and `None` as the outside rejection, preserving each message string verbatim. Add matrix tests: overlapping grants for existing and nonexistent paths, symlinked read-only ancestor for a new file, skill-dir message alignment.

Milestone 2 --- declared capabilities. Add `ToolCapabilities { repairs_arguments: bool, read_safe: bool, mutating_target: Option<MutationTargetFn> }` with fail-closed defaults; give `ToolEntry` a `capabilities` field plus builder methods. Registration in `default_tool_registry` marks Bash/Read `read_safe()`, Edit/Write `repairs_arguments()` + `mutates_path(...)`, and `toolbox_tool_entry` `repairs_arguments()` only. Rewrite `retain_read_safe_tools`, `ToolRegistry::mutating_target`, and a new `ToolRegistry::repairs_arguments(name)` to read capabilities. Delete free fn `tool_uses_repair_pass` and its re-export; thread the registry-derived flag through `argument_compensation_events` and through `HookRunner::pre_tool_use`/`post_tool_use` (and their internal payload helpers) from `agent_loop.rs`. Update hook tests' signatures. Add tests proving scheduling and filtering derive from capabilities, not names.

## Concrete Steps

Working directory: `/Users/travisennis/Projects/cake`.

Milestone 1:

```
just claim 277 && just branch refactor/centralize-path-classification   # done
# edits in src/clients/tools/mod.rs per Plan of Work
cargo test --bin cake tools::tests::write_scheduling
cargo test --bin cake tools::tests::overlap
cargo fmt && cargo clippy --all-features --all-targets -- -D warnings
git add -p src/clients/tools/mod.rs && git commit -m "refactor(tools): share resolved-path grant classification between existing and prospective writes"
```

Milestone 2:

```
# edits in mod.rs, toolbox.rs, clients/mod.rs, agent_loop.rs, hooks.rs, hooks_tests.rs
cargo test --bin cake hooks_tests
cargo test --bin cake tools::
cargo fmt && cargo clippy --all-features --all-targets -- -D warnings
```

Final verification:

```
just ci
```

Expected: green across toolchain check, Linux compile, fmt, clippy (both feature modes), tests, coverage/change-risk, import lint, module-size lint.

## Validation and Acceptance

Behavior a human can verify after the change:

1. Overlapping grants. From a checkout, run cake with `--add-dir <parent-of-cwd>` and ask Write to create `<cwd>/new.txt`. Before: rejected as read-only. After: created. Edit of an existing file in cwd behaved the same before and after.
2. Matrix tests in `cargo test --bin cake tools::` cover: existing vs nonexistent paths under cwd/temp/settings/additional/skill grants; overlapping grants resolving writable in both branches; a new file whose parent chain traverses a symlink into a read-only grant still denied; read-only filtering dropping only non-read-safe entries.
3. Read-only sandbox policy still offers exactly Bash+Read (`format_tool_list_section_read_only_excludes_mutating_tools`).
4. Hook payloads unchanged: `cargo test --bin cake hooks_tests` passes with the flag sourced from the registry.
5. `just ci` green, including sandbox platform tests on macOS; Linux compile checked by the gate.

## Idempotence and Recovery

All steps are ordinary code edits re-runnable after a failed attempt; `git` restores state via `git restore src/clients/tools/mod.rs` (and sibling files) back to the last commit. Commits are additive; no destructive steps. If `ci/cargo-crap-baseline.json` conflicts arise, take master's copy and regenerate with `just change-risk-baseline`; never hand-edit it.

## Artifacts and Notes

- Issue: travisennis/cake#277 (audit findings S16-1/S16-2, baseline a05570c).
- Divergence proof sketch: `validate_path_with_dirs` orders cwd before additional dirs; the nonexisting branch of `resolve_path_for_write_scheduling` denies on additional dirs before consulting cwd, so identical logical locations classified differently depending on existence.

## Interfaces and Dependencies

- `crate::clients::tools::classify_resolved_path` --- single grant-table classifier used by both validation entry points.
- `crate::clients::tools::ToolCapabilities` and `ToolEntry::capabilities` with builders `read_safe`, `repairs_arguments`, `mutates_path`.
- `crate::clients::tools::ToolRegistry::repairs_arguments(&str) -> bool`.
- Removal of `crate::clients::tools::tool_uses_repair_pass` and its `crate::clients` re-export; `HookRunner::pre_tool_use`/`post_tool_use` gain a `repairs_arguments: bool` parameter fed from the registry.
- No dependency changes; no wire-format changes.

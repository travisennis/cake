# Project-Customizable Sandbox Paths ([sandbox] Settings)

This ExecPlan is a living document, maintained per docs/workflow/exec-plans.md. The sections Progress, Surprises & Discoveries, Decision Log, and Outcomes & Retrospective must be kept current as work proceeds.

This plan implements issue 71. The architecture decision is recorded in ADR 019 (`docs/adr/019-project-customizable-sandbox-paths.md`), which this plan cites and implements.

## Purpose / Big Picture

Cake's Bash tool runs model-generated shell commands inside an OS filesystem sandbox with a deny-default policy. Before this change, the allowed path set came from hard-coded toolchain paths in `SandboxConfig::extend_with_toolchain_paths`, the `--add-dir` CLI flag (one invocation, read-only), and the `directories` settings key (persistent, read-write). Projects could not grant sandbox access to extra paths through settings, and there was no way to grant read-only access to one executable file without granting its whole directory.

After this change:

- A project can add `.cake/settings.toml` with `[sandbox].read_only = ["~/.local/bin/claude"]` and run `claude` from sandboxed Bash while sibling binaries in `~/.local/bin` stay denied.
- A project can add `[sandbox].writable = ["~/.claude", "~/.cache/claude"]` so Claude Code can write its state under the sandbox.
- `directories = ["~/foo"]` now expands `~` and works (previously it silently ignored the path).
- Under `--sandbox read-only`, `[sandbox].writable` paths are demoted to read-only, matching the existing `directories` behavior.
- Hard-coded codex paths are removed from the source; codex keeps working through user settings.

How to see it working: `cargo test` passes with new settings-merge, tilde-expansion, and Seatbelt file-literal tests; on macOS, a sandboxed Bash run that executes a `[sandbox].read_only` file succeeds while a sibling file is denied.

## Progress

- [x] (2026-08-08) ExecPlan written; ADR 019 created; issue 71 claimed; branch `feat/sandbox-settings` cut.
- [x] (2026-08-08) Milestone 1: `[sandbox]` settings schema, union merge, `~` expansion, shared `expand_home` helper. Committed as `feat(sandbox): add [sandbox] path grants with tilde expansion` (3347eee).
- [x] (2026-08-08) Milestone 2: `main.rs` plumbing --- `[sandbox].read_only` into `additional_dirs`, `[sandbox].writable` into `settings_dirs` (same commit).
- [x] (2026-08-08) Milestone 3: macOS Seatbelt file-vs-directory rules; Linux Landlock file-grant test. Committed as `feat(sandbox): emit literal Seatbelt rules for file grants` (f8f056a).
- [x] (2026-08-08) Milestone 4: read-only demotion coverage; docs (configuration.md, security.md) (same commit).
- [x] (2026-08-08) Milestone 5: codex hard-coded path removal commit; personal settings.toml migration. Committed as `refactor(sandbox): remove hard-coded codex paths in favor of settings` (edb3731).
- [x] (2026-08-08) Full verification: `cargo fmt`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --bins` (1134 passed), `just ci` (green, including coverage and CC gates), plus a new macOS end-to-end sandbox test (`test_sandbox_read_only_file_grant_runs_file_but_denies_sibling`). Fix commit `fix(sandbox): restore merge_settings baseline complexity` (94d4ac5); test commit `test(sandbox): verify read-only file grants end-to-end on macOS` (e239c10).

## Surprises & Discoveries

- Observation: `valid_settings_dirs` in `src/main.rs` filters on `p.is_dir()`, which would silently drop `[sandbox].read_only` file entries. The read-only filter must accept existing files and directories. Evidence: `src/main.rs:560` `fn valid_settings_dirs`.
- Observation: `push_dirs_with_canonical` and `deduplicated_with_canonical` in `src/clients/tools/sandbox/mod.rs` only require `exists()`, so they already accept file paths; only the macOS Seatbelt rule emission (always `allow_subpath`) needs file-vs-directory handling. Evidence: `src/clients/tools/sandbox/mod.rs:590` and `macos.rs:330`.
- Observation: `Path::is_file()` returns false for the synthetic paths used in existing unit tests (`/workspace`, `/usr`, `/etc`), so the file-vs-directory seatbelt logic preserves existing snapshot expectations.
- Observation: adding sandbox unions to `SettingsLoader::load_with_profile` pushed it past clippy's 100-line `too_many_lines` and 7-argument `too_many_arguments` gates. Refactored the loader around a `SettingsAccumulator` struct, which also keeps the grandfathered CC-18 `load_with_profile` from growing. Evidence: clippy clean after refactor.
- Observation: `SettingsAccumulator` profile overlays had to be cloned before iteration (`overlays.clone()`) because `apply_profile_overlay(&mut self)` conflicts with the immutable borrow of `acc.profiles`. Evidence: E0502 during Milestone 1.
- Observation: the Edit tool's backslash-quote escaping is fragile for Rust string literals containing `\"`; one malformed edit produced an unterminated string literal. Fixed with a byte-exact python patch and verified with `od -c`. Evidence: `src/clients/tools/sandbox/macos.rs` line 787.

## Decision Log

- Decision: Follow ADR 019 --- structured `[sandbox]` section with `read_only` (read + execute) and `writable` (read + write + execute), union-merged across global, project, and profile, exactly like `directories`. Rationale: matches existing precedence semantics and keeps one schema for both access classes. Date/Author: 2026-08-08, cake agent.
- Decision: Expand `~` at settings-merge time (in `SettingsLoader`), so `LoadedSettings.directories` and `.sandbox` already carry expanded paths, and keep relative paths untouched (they resolve from invocation cwd as today). Rationale: single expansion site, matches the issue's "canonicalization and warnings once at settings-load time". Date/Author: 2026-08-08, cake agent.
- Decision: `[sandbox].writable` entries must be existing directories (same filter as `directories`); `[sandbox].read_only` entries may be existing files or directories (the whole point is a single executable). Rationale: a writable grant to a file is not a supported use case in this task; read-only file grants are. Date/Author: 2026-08-08, cake agent.
- Decision: Apply the file-vs-directory rule emission to all three Seatbelt path classes (writable, system_paths, readable), not only readable, because the same latent bug would bite `[sandbox].writable` file entries. Date/Author: 2026-08-08, cake agent.
- Decision: Refactor the settings loader around a `SettingsAccumulator` struct to satisfy clippy gates while keeping `load_with_profile`'s grandfathered CC unchanged. Rationale: avoids growing a grandfathered function and reduces `merge_settings` from 9 to 2 arguments. Date/Author: 2026-08-08, cake agent.

## Outcomes & Retrospective

The `[sandbox]` settings mechanism landed as planned and all acceptance criteria are met on macOS: a `[sandbox].read_only` file grant runs from sandboxed Bash while a sibling file is denied (verified end to end through a real `sandbox-exec` run), `[sandbox].writable` paths feed the read-write sandbox set and demote under `--sandbox read-only`, `directories = ["~/foo"]` now expands `~`, and the hard-coded codex paths are gone from the source with the user's `~/.config/cake/settings.toml` carrying the replacement grants. Linux Landlock file grants are covered by a unit test on rule-path classification; live Landlock enforcement could not be exercised on macOS and remains a stated verification gap for Linux CI/review.

Implementation notes: the loader was refactored around `SettingsAccumulator` to satisfy clippy's line/argument gates without growing the grandfathered `load_with_profile`; `merge_settings` kept its baseline CC by merging the `[sandbox]` section unconditionally via `unwrap_or_default`. The `~` expansion helper moved from `skills.rs` to `config/mod.rs` and is shared by `directories`, `[sandbox]`, and `skills.path`.

## Context and Orientation

Cake is a Rust 2024 binary-only AI coding assistant CLI. Settings load from `~/.config/cake/settings.toml` (global) and `{project}/.cake/settings.toml` (project), merged by `SettingsLoader` in `src/config/settings.rs`. A selected profile (`--profile NAME`) overlays behavior from `[profiles.NAME]`. The merged result is `LoadedSettings` (same file). `main.rs` turns `LoadedSettings` into a `ToolContext` (`src/clients/tools/mod.rs`) whose `additional_dirs` (read-only) and `settings_dirs` (read-write) feed both the in-process `validate_path_with_dirs` checks for the Read/Edit/Write tools and the OS sandbox via `SandboxConfig::build` (`src/clients/tools/sandbox/mod.rs`).

The OS sandbox: on macOS, `sandbox-exec` applies a Seatbelt profile generated by `MacOsSandbox::generate_profile` (`src/clients/tools/sandbox/macos.rs`), which emits `allow_subpath` rules for every path in `config.writable` / `config.system_paths` / `config.readable`, plus `allow_literal` rules for ancestor directories. On Linux, `LandlockSandbox::prepare_ruleset` (`src/clients/tools/sandbox/linux.rs`) adds `path_beneath_rules` for the same three lists.

Sandbox policies: `WorkspaceWrite` (default), `ReadOnly` (temp dirs stay writable; workspace, toolchain, and settings dirs demote to readable), `DangerFullAccess`. `SandboxConfig::partition_read_only` performs the demotion in `src/clients/tools/sandbox/mod.rs`.

Key files by full path:

- `src/config/settings.rs` --- `Settings`, `ProfileSettings`, `LoadedSettings`, `SettingsLoader::load_with_profile`, `merge_settings`.
- `src/config/skills.rs` --- `parse_skill_path_list` and private `expand_home` (to be extracted).
- `src/config/mod.rs` --- module root; destination for the shared `expand_home` helper.
- `src/main.rs` --- `load_run_resources`, `valid_settings_dirs`.
- `src/clients/tools/mod.rs` --- `ToolContext`, `validate_path_with_dirs`.
- `src/clients/tools/sandbox/mod.rs` --- `SandboxConfig::build_with_policy`, `push_dirs_with_canonical`, `partition_read_only`, `extend_with_toolchain_paths` (codex entries to remove).
- `src/clients/tools/sandbox/macos.rs` --- `generate_profile`, `SeatbeltProfileBuilder`.
- `src/clients/tools/sandbox/linux.rs` --- `prepare_ruleset`, `prepare_rule_paths`.
- `docs/configuration.md`, `docs/security.md` --- authorities to update.

## Plan of Work

### Milestone 1 --- Settings schema, merge, tilde expansion

In `src/config/settings.rs`:

- Add `pub struct SandboxSettings` with `#[serde(default)] pub read_only: Vec<String>` and `#[serde(default)] pub writable: Vec<String>`. Derive `Debug, Clone, Default, Serialize, Deserialize`.
- Add `#[serde(default)] pub sandbox: Option<SandboxSettings>` to `Settings` (top level).
- Add `#[serde(default)] pub sandbox: Option<SandboxSettings>` to `ProfileSettings` (profile overlay).
- Add `pub sandbox: SandboxSettings` to `LoadedSettings`.
- In `load_with_profile`: keep two accumulators `sandbox_read_only: HashSet<String>` and `sandbox_writable: HashSet<String>` mirroring the existing `directories` accumulator. Thread them through `merge_settings` (union for top-level `[sandbox]`), and union profile `[profiles.X.sandbox]` lists in the profile loop.
- Expand `~` when inserting: map each `directories`, `sandbox.read_only`, and `sandbox.writable` string through the shared `expand_home` before inserting into the accumulator.
- Build `LoadedSettings.sandbox` from the accumulators.

In `src/config/mod.rs`:

- Add `pub(crate) fn expand_home(path: PathBuf) -> PathBuf` moved verbatim from `src/config/skills.rs` (handle exact `~` and `~/`/`~\` prefixes via `dirs::home_dir()`).
- Update `src/config/skills.rs` to use `crate::config::expand_home` instead of its private copy.

Tests in `src/config/settings_tests.rs`:

- Union merge across global + project + profile for both `[sandbox]` keys, deduplicated.
- `~` expansion for `directories`, `[sandbox].read_only`, `[sandbox].writable` (using the existing `with_var("HOME", ...)` helper so tests are hermetic).
- Absent `[sandbox]` defaults to empty.

### Milestone 2 --- ToolContext plumbing in main.rs

In `src/main.rs`:

- Change `valid_settings_dirs(loaded)` to include `loaded.sandbox.writable` (existing-dir filter with a file-only log warning on missing paths), keeping `directories` behavior.
- Add `valid_sandbox_read_only_dirs(loaded)` that accepts existing files and directories, warning (file-only log) on missing paths.
- In `load_run_resources`: `let mut additional_dirs = additional_dirs; additional_dirs.extend(Self::valid_sandbox_read_only_dirs(&loaded));` before `ToolContext::new`. `settings_dirs` already flows into `ToolContext::new` unchanged.

Behavior check: `SandboxConfig::build` already routes `additional_dirs` → `readable` (read-only) and `settings_dirs` → `writable` (read-write, demoted by `partition_read_only` under `ReadOnly`). `validate_path_with_dirs` in `src/clients/tools/mod.rs` already treats `additional_dirs` as `ReadOnly` and `settings_dirs` as `ReadWrite`, so file grants work for Read/Edit/Write without changes there.

### Milestone 3 --- Platform rule emission

In `src/clients/tools/sandbox/macos.rs` `generate_profile`:

- Add a helper `fn allow_path_access(profile: &mut SeatbeltProfileBuilder, permissions: &str, path: &Path)` that emits `allow_literal(permissions, path)` when `path.is_file()` and `allow_subpath(permissions, path)` otherwise.
- Use it in the writable loop (`"file-read* file-write*"`), the system_paths loop (`"file-read*"`), and the readable loop (`"file-read*"`).
- Ancestor literals are already generated from all three lists; no change needed there.

Tests in `src/clients/tools/sandbox/macos.rs`:

- A real temp file in `config.readable` produces `(allow file-read* (literal "..."))` and no subpath rule for the file's parent directory; a sibling file has no rule at all.
- Directory paths still produce `allow_subpath` (existing tests already cover this).

In `src/clients/tools/sandbox/linux.rs`:

- Add a `#[cfg(target_os = "linux")]`-gated unit test that `prepare_rule_paths` keeps a real file in `readable` (the existing `rule_paths_are_filtered_and_classified_before_fork` only exercises directories). State the platform verification gap: Landlock runtime behavior for file grants is not executable in macOS CI and is verified on Linux per the guardrail in docs/guardrails.

### Milestone 4 --- Read-only demotion coverage and docs

- Extend `read_only_policy_moves_workspace_and_toolchain_to_readable` (or add a sibling test) in `src/clients/tools/sandbox/mod.rs`: with `settings_dirs` containing a configured sandbox writable path, `SandboxConfig::build_with_policy(ReadOnly, ...)` moves it to `readable` and out of `writable`.
- Update `docs/configuration.md`: document the `[sandbox]` section, `read_only` vs `writable` semantics, file-vs-directory grants, union merge across global/project/profile, `~` expansion (including the fixed `directories` behavior), and read-only demotion.
- Update `docs/security.md`: document the trust model (project-level `.cake/settings.toml` grants are fully trusted, no deny-list, no trust prompt) and that `[sandbox]` feeds both the OS sandbox and in-process validation.

### Milestone 5 --- Codex hard-coded path removal (separate commit)

In `src/clients/tools/sandbox/mod.rs`:

- Remove the codex entries from `extend_with_toolchain_paths` (`.codex`, `.cache/codex`, `.local/share/codex`, `.local/state/codex`) and the macOS `Library/Caches/codex`, `Library/Application Support/codex` entries.
- Remove the corresponding expectations from `build_covers_common_toolchains` (cross-platform and macOS lists).

Outside the repo:

- Add the codex paths to the personal `~/.config/cake/settings.toml` as `[sandbox].writable` so codex functionality continues to work (one-time migration, per the issue).

## Concrete Steps

All commands run from `/Users/travisennis/Projects/cake-2` on branch `feat/sandbox-settings`.

1. `cargo test config::settings` --- baseline green before editing.
2. Implement Milestone 1, then `cargo test config::settings_tests`.
3. Implement Milestone 2 and 3, then `cargo test tools::sandbox tools::edit tools::read tools::write` (macOS seatbelt tests included).
4. `cargo fmt` and `cargo clippy --all-targets --all-features -- -D warnings`.
5. Implement Milestone 4 docs, then `just docs-check` (markdown lint; panache).
6. Implement Milestone 5 as its own commit, then rerun the sandbox tests.
7. `just ci` --- the primary local gate. `just cc-check` for complexity. Recapture `ci/cargo-crap-baseline.json` with `just change-risk-baseline` only if coverage/complexity deltas are intentional and reported by `just check-coverage`.

## Validation and Acceptance

- `cargo test` (full suite) green on macOS; Linux-specific Landlock behavior stated as a verification gap.
- `just ci` green, `just cc-check` green.
- New unit tests demonstrate: union merge of both `[sandbox]` keys; `~` expansion for `directories` and `[sandbox]`; a `[sandbox].read_only` file emits a Seatbelt literal rule with no sibling access; `[sandbox].writable` demotes under `ReadOnly`.
- `grep -n codex src/clients/tools/sandbox/mod.rs` returns nothing.
- `docs/configuration.md` and `docs/security.md` describe the new surface; no second implementation description.

## Idempotence and Recovery

- Every `cargo` step is safe to rerun. `just change-risk-baseline` rewrites `ci/cargo-crap-baseline.json`; run it only after confirming `just check-coverage` reports intentional regressions, and commit it in the same commit as the code that caused them. Never resolve a conflict in that file by hand.
- The personal `~/.config/cake/settings.toml` edit is additive (new `[sandbox]` section); back it up before editing and confirm the TOML still parses with `cake` afterwards.

## Artifacts and Notes

None yet. Transcripts will be added as milestones complete.

## Interfaces and Dependencies

- `crate::config::settings::SandboxSettings` --- new struct; fields `read_only: Vec<String>`, `writable: Vec<String>`.
- `crate::config::settings::LoadedSettings::sandbox: SandboxSettings` --- merged union result consumed by `src/main.rs`.
- `crate::config::expand_home(PathBuf) -> PathBuf` --- shared tilde expansion used by settings merge and `skills::parse_skill_path_list`.
- `SandboxConfig::build(context: &ToolContext)` --- unchanged signature; receives file grants through `additional_dirs` and directory grants through `settings_dirs`.
- `MacOsSandbox::generate_profile` --- internal file-vs-directory rule emission.
- `LandlockSandbox::prepare_rule_paths` --- unchanged signature; verified with a file-path test.

# Require Explicit Trust for Authority-Bearing Project Configuration

This ExecPlan is a living document, maintained per `docs/workflow/exec-plans.md`. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept current as work proceeds.

## Purpose / Big Picture

A `git clone && cake` run must not let the cloned repository redirect the user's provider credential, widen the Bash sandbox, replace the command-safety gate, or start project hooks and toolbox executables before the user has reviewed and accepted that authority. Cake will keep project configuration useful for low-risk behavior, but authority-bearing project configuration will require an explicit trust record. Model definitions become global-only, even in a trusted repository, so a project can select a model by name but cannot define a new endpoint or credential source.

After this change, someone can run Cake in an unreviewed checkout and get a clear, fail-closed diagnostic instead of a silent credential or sandbox change. They can then run `cake trust` to review and trust that snapshot, use `--trust-project` for a single CI invocation, or use `--no-project-config` to ignore automatic project `.cake/` configuration entirely. Read-only inspection commands can still report what the project asks for without applying it. A project that only selects a global model, narrows tools, sets limits, or supplies prompt/skill content keeps working without a trust record.

The behavior is observable in a scratch repository: with an untrusted project `[sandbox].writable` grant, `cake debug models --json` reports the trust state but does not apply the grant; an agent run exits `3` before resolving the provider credential or starting any project hook, gate, or toolbox executable; after `cake trust`, the same run proceeds; after editing a trusted file, the next run fails with the changed-file list.

This work implements accepted ADR 053, `docs/adr/053-trust-project-configuration-by-capability.md`. Provider identity pinning is a separate follow-on task (052); this plan only makes model definitions global-only so that pinning can subsequently compare a stable, user-owned definition.

## Progress

- [x] (2026-11-02T12:55:30Z) Read task 142, accepted ADR 053, `docs/security.md`, `docs/configuration.md`, `docs/integrations.md`, `src/config/settings.rs`, `src/config/hooks.rs`, `src/config/toolbox.rs`, `src/config/gates.rs`, `src/prompts/mod.rs`, `src/main.rs`, the CLI modules, and the relevant tests.
- [x] (2026-11-02T12:55:30Z) Confirmed there is no existing trust-record, capability-classification, or project-settings provenance implementation.
- [x] (2026-11-02T12:55:30Z) Drafted this ExecPlan. No implementation has started.
- [ ] Milestone 1: trust-record and manifest primitives.
- [ ] Milestone 2: source-aware settings and global-only model definitions.
- [ ] Milestone 3: trust enforcement, CLI, and startup order.
- [ ] Milestone 4: hooks, toolbox, prompts, and sandbox grants consume only validated project data.
- [ ] Milestone 5: diagnostics, telemetry, documentation, and test-fixture migration.
- [ ] Final verification and preflight.

## Surprises & Discoveries

- Observation: `SettingsLoader::settings_paths` returns the global path first and the project path second, and `parse_settings_file` reads the path again after existence checks. There is no source object, digest, or provenance carried into the merge. Evidence: `src/config/settings.rs:932-950`, `src/config/settings.rs:1099-1117`.
- Observation: project `[[models]]` currently replace global entries because `add_models_to_map` inserts into the map after parsing each file, and `merge_settings` accepts a `Settings` value without an origin. Evidence: `src/config/settings.rs:1223-1253`, `src/config/settings.rs:1432-1462`.
- Observation: `load_run_resources` creates configured writable directories and describes toolbox executables before model resolution and provider credential resolution. A trust check placed after that function would be too late. Evidence: `src/main.rs:631-740`, `src/main.rs:1319-1338`.
- Observation: `HooksLoader::load` reads the project hook files directly, and `attach_hooks` runs after session creation and gate setup. The trust decision must happen before any of those calls, and hook parsing should consume the same bytes the trust check hashed. Evidence: `src/config/hooks.rs:236-285`, `src/main.rs:919-935`.
- Observation: `resolve_system_prompt` automatically reads `.cake/system.md` before settings `system_prompt`, so `--no-project-config` needs a prompt-construction flag rather than only a settings-loader change. Evidence: `src/prompts/mod.rs:171-214`.
- Observation: `debug models` uses the same `SettingsLoader` as an agent run and therefore currently applies project authority even though its output is read-only. It needs an inspection mode that returns low-authority settings plus a trust report. Evidence: `src/cli/debug.rs:74-100`.
- Observation: `SettingsLoader::load` and its profile variants have five production call sites but 82 test references, and many tests deliberately put `[[models]]` in project settings. The test fixtures must move model definitions to global settings; the production loader can keep test-only convenience wrappers. Evidence: `rg -n "SettingsLoader::load|load_with_profile" src`.
- Observation: the repository already depends on `sha2` and `serde_json`, so the trust manifest and record require no new crate. Evidence: `src/session_telemetry.rs` uses `sha2`; `Cargo.toml` carries it transitively as a direct dependency.
- Observation: `SessionMeta` already stores `model`, `model_config`, `tools`, and `system_prompt`; adding provider identity is deliberately deferred to task 052. This plan must not change session format version 4. Evidence: `src/types/session.rs:567-595`.

## Decision Log

- Decision: The trust snapshot is the only project-settings source the loader merges. The snapshot reads `.cake/settings.toml` bytes once, hashes those bytes, parses the same bytes into `Settings`, and exposes the parsed value to `SettingsLoader`. Rationale: this makes hash-and-parse the same bytes true by construction and prevents a check/use race between trust validation and configuration parsing. Date/Author: 2026-11-02, Cake agent.

- Decision: Project `[[models]]` definitions are a settings error in every mode except `--no-project-config`, even after trust. A trusted project may select a global model by name, but may not define an endpoint, API type, credential source, provider headers, or request-shape controls. Rationale: ADR 053 makes model definitions user-owned; trust permits reviewed extensions, not an arbitrary credential destination. Date/Author: 2026-11-02, Cake agent.

- Decision: A project `[tools].enabled` value may only narrow the inherited effective selection. If it names a tool absent from the inherited list, Cake rejects it as a settings error rather than treating it as trustable authority. Rationale: the ADR classifies tool selection as low-authority only when it narrows; rejecting broadening keeps the low-authority class honest and avoids adding a second trust-required setting not named by the ADR. Date/Author: 2026-11-02, Cake agent.

- Decision: Authority-bearing keys are detected in every project profile, including profiles not selected by the current invocation. Rationale: the project settings file is the reviewed snapshot, and a later invocation can select any profile. Treating an unselected authority overlay as harmless would let a trusted snapshot change behavior without a new review. Date/Author: 2026-11-02, Cake agent.

- Decision: Read-only inspection commands (`cake trust --status`, `cake debug models`, `cake debug skills`, and `cake gates list`) use an inspection mode that strips authority-bearing project keys, applies low-authority project keys, and reports trust state. They do not fail merely because authority is untrusted. Enforcing commands (agent runs, `cake gates doctor`, and `cake bash check`) fail closed before applying or starting anything. Rationale: ADR 053 requires diagnostics to remain usable without turning inspection into an authority path. Date/Author: 2026-11-02, Cake agent.

- Decision: The trust record is a deterministic, versioned JSON manifest with no timestamp. Each entry carries the relative path, SHA-256 of the file bytes, and whether the executable bit is set. The record is created owner-only (`0600`, parent `0700`) through a temporary file and atomic rename. Rationale: deterministic output makes status and tests stable; the executable bit matters because a non-executable file under `.cake/tools` becomes authority when chmodded. Date/Author: 2026-11-02, Cake agent.

- Decision: Reject a symlinked `.cake` directory, a symlinked `settings.toml`, hook file, or any entry under `.cake/tools`. Other `.cake/` symlinks are omitted from the manifest, and a symlinked `.cake/system.md` is skipped rather than followed. Rationale: authority and automatically-read project files must stay inside the project root; unrelated symlinks are not authority and should not break low-authority project configuration. Date/Author: 2026-11-02, Cake agent.

- Decision: Trust failures and configuration defects exit `3`. `cake trust --check` exits `0` when the project is trusted or has no authority-bearing configuration, and `3` when authority is untrusted, changed, invalid, or unsafe. Rationale: exit `3` is Cake's existing input/configuration class. Date/Author: 2026-11-02, Cake agent.

- Decision: `--trust-project`, `--no-project-config`, `CAKE_TRUST_PROJECT=1`, and `CAKE_PROJECT_CONFIG=off` are one-run modes that never write a trust record. Conflicting effective modes are a fail-closed input error. The `cake trust` management command ignores these environment overrides and always evaluates persistent state. Rationale: a CI flag must not silently become persistent state, and an ambiguous security mode must not pick a winner by accident. Date/Author: 2026-11-02, Cake agent.

- Decision: Telemetry records the effective trust mode, a one-way project-root key, and an authority-file count, never the project root or file paths. `cake trust --status --json` and `debug models --json` may report local paths because they are explicit local diagnostics. Rationale: telemetry's existing minimization rule excludes absolute working directories and project content. Date/Author: 2026-11-02, Cake agent.

- Decision: No new crate dependency. The implementation uses `sha2`, `serde`, `serde_json`, `toml`, and Unix `PermissionsExt` already available in the repository. Date/Author: 2026-11-02, Cake agent.

- Decision: Existing `SettingsLoader::load` and profile convenience methods become `#[cfg(test)]` direct-read helpers that still enforce project-model rejection and tool narrowing. Production code uses one trust-aware entry point. Rationale: the binary crate has no external API compatibility burden, and this keeps production from acquiring a second, untrusted loading path. Date/Author: 2026-11-02, Cake agent.

- Decision: Existing integration tests that stage `[[models]]` in project settings move those definitions to a new global-settings fixture helper. Tests that need project authority explicitly run `cake trust` or use `--trust-project`; the default test helper does not auto-trust. Rationale: auto-trust would make the suite unable to prove the new boundary. Date/Author: 2026-11-02, Cake agent.

## Outcomes & Retrospective

To be completed during implementation. The intended outcome is a fail-closed project-configuration boundary with a small, auditable trust record; unchanged global settings; global-only model definitions; and inspection commands that can explain untrusted configuration without applying it.

## Context and Orientation

Cake is a Rust 2024 binary crate. The CLI resolves configuration, session mode, model credentials, sandbox policy, prompts, skills, hooks, gates, and toolbox tools before it builds an agent. `src/main.rs` owns the root run orchestration; `src/config/` owns settings, hooks, toolbox discovery, gates, sessions, and data-directory resolution; `src/cli/` owns subcommands and output; `src/prompts/` owns system-prompt and initial-message assembly; `src/clients/` owns the agent loop, providers, and tools.

The current project configuration model is fully trusted. `SettingsLoader` merges `<config>/cake/settings.toml` with `<working-directory>/.cake/settings.toml`; project scalars override global scalars, project `[[models]]` entries replace global entries, path grants are unioned, and project hooks and `.cake/tools` executables are discovered automatically. This plan changes where project authority is granted, not how the sandbox, gate, hook, or toolbox protocols enforce it afterward.

The three trust classes are:

- Low-authority project settings load without a trust record. These are `default_model` and profile `default_model` that name a global model; profile selection and non-authority profile overlays; `[tools].enabled` when it only narrows; `[limits]`; project `AGENTS.md`; and skills under the default project `.agents/skills/` root. They can change behavior or cost but cannot add filesystem authority, read arbitrary host files, start executables, or select a new provider endpoint.
- Authority-bearing project settings require a trust record. These are top-level and profile `directories`; `[sandbox].read_only` and `.writable`; `[tools.bash].gates`; every `[gates.<name>]` table; the legacy `[tools.bash.judge]` table; `system_prompt`; `skills.path`; `.cake/hooks.json`; `.cake/hooks.local.json`; and everything under `.cake/tools/`.
- Model definitions are global-only. A project may not contain `[[models]]` even after trust. A project selects a global entry through `default_model`, a profile, or `--model`.

An authority-bearing project file is a regular file inside the project root that Cake reads, parses, or executes as part of project configuration. A trust record is a user-owned local file under `<config>/cake/trust/<project-key>.json` that records the canonical project root and a manifest of those files. A manifest entry is a project-relative path, the SHA-256 digest of the file bytes, and an executable flag. A project key is a SHA-256 digest of the canonical project root, used only as a stable filename; the record also carries the root so a collision or relocation is detectable. A trust snapshot is the set of bytes Cake reads while creating or checking the manifest. A one-run trust mode bypasses the persistent record for one invocation without writing state. The inspection mode is the read-only configuration mode that strips authority-bearing project keys and returns a trust report.

Startup order matters because several steps have side effects or read arbitrary host data. `src/main.rs` currently validates inputs and may create a worktree, then `load_run_resources` loads settings, reads AGENTS.md, discovers skills, prepares writable sandbox directories (creating missing ones), and describes toolbox executables. Only after that does it resolve the model and provider credential, start gates, create a session, and load hooks. The trust decision must be made inside the settings-loading step, before any of those later effects. A trust failure must therefore occur before `prepare_writable_dirs`, `load_toolbox_tools`, `ResolvedModelConfig::resolve`, `with_policy_gates`, session creation, and `HooksLoader`.

The primary files are `src/config/settings.rs` (settings merge), `src/config/hooks.rs` (hook parsing), `src/config/toolbox.rs` (toolbox discovery and describe), `src/config/gates.rs` (gate configuration), `src/prompts/mod.rs` (system prompt), `src/main.rs` (startup order), `src/cli/debug.rs` (inspection), `src/cli/gates.rs` and `src/cli/bash.rs` (gate inspection and enforcement), `src/exit_code.rs` (error classes), and `src/session_telemetry.rs` (telemetry). New code goes in `src/config/trust.rs` and `src/cli/trust.rs`; tests go in new focused modules plus the existing tests.

The project-local `.cake/` directory is the input to this boundary. Global `<config>/cake/` configuration remains trusted. Project `AGENTS.md`, project default-root skills, and `.cake/system.md` remain untrusted model input; they are not authority and cannot add grants or execute code. A project prompt can still inject instructions, and a trusted project can still run arbitrary reviewed code with Cake's ambient authority. Those residual risks stay documented.

## Plan of Work

The implementation proceeds in five milestones. First, build and test the trust-record and manifest primitives without wiring them into startup. Second, make settings loading source-aware and reject project model definitions, with authority classification available as data. Third, add the `cake trust` command and one-run flags, and enforce the decision at the earliest point in startup. Fourth, make hooks, toolbox discovery, prompts, and sandbox grants consume only validated project data, and add trust reporting to inspection commands. Fifth, record the effective trust mode in telemetry, update the documentation and generated scaffolding, migrate the integration-test fixtures from project models to global models, and run the full verification.

Each milestone keeps the tree buildable. Until Milestone 3, the new primitives are unused; the existing behavior is unchanged. Milestone 3 is the point where an unreviewed authority-bearing project starts failing closed. Milestone 4 closes the remaining side-effect paths. Milestone 5 makes the boundary observable and documented.

## Milestones

### Milestone 1: Trust Record and Manifest Primitives

Overview: At the end of this milestone, Cake has a tested, owner-only trust record format and a manifest scanner for `.cake/`, but no startup path uses them. The unit tests prove that a record can be written atomically, read back, compared, and invalidated by changed, added, removed, or mode-changed files.

Repository Context: Add `src/config/trust.rs` and register it in `src/config/mod.rs`. Use `sha2::Sha256`, `serde`, `serde_json`, and `std::fs`. On Unix, use `std::os::unix::fs::PermissionsExt`; on non-Unix, omit mode checks and keep the same record shape. This module must not import `src/clients/` or start any process.

Plan of Work: Define `ProjectConfigMode` with the effective modes `Enforce`, `Inspect`, and `Ignore`, plus the one-run trust source (`Persistent`, `OneRun`, `None`). Define `TrustState` with `NotRequired`, `Trusted`, `Untrusted`, `Changed`, and `Invalid`. Define `TrustedFile { path: String, sha256: String, executable: bool }` and `TrustRecord { version: u32, project_root: String, files: Vec<TrustedFile> }`. Define `ManifestChange { path: String, change: ManifestChangeKind }` with `Added`, `Removed`, `Modified`, and `ModeChanged`. Define `AuthoritySource { path: PathBuf, kind: AuthorityKind, keys: Vec<String> }`, where `AuthorityKind` is `Settings`, `Hooks`, or `ToolboxFile`.

Implement `canonical_project_root(project_dir: &Path) -> Result<PathBuf, TrustError>` using `std::fs::canonicalize`. Implement `project_key(root: &Path) -> String` as lowercase hex SHA-256 of the canonical path bytes. Implement `trust_record_path(config_dir: &Path, root: &Path) -> PathBuf` as `<config_dir>/cake/trust/<project-key>.json`. Config-dir resolution remains `crate::config::config_dir()`.

Implement `TrustRecord::read(path)` and `TrustRecord::write_atomic(path, record)`. `read` must reject a symlink, a non-regular file, a version other than `1`, and a serialized `project_root` that does not equal the canonical root supplied by the caller. On Unix, it must also reject a record whose mode has group or other bits set. `write_atomic` must create the trust directory if needed, set it to `0700` on Unix, create a uniquely named temporary file in that directory with `0600`, write and sync the JSON, rename it over the destination, and clean the temporary file on any failure. A failed write must leave any existing record unchanged.

Implement `scan_project_cake(project_root: &Path) -> Result<ProjectManifest, TrustError>`. The manifest scanner walks `.cake/` with `symlink_metadata`, not `metadata`. It includes every regular file under `.cake/`, recursively, sorted by normalized UTF-8 relative path, and records the executable bit. It rejects a symlinked `.cake` directory, a symlinked `.cake/settings.toml`, a symlinked `.cake/hooks.json` or `.cake/hooks.local.json`, and any symlink under `.cake/tools/`, including a symlinked `.cake/tools` directory. It skips symlinks elsewhere under `.cake/` and reports them only in debug logging. It rejects non-UTF-8 paths and entries that are neither regular files nor directories. It rejects a `settings.toml`, hook file, or `.cake/tools` that is not a regular file or directory as appropriate. It does not parse settings, read hook JSON, or execute anything.

Implement `TrustRecord::compare(&self, manifest: &ProjectManifest) -> Vec<ManifestChange>` comparing path, digest, and executable flag in both directions. A record with the same entries has no changes; a missing or extra file is `Added` or `Removed`; a different digest is `Modified`; a changed executable bit is `ModeChanged`.

Add focused tests in `src/config/trust_tests.rs`: record round trip; atomic overwrite leaves the old record on write failure; Unix mode `0600` and directory `0700`; record symlink and bad-version rejection; manifest sorting and digest stability; added, removed, modified, and executable-bit changes; symlinked settings, hook, and toolbox entries rejected; non-UTF-8 path rejected where the platform allows constructing one; project-key stability across repeated canonicalization and difference across two roots.

Acceptance: `cargo test trust` passes. No production startup path changes yet. The tests use isolated `tempfile` directories and set `XDG_CONFIG_HOME`/`HOME` only when exercising the config-dir helper; no test reads or writes the user's real trust records.

### Milestone 2: Source-Aware Settings and Global-Only Models

Overview: At the end of this milestone, settings loading knows which file supplied each value, project `[[models]]` definitions are rejected, project `tools.enabled` broadening is rejected, and the authority-bearing surface can be classified from the same bytes that will be merged. Existing production behavior for global settings and low-authority project settings is preserved.

Repository Context: Work in `src/config/settings.rs`, `src/config/settings_tests.rs`, and the new trust module. `SettingsLoader` has five production call sites (`src/main.rs`, `src/cli/debug.rs` twice, `src/cli/bash.rs`, `src/cli/gates.rs`) and many test call sites. Add `src/config/settings_load.rs` only if the source types make `settings.rs` materially harder to navigate; the existing file is already over the module-size report threshold, so keep new trust logic in `src/config/trust.rs`.

Plan of Work: Introduce `SettingsOrigin { Global, Project }` and `SettingsSource { path, origin, content, sha256, settings, warnings }`. Add `SettingsLoader::read_source(path, origin) -> Result<Option<SettingsSource>, SettingsError>` that reads the file bytes once, computes the digest, parses the typed `Settings` and the raw TOML value from the same string, computes unknown-key warnings, and returns the source. `SettingsLoader::merge_settings_file` and `parse_settings_file` are replaced by this source path. The global source remains optional; a missing global settings file is an empty source.

Add `SettingsError::ProjectModelsUnsupported { path: PathBuf, names: Vec<String> }` with a message that names each project model and the migration: move the entries to `<config>/cake/settings.toml` and select one by name from the project. Add `SettingsError::ProjectToolsBroadened { path: PathBuf, added: Vec<String> }` with a message that names the tools that were not in the inherited selection. Update `src/exit_code.rs::classify_settings_error` to classify both as input errors (`3`) with no wildcard arm.

Change `SettingsLoader::merge_settings` to take a `SettingsSource` (or its origin/path) rather than a bare `Settings`. When the origin is `Project`, reject any non-empty `settings.models` before adding models. Global models continue to use `add_models_to_map`, including duplicate detection within one file. The project source's warnings continue to carry the project path.

Change `SettingsAccumulator.profiles` from `HashMap<String, Vec<ProfileSettings>>` to a structure that preserves `SettingsOrigin` and source path, for example `HashMap<String, Vec<ProfileOverlay>>` where `ProfileOverlay { settings: ProfileSettings, origin: SettingsOrigin, path: PathBuf }`. `merge_settings` pushes project and global overlays with their origin. `profile_overlays` returns clones. `apply_profile_overlay` reads the overlay's settings and origin.

In `merge_tools_settings`, when the origin is `Project` and `enabled` is `Some`, compare the requested names against the current `acc.tools_enabled`. If the current value is `None`, any list narrows; if it is `Some`, every requested name must already be present. Reject additions before applying the list. In `apply_profile_overlay`, apply the same rule when the overlay origin is `Project`; compare against the effective `self.tools_enabled` at the point the overlay is applied. A project profile may narrow a global profile; it may not add a name. Keep the existing "list replaces lower-precedence selection" behavior for valid narrowing lists.

Add `Settings::without_authority()` for inspection mode. It returns a clone with `gates` cleared, `directories` empty, `sandbox` `None`, `system_prompt` `None`, `skills.path` cleared, `tools.bash.gates` and `tools.bash.judge` cleared, and `models` left untouched (the project model check has already rejected them). For every entry in `profiles`, clear `gates`, `directories`, `sandbox`, `system_prompt`, `skills.path`, `tools.bash.gates`, and `tools.bash.judge`, while preserving `default_model`, `tools.enabled`, `limits`, and `skills.disabled`/`skills.only`. Add a test that the stripped value contains no authority-bearing key and still preserves low-authority overlays.

Add `ProjectAuthority::from_settings(settings: &Settings, settings_path: &Path) -> Vec<AuthoritySource>`. It reports top-level `directories`, `sandbox.read_only`, `sandbox.writable`, every `gates.<name>`, `tools.bash.gates`, `tools.bash.judge`, `system_prompt`, and `skills.path`, plus the same keys inside every profile. It reports hooks files and toolbox files separately from the trust module's manifest scanner. Keep the key labels stable and human-readable, for example `[profiles.review.sandbox].writable`.

Add `ConfigLoadError { Settings(SettingsError), Trust(TrustError) }` with `From` conversions and an exit-code classification of `3`. Production callers use this combined error; `SettingsLoader::load`, `load_with_profile`, and `load_with_profile_and_gate_bypass` become `#[cfg(test)]` convenience wrappers that still use the source-aware merge, reject project models, and enforce tool narrowing. This keeps the old unit tests close to the production merge without leaving an untrusted production entry point.

Update `src/config/settings_tests.rs`: remove the test that expects a project model override; replace it with a test that project models are rejected with the migration message. Add tests that a project `default_model` naming a global model loads untrusted; a project profile with low-authority fields loads; a project profile with `directories`, `sandbox`, `gates`, `system_prompt`, `skills.path`, or `tools.bash` is classified as authority; `tools.enabled` narrowing succeeds and broadening fails for both top-level and profile overlays; `without_authority()` preserves low-authority fields; global settings still merge and override as before.

Acceptance: `cargo test settings` and `cargo test trust` pass. A project with only low-authority settings loads; a project with `[[models]]` fails with exit class `3` and the migration message; a project that broadens `tools.enabled` fails with the added names; global model definitions still work. No production call site uses the test-only loader.

### Milestone 3: Trust Enforcement, CLI, and Startup Order

Overview: At the end of this milestone, an agent run in an unreviewed checkout fails with exit `3` and a complete diagnostic before any credential, hook, gate, toolbox, writable-directory, or session side effect. `cake trust`, `--status`, `--revoke`, and `--check` work, and `--trust-project`/`--no-project-config` select one-run modes.

Repository Context: Add `src/cli/trust.rs` and register `TrustCommand` in `src/cli/mod.rs`. Add trust-aware loading to `src/config/settings.rs` and `src/config/trust.rs`; wire it in `src/main.rs`, `src/cli/debug.rs`, `src/cli/gates.rs`, and `src/cli/bash.rs`. Update `src/main_tests.rs` and add `src/cli/trust_tests.rs`. Extend `src/exit_code.rs` for `ConfigLoadError` and `TrustError`.

Plan of Work: Implement `ProjectConfigSnapshot::load(project_root, config_dir, mode) -> Result<Self, ConfigLoadError>`. If the mode is `Ignore`, return an empty snapshot with a `NoProjectConfig` report and do not read `.cake/`. Otherwise canonicalize the project root, scan `.cake/`, read `.cake/settings.toml` through `SettingsLoader::read_source` if present, classify authority, reject project models, and capture hook file bytes without parsing them. Build the authority report from settings keys plus present hook files plus every regular file under `.cake/tools/`. Compute the manifest from the same file reads where possible; use the settings source's digest for `.cake/settings.toml` and the hook bytes' digests for hook files, rather than reopening them.

Implement `ProjectConfigSnapshot::evaluate(mode) -> Result<ProjectTrustReport, TrustError>` for `Enforce` and `Inspect`. In `Enforce` with no authority-bearing configuration, report `NotRequired` and allow. In `Enforce` with a valid persistent record and an unchanged manifest, report `Trusted` and allow. In `Enforce` with authority and no record, report `Untrusted` and return a `TrustError::Untrusted` diagnostic; with a record that differs, report `Changed` and return a diagnostic listing added, removed, modified, and mode-changed files; with an unreadable, symlinked, wrong-version, wrong-root, or insecure record, report `Invalid` and return a diagnostic. In `OneRun` mode, report `OneRun` and allow after all structural checks pass, without writing a record. In `Inspect`, never fail solely because the record is absent or changed; return the report and a `Settings` value with authority stripped.

Implement `SettingsLoader::load_with_project_config(project_dir, profile, no_gate, mode, config_dir) -> Result<SettingsLoad, ConfigLoadError>`. `SettingsLoad` carries `settings: LoadedSettings` and `trust: ProjectTrustReport`. The loader reads the global source first, asks `ProjectConfigSnapshot::load` for the project source (or `None` for `Ignore`), merges the global source and the project source, applies the selected profile, validates `default_model`, judge model, and gate configuration, and returns the pair. In `Inspect`, merge `settings.without_authority()` for the project source; the project model check still applies. In `Enforce`, merge the full project source only after the trust decision allows it.

Add `cake trust` as a new `Commands` variant. `TrustCommand` uses an action group for `--status`, `--revoke`, and `--check`; no action means "review and trust the current snapshot". `--json` is valid only with `--status`. The command canonicalizes the current directory, loads a `ProjectConfigSnapshot` without applying settings, and:

- Review: print the root, authority sources, and file count; if no authority is present, print that no record is needed and do not write; otherwise write the record atomically and print its path.
- Status: print `not_required`, `trusted`, `untrusted`, `changed`, or `invalid`, the authority sources, and any changes. `--json` emits a `DiagnosticDocument` with `command: "trust status"`, `summary` counts, and `data` containing the state, project root, project key, record path, authority array, and changes array. Status exits `0` even when the state is untrusted; it is a successful query.
- Revoke: remove a regular record for the current canonical root; an absent record is success. Do not follow a symlinked trust directory or record.
- Check: exit `0` for `not_required` or `trusted`; exit `3` for `untrusted`, `changed`, or `invalid`, with the same diagnostic as an agent run.

Add the root flags `--trust-project` and `--no-project-config` to `CodingAssistant`. Make them global flags so inspection and enforcement subcommands can receive them, but reject them in `validate_subcommand_options` for `sessions`, `replay`, `init`, and `trust`, where they have no effect. They conflict with each other. Add `ProjectConfigMode::from_cli` to read `CAKE_TRUST_PROJECT` (only the exact value `1` enables one-run trust) and `CAKE_PROJECT_CONFIG` (only the exact value `off` enables ignore mode). A CLI flag plus a conflicting environment variable, or both environment variables, is an input error (`3`) rather than a silent precedence choice. `cake trust` management ignores these environment variables.

Carry the effective mode in `CommandRunOptions` so `src/cli/debug.rs`, `src/cli/gates.rs`, and `src/cli/bash.rs` use the same value. Classify commands: no subcommand, `cake gates doctor`, and `cake bash check` are enforcing; `cake debug models`, `cake debug skills`, and `cake gates list` are inspecting; `cake sessions`, `cake replay`, `cake init`, and `cake trust` do not load project configuration for authority. A root agent run is enforcing.

In `src/main.rs`, replace `load_settings` with a call to `load_with_project_config` and keep the trust report in `RunResources`. The report must be available to filter toolbox directories, prompt construction, telemetry, and diagnostics. Do not move the trust check later than the first call in `load_run_resources`. If `SettingsLoader` fails, return before `read_agents_files`, skill discovery, `prepare_writable_dirs`, or `load_toolbox_tools`. Confirm by test that an untrusted project with a writable grant does not create the grant directory and does not open a session file.

Add `TrustError` diagnostics with stable, complete text. The untrusted message must list every authority source and key, name the trust-record path, and show the three remedies: `cake trust`, `--trust-project` (or `CAKE_TRUST_PROJECT=1`), and `--no-project-config` (or `CAKE_PROJECT_CONFIG=off`). The changed message must list each changed path and change kind. The unsafe-path message must name the symlink or non-regular path and state that authority-bearing project files must be regular files inside the project root. No message may include a credential value or project file contents.

Update `src/exit_code.rs` so `TrustError` and `ConfigLoadError` are typed input errors (`3`). Keep the existing settings-error classification exhaustive. Add tests that the error class is `3` for untrusted, changed, invalid, and unsafe project configuration.

Add `src/cli/trust_tests.rs` for parsing: default review, each action, `--json` requirement, flag conflicts, environment modes, conflicting environment modes, and trust-command immunity to `CAKE_TRUST_PROJECT`/`CAKE_PROJECT_CONFIG`. Add `src/main_tests.rs` coverage for the new help text and root flags. Add focused unit tests for `ProjectConfigSnapshot::evaluate` covering not-required, trusted, untrusted, changed, invalid, one-run, ignore, inspection stripping, canonical-root separation, and a new worktree root.

Acceptance: `cargo test trust`, `cargo test settings`, and `cargo test main_tests` pass. In a scratch repository with an untrusted project sandbox grant, an agent run exits `3` before a fake writable directory is created, before a marker-writing hook or toolbox describe runs, and before any provider request. `cake trust` writes a `0600` record under the isolated config root; `cake trust --check` then exits `0`; editing a trusted file makes `--check` exit `3` with the changed path; `--revoke` returns to untrusted; `--trust-project` and `--no-project-config` work without writing a record.

### Milestone 4: Validated Hooks, Toolbox, Prompts, and Sandbox Grants

Overview: At the end of this milestone, every remaining project authority path consumes the validated snapshot or is skipped. Project hook JSON is parsed from the bytes that were hashed; project toolbox directories are filtered after trust and never described when untrusted; `--no-project-config` skips automatic `.cake/system.md`; and inspection commands report trust without applying authority.

Repository Context: Work in `src/config/hooks.rs`, `src/config/toolbox.rs`, `src/prompts/mod.rs`, `src/main.rs`, `src/cli/debug.rs`, `src/cli/gates.rs`, and their tests. `ProjectConfigSnapshot` carries `SettingsSource` for settings and `HookFileSource` for `.cake/hooks.json` and `.cake/hooks.local.json`; `HookFileSource` contains the path and the exact bytes read and hashed.

Plan of Work: Change `HooksLoader` to accept global hook paths and a list of validated project hook sources. Add `HooksLoader::load_with_project_sources(global_path, project_sources)` that parses each source's bytes with `serde_json::from_slice` and preserves the existing order (global, project, local) and error messages. Keep `load_from_paths` for tests and global-only callers. In `src/main.rs::attach_hooks`, pass the project hook sources from `RunResources`. If `--no-project-config` is active, pass no project sources. The hook command strings still execute later outside the sandbox; this change makes the trust check happen before parse and execution, not a new hook sandbox.

Change toolbox directory construction so the project-local `.cake/tools` directory is included only when the effective mode allows project authority. Add a helper such as `toolbox_directories_for_project(..., include_project_tools: bool, project_tools_identity: Option<&Path>)` that filters any directory whose canonical identity equals the project `.cake/tools` directory when project tools are not allowed. This also covers `CAKE_TOOLBOX` or `--toolbox` pointing at `.cake/tools`: `--no-project-config` must not re-enable project executables. The trust check has already failed before this helper on an untrusted enforcing run. `load_toolbox_tools` continues to describe only directories that survive the filter; do not describe project tools during `cake trust` or any inspection command.

Add `include_project_prompt: bool` to `resolve_system_prompt` and thread it through `build_initial_prompt_messages_with_enabled_tools` and `RunInputs`. When false, skip the automatic `working_dir/.cake/system.md` source; keep the CLI `--system-prompt`, settings `system_prompt` (already stripped in ignore mode), and user `<config>/cake/system.md` sources. When true, read `.cake/system.md` with `symlink_metadata` and only accept a regular file; a symlink is skipped with a warning rather than followed. Project `AGENTS.md` and default-root project skills remain active in both modes.

Update `src/cli/debug.rs` to use `load_with_project_config` in `Inspect` mode. `debug models` keeps its existing model/default output and adds an additive `project_trust` object to `--json` with state, authority paths/keys, and changes; in text mode, print a trust warning to stderr. `debug skills` prints the same warning when the project is untrusted or changed and uses the stripped settings, so `skills.path` does not contribute roots. `debug models` must not resolve credentials or create a session; it already does not. Do not add a new `debug config` command in this task; task 077b owns that surface, and the `ProjectTrustReport` API is its integration point.

Update `src/cli/gates.rs` so `gates list` uses `Inspect` mode and reports trust state; it may show global gates and global selection when project gates are stripped. `gates doctor` and `src/cli/bash.rs` use `Enforce`, so an untrusted project gate configuration fails before any gate process starts. Add tests with a fake gate whose `describe` writes a marker: an untrusted `gates doctor` and `bash check` leave the marker absent; a trusted run starts the gate.

Add tests for hooks: a project hook file whose command writes a marker; untrusted run leaves it absent; trusted run executes it. Add a test that invalid project hook JSON is reported as a parse error only after trust; an untrusted invalid file reports the trust error first. Add tests for toolbox: a project tool whose describe writes a marker; untrusted, inspection, and `--no-project-config` leave it absent; trusted run describes it. Add prompt tests that `--no-project-config` skips `.cake/system.md` and a symlinked `.cake/system.md` is skipped even when project configuration is otherwise trusted. Add a test that a trusted project `system_prompt` path is applied and an untrusted one is not (inspection strips it; enforcement fails before prompt construction).

Acceptance: `cargo test hooks`, `cargo test toolbox`, `cargo test prompts`, `cargo test debug_models`, `cargo test gates`, and `cargo test trust` pass. Marker-based tests prove no project hook, gate describe/init, or toolbox describe runs before trust. A trusted project hook and tool run normally. `--no-project-config` skips automatic project prompt content and project toolbox discovery. `debug models --json` reports trust state without a provider, hook, gate, or toolbox side effect.

### Milestone 5: Diagnostics, Telemetry, Documentation, and Fixture Migration

Overview: At the end of this milestone, the effective trust mode is visible in session telemetry without leaking paths, the documentation describes the new boundary and migration, `cake init` comments no longer suggest project models, and the existing test suite uses global model definitions while explicitly trusting only the project authority it intends to exercise.

Repository Context: Work in `src/session_telemetry.rs`, `src/main.rs`, `src/cli/init.rs`, `src/cli/init_tests.rs`, `docs/security.md`, `docs/configuration.md`, `docs/integrations.md`, `README.md`, `docs/adr/019-project-customizable-sandbox-paths.md`, `tests/support/mod.rs`, and the integration tests that currently stage `[[models]]` in project settings.

Plan of Work: Add an optional `project_trust` field to `SessionTelemetryRecord::TelemetryInit`. The value carries the effective mode (`persistent`, `one_run`, `not_required`, or `no_project_config`), an optional one-way project-key digest, and an authority-file count. Do not include the project root, authority file paths, settings key names, or file contents. Use `skip_serializing_if = "Option::is_none"` so old telemetry records and consumers keep their shape. Thread the `ProjectTrustReport` from `RunResources` into `attach_session_telemetry`. Add serialization tests that the field is absent for a report-less test fixture and that a one-run mode is visible while no path appears in the serialized record.

Update `src/cli/init.rs` comments so the generated `.cake/settings.toml` says model definitions belong in global `<config>/cake/settings.toml`; a project may only select one by name. Keep the generated file behavior-preserving and comment-only. Update `src/cli/init_tests.rs` assertions for the new wording. Document that `cake init` creates inert files that need no trust record; a project that adds an authority-bearing setting or hook must run `cake trust`.

Update `docs/security.md`: replace the statement that project `.cake/settings.toml` is fully trusted with the three trust classes, the trust-record lifecycle, the one-run flags, the inspection-command exception, the symlink policy, and the residual risks. Update the gate paragraph that says a project may replace the safety gate as trusted input: it may do so only after the project is trusted. Update the bypass-class bullet that says project gate replacement is accepted as trusted configuration to say it is accepted only for a trusted snapshot, and add the trust-record bypass classes. Update the trusted-extensions section so project hooks and `.cake/tools` are not described as automatically trusted. Keep global configuration, the OS sandbox, gate protocol, hook protocol, and toolbox protocol unchanged.

Update `docs/configuration.md`: add a "Project trust" section before models; state global-only model definitions and the migration; document `cake trust`, `--status [--json]`, `--revoke`, `--check`, `--trust-project`, `CAKE_TRUST_PROJECT=1`, `--no-project-config`, and `CAKE_PROJECT_CONFIG=off`; document which project settings are low-authority and which require trust; update the models examples to put `[[models]]` in global settings; update `system_prompt`, `skills.path`, hooks, toolbox, and sandbox sections to say project values require trust; document inspection commands and the exit-3 behavior. Update `README.md` to point to the trust section and state model definitions are global-only.

Update `docs/integrations.md` only where the machine-facing contract changes: add the `trust status --json` diagnostic document shape if it uses `DiagnosticDocument`, and the additive `project_trust` field in `debug models --json` and telemetry. State that session format version 4 is unchanged and that no hook, toolbox, gate, or provider wire shape changes. Keep exit-code `3` as the trust/config class.

Add the partial-supersession note to `docs/adr/019-project-customizable-sandbox-paths.md` under `## More Information`: ADR 053 changes the project-trust consequence while preserving the schema and merge semantics. Do not change ADR 019's accepted status. Update `ARCHITECTURE.md` only if the boundary/invariant section needs the one-sentence invariant that project configuration authority requires an explicit trust decision; do not duplicate the security contract.

Migrate test fixtures. Add `TestEnv::write_global_settings(content)` to `tests/support/mod.rs` that writes `<xdg_config_home>/cake/settings.toml`. Add `TestEnv::trust_project()` that runs `cake trust` in the fixture workspace and `TestEnv::trusted_command()` that adds `--trust-project` for tests that should not write state. Keep `TestEnv::command()` trust-neutral; do not auto-trust. Move the provider/model blocks in `tests/debug_models.rs`, `tests/diagnostic_json.rs`, `tests/exit_codes.rs`, `tests/resume_repair.rs`, `tests/sandbox_grants.rs`, `tests/session_modes.rs`, `tests/session_telemetry.rs`, `tests/stdin_handling.rs`, `tests/tool_selection.rs`, and `tests/toolbox.rs` from project settings to global settings. For tests that intentionally exercise project authority (sandbox grants, project gates, project hooks, project toolbox, project system prompt), add explicit `cake trust` setup or `--trust-project`; keep at least one untrusted test per authority class. Add `tests/trust.rs` for
the real-binary boundary cases.

Add integration tests in `tests/trust.rs`: low-authority project settings run without trust; project models fail with the migration message; untrusted sandbox grants fail with exit `3` and do not create the directory; untrusted hooks, gates, and toolbox describe do not write markers; `cake trust` then `--check` succeeds; changed, added, removed, and mode-changed files fail; symlinked settings/hook/tool fail; `--trust-project` and `CAKE_TRUST_PROJECT=1` work without a record; `--no-project-config` and `CAKE_PROJECT_CONFIG=off` ignore project authority and skip project prompt/tools; conflicting modes fail; `trust --status --json` has the documented shape; a new worktree starts untrusted; existing sessions still load (no format bump). Use a local TCP listener or a mock provider configured so an untrusted run cannot connect; assert the listener saw no connection.

Acceptance: `cargo test telemetry`, `cargo test --test trust`, `cargo test --test debug_models`, `cargo test --test diagnostic_json`, `cargo test --test exit_codes`, `cargo test --test toolbox`, `cargo test --test sandbox_grants`, `cargo test --test session_modes`, `cargo test --test session_telemetry`, `cargo test --test stdin_handling`, `cargo test --test tool_selection`, and `cargo test --test resume_repair` pass. `just docs-check` passes. No project model fixture remains in the default project settings fixture. Existing provider, session, hook, gate, and toolbox protocol tests pass unchanged except for the trust setup.

## Concrete Steps

Work from the repository root, `/Users/travisennis/Projects/cake/cake`. Use the built binary for manual checks, for example `cargo build` followed by `target/debug/cake`, or an installed `cake` on `PATH`.

Run the focused suites as each milestone lands:

```
cargo test trust
cargo test settings
cargo test hooks
cargo test toolbox
cargo test prompts
cargo test gates
cargo test debug_models
cargo test --test trust
```

Run the fast repository gate before handoff:

```
just check
```

Run the security/coverage-sensitive gate because this change adds new functions and platform-conditioned code:

```
just check-coverage
```

Run the documentation gate:

```
just docs-check
```

For a macOS and Linux manual check, use a scratch repository and an isolated config root:

```
scratch=$(mktemp -d)
mkdir -p "$scratch/repo/.cake" "$scratch/home/.config/cake"
cd "$scratch/repo"
git init -q
cat > "$scratch/home/.config/cake/settings.toml" <<'EOF'
default_model = "test"

[[models]]
name = "test"
model = "test-model"
base_url = "http://127.0.0.1:9/v1"
api_key_env = "TRUST_TEST_KEY"
EOF
cat > .cake/settings.toml <<'EOF'
[sandbox]
writable = ["../outside"]
EOF
XDG_CONFIG_HOME="$scratch/home/.config" HOME="$scratch/home" CAKE_DATA_DIR="$scratch/data" \
  cake trust --status
```

Expected: the status names `.cake/settings.toml` and `[sandbox].writable` as untrusted authority and exits `0`.

```
XDG_CONFIG_HOME="$scratch/home/.config" HOME="$scratch/home" CAKE_DATA_DIR="$scratch/data" \
  TRUST_TEST_KEY=fake cake "say hello"
```

Expected: exit `3`; stderr names the project settings, the writable grant, the trust-record path, and the three remedies; no `$scratch/outside` directory is created; no session file is created.

```
XDG_CONFIG_HOME="$scratch/home/.config" HOME="$scratch/home" CAKE_DATA_DIR="$scratch/data" \
  cake trust
XDG_CONFIG_HOME="$scratch/home/.config" HOME="$scratch/home" CAKE_DATA_DIR="$scratch/data" \
  cake trust --check
```

Expected: the first command writes the record; the second exits `0`.

```
printf '\n# changed\n' >> .cake/settings.toml
XDG_CONFIG_HOME="$scratch/home/.config" HOME="$scratch/home" CAKE_DATA_DIR="$scratch/data" \
  cake trust --check
```

Expected: exit `3`, naming `.cake/settings.toml` as modified.

```
XDG_CONFIG_HOME="$scratch/home/.config" HOME="$scratch/home" CAKE_DATA_DIR="$scratch/data" \
  cake trust --revoke
XDG_CONFIG_HOME="$scratch/home/.config" HOME="$scratch/home" CAKE_DATA_DIR="$scratch/data" \
  cake --trust-project "say hello"
```

Expected: `--trust-project` reaches the provider attempt with the fake endpoint rather than failing the trust check, and writes no record. With a real reachable test endpoint, the run would proceed; with the fake endpoint, it fails at the network layer, not the trust layer.

```
XDG_CONFIG_HOME="$scratch/home/.config" HOME="$scratch/home" CAKE_DATA_DIR="$scratch/data" \
  cake --no-project-config "say hello"
```

Expected: the project writable grant is ignored, no trust error occurs, and no project `.cake/system.md` or `.cake/tools` executable is used.

For the side-effect checks, create marker-writing fixtures in the scratch repository. A project hook command that writes `$scratch/hook-ran`, a project gate whose `describe` writes `$scratch/gate-ran`, and a project toolbox executable whose `describe` writes `$scratch/tool-ran` must all leave their markers absent on an untrusted agent run, `cake trust --status`, and `cake debug models`. After `cake trust`, the trusted hook and toolbox markers should appear only when the corresponding feature is actually exercised; the gate marker should appear only when a command is checked or `gates doctor` runs.

For a new-worktree check:

```
git -C "$scratch/repo" add .cake/settings.toml
git -C "$scratch/repo" -c user.name=Test -c user.email=test@example.invalid commit -qm init
git -C "$scratch/repo" worktree add "$scratch/worktree" -q -b trust-check
cd "$scratch/worktree"
XDG_CONFIG_HOME="$scratch/home/.config" HOME="$scratch/home" CAKE_DATA_DIR="$scratch/data" \
  cake trust --status
```

Expected: `untrusted`, because the canonical worktree root has a different project key even though the content matches.

## Validation and Acceptance

The change is complete when a person can verify all of the following without reading the implementation:

- A project that only sets `default_model` to a global entry, selects a low-authority profile, sets `[limits]`, narrows `[tools].enabled`, supplies project `AGENTS.md`, or uses default-root project skills runs without a trust record.
- A project with a sandbox grant, gate configuration, legacy judge table, `system_prompt`, `skills.path`, project hook, or project toolbox executable is reported as untrusted by `cake trust --status` and fails an agent run with exit `3` before any provider credential is read, hook runs, gate starts, toolbox executable is described or executed, writable directory is created, or session file is opened.
- A project `[[models]]` entry fails with exit `3` even after `cake trust`, and the message tells the user to move it to global settings and select it by name.
- `cake trust` writes an owner-only record keyed to the canonical root and prints the reviewed authority; `cake trust --check` exits `0` for `not_required` or `trusted`, and `3` for `untrusted`, `changed`, or `invalid`; `--revoke` removes the record idempotently.
- Any changed, added, removed, or executable-bit-changed file under `.cake/` invalidates the record. A symlinked `.cake`, settings file, hook file, or toolbox entry is rejected, and a symlinked `.cake/system.md` is skipped rather than followed.
- `--trust-project` and `CAKE_TRUST_PROJECT=1` allow the current snapshot for one invocation without writing state; `--no-project-config` and `CAKE_PROJECT_CONFIG=off` ignore automatic project `.cake/` settings, prompts, hooks, and tools; conflicting modes fail closed with exit `3`.
- `cake trust --status --json` and `cake debug models --json` report the trust state, authority sources, and changes on stdout with exit `0`; they do not resolve credentials, start gates, run hooks, describe toolbox tools, or create writable directories.
- A session created or resumed under this change still loads; the session format version remains `4`; the only telemetry change is an optional path-free `project_trust` object.
- `just check`, `just check-coverage`, and `just docs-check` pass. macOS Seatbelt and Linux Landlock CI jobs run the trust tests; the implementation has no network dependency in the trust tests.

## Idempotence and Recovery

`cake trust` is repeatable: it rebuilds the manifest from the current files and atomically replaces the record. If the write fails, the existing record remains. `cake trust --check` and `--status` never write. `--trust-project` and `--no-project-config` never write. `cake trust --revoke` is idempotent and succeeds when no record exists; it removes only a regular record for the canonical root, not a symlink target.

If the trust record is corrupt, has the wrong version, has insecure permissions, or is a symlink, `--status` reports `invalid` and an enforcing run fails closed. Recover with `cake trust --revoke` and `cake trust`. If the trust directory itself is a symlink or cannot be secured, remove or replace it and re-run `cake trust`; Cake must not follow it. If a project `.cake` directory or authority file is a symlink, replace it with a regular file/directory or use `--no-project-config`; Cake does not follow it. If a project file changes during a run after the trust check, the next invocation reports a mismatch; this is the documented residual race, not a record repair path.

If `--worktree` creates a new worktree and the trust check then fails, the existing `WorktreeGuard` drops the worktree when it has no changes, preserving the previous cleanup behavior. If the worktree has changes, it is kept and the user can trust it explicitly. If a test fixture or CI job cannot trust a project, use `--trust-project` for a single invocation or `--no-project-config` for a run that should ignore project `.cake/` entirely; neither is a persistent state change.

## Artifacts and Notes

The trust record is deterministic JSON with this shape:

```
{
  "version": 1,
  "project_root": "/Users/example/project",
  "files": [
    {
      "path": ".cake/settings.toml",
      "sha256": "6f1c...",
      "executable": false
    },
    {
      "path": ".cake/tools/lint",
      "sha256": "a92d...",
      "executable": true
    }
  ]
}
```

An untrusted diagnostic should read like this, with the exact paths and keys from the checkout:

```
Project configuration requires trust before it can be used:
  .cake/settings.toml: [sandbox].writable, [gates.safety], system_prompt
  .cake/hooks.json
  .cake/tools/lint (executable)

Run `cake trust` to review and trust this snapshot.
Or re-run with --trust-project (CAKE_TRUST_PROJECT=1) for this invocation only.
Or re-run with --no-project-config (CAKE_PROJECT_CONFIG=off) to ignore project .cake configuration.
Trust record: /Users/example/.config/cake/trust/6b2e....json
```

A changed diagnostic should read like this:

```
Project configuration changed since it was trusted:
  modified: .cake/settings.toml
  added: .cake/tools/new-check
  removed: .cake/hooks.json

Run `cake trust` again to review the new snapshot.
```

A status JSON document should have the established diagnostic top-level fields and a payload like this:

```
{
  "state": "untrusted",
  "project_root": "/Users/example/project",
  "project_key": "6b2e...",
  "record_path": "/Users/example/.config/cake/trust/6b2e....json",
  "authority": [
    {
      "path": ".cake/settings.toml",
      "kind": "settings",
      "keys": ["[sandbox].writable", "[gates.safety]", "system_prompt"]
    },
    {
      "path": ".cake/hooks.json",
      "kind": "hooks",
      "keys": []
    }
  ],
  "changes": []
}
```

The telemetry init field should look like this and must contain no project path or file contents:

```
{
  "mode": "one_run",
  "project_key": "6b2e...",
  "authority_files": 3
}
```

A successful focused test run should end with the new `trust` tests passing, for example:

```
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The exact count is not an acceptance criterion; the behavior and the absence of ignored trust tests are.

## Interfaces and Dependencies

New module `src/config/trust.rs` exports, at crate visibility, `ProjectConfigMode`, `ProjectConfigSnapshot`, `ProjectTrustReport`, `ProjectAuthority`, `AuthoritySource`, `AuthorityKind`, `TrustState`, `TrustedFile`, `TrustRecord`, `ManifestChange`, `ManifestChangeKind`, `HookFileSource`, `TrustError`, and `ConfigLoadError`. `ProjectConfigSnapshot::load` reads and hashes `.cake/` and parses project settings; it never starts a process. `ProjectTrustReport` is the reusable inspection value for `cake trust --status`, `cake debug models`, and task 077b's future `cake debug config`.

`src/config/settings.rs` adds `SettingsOrigin`, `SettingsSource`, `SettingsLoad`, `SettingsLoader::read_source`, `SettingsLoader::load_with_project_config`, `Settings::without_authority`, `ProjectAuthority::from_settings`, and the new `SettingsError` variants. The production loader returns `SettingsLoad`; the test-only convenience methods preserve the existing unit-test style while using the source-aware merge.

`src/config/hooks.rs` adds `HooksLoader::load_with_project_sources(global_path, project_sources)` and a `HookFileSource` abstraction carrying the validated bytes. The existing `HooksLoader::load_from_paths` remains for global-only and unit-test callers. The hook wire format and command execution semantics do not change.

`src/config/toolbox.rs` adds a project-tools filter used by `src/main.rs`. The existing `toolbox_directories` ordering remains for global and explicit directories; only automatic project `.cake/tools` membership becomes conditional. The describe/execute protocols do not change.

`src/prompts/mod.rs` adds `include_project_prompt: bool` to `resolve_system_prompt` and `build_initial_prompt_messages_with_enabled_tools`; `src/cli/session_factory.rs::RunInputs` carries it. The `--system-prompt` CLI source remains highest precedence and is still a user-supplied path. The automatic `.cake/system.md` source is skipped under `--no-project-config` and for symlinks.

`src/cli/trust.rs` adds `TrustCommand` and its action parsing. `src/cli/mod.rs` adds `Commands::Trust`. `src/cli/debug.rs`, `src/cli/gates.rs`, and `src/cli/bash.rs` receive the effective `ProjectConfigMode` through `CommandRunOptions`. `src/main.rs` passes the trust report to prompt construction, toolbox filtering, telemetry, and diagnostics.

`src/session_telemetry.rs` adds optional `ProjectTrustTelemetry` and `ProjectTrustMode` to `TelemetryInit`. The field is additive and omitted for old or non-project-report fixtures. It contains no absolute path, file name, settings key, credential, or project content.

`src/exit_code.rs` classifies `TrustError` and `ConfigLoadError` as input errors (`3`) and keeps the existing exhaustive settings classification.

Dependencies: no new crate. Use the existing `sha2` for SHA-256, `serde`/`serde_json` for the record and status document, `toml` for settings parsing, `tempfile` for tests, and `std::os::unix::fs::PermissionsExt` under `cfg(unix)`. Tests must not contact a provider or use the user's real `HOME`, `XDG_CONFIG_HOME`, `CAKE_DATA_DIR`, or trust records.

## Revision Notes

- 2026-11-02: Initial draft for task 142 after accepting ADR 053. It defines the trust snapshot as the single parsed project-settings source, global-only model definitions, narrowing-only project tool selection, inspection versus enforcement modes, a deterministic owner-only manifest, the one-run/ignore modes, and the test-fixture migration from project models to global models. No implementation has started.

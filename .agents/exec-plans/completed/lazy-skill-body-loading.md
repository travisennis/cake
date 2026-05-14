# Implement Lazy Skill Body Loading at Activation Time

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan follows `.agents/PLANS.md` and implements task `.agents/.tasks/125.md`.

## Purpose / Big Picture

Cake has a skills system where startup discovers `SKILL.md` files and the model later activates a relevant skill by reading its file. The user benefit of this work is lower startup memory and I/O for large skill files: discovery should read only YAML frontmatter (`name` and `description`), while activation should read the markdown body on demand. The behavior is observable through tests that prove invalid body bytes are not consumed during discovery, and that reading a known skill path activates it exactly once per session.

## Progress

- [x] (2026-05-14T11:53Z) Read task 125, `.agents/TASKS.md`, `.agents/.tasks/index.md`, `.agents/PLANS.md`, repo `AGENTS.md`, skill design docs, and the relevant source in `src/config/skills.rs`, `src/clients/agent.rs`, `src/prompts/mod.rs`, and `src/clients/tools/read.rs`.
- [x] (2026-05-14T11:53Z) Confirmed current discovery uses `std::fs::read_to_string` in `Skill::parse`, so it does read every skill body during discovery.
- [x] (2026-05-14T12:16Z) Updated `Skill::parse` so it reads only the frontmatter section and never reads bytes after the closing frontmatter delimiter.
- [x] (2026-05-14T12:16Z) Made `Skill::load_body` and `SkillCatalog::get_skill_by_location` production APIs by removing their test-only gating and documenting the activation-time loading policy.
- [x] (2026-05-14T12:16Z) Wired known skill `Read` calls through `Skill::load_body` so activation loads the body on demand once per session while preserving the existing active/pending deduplication behavior.
- [x] (2026-05-14T12:16Z) Added focused tests for discovery-without-body-read, activation-time body loading, and duplicate activation caching/deduplication.
- [x] (2026-05-14T12:16Z) Ran focused tests, ran the deslop pass, updated task/index/plan status, and passed final `just ci`.

## Surprises & Discoveries

- Observation: The system prompt already includes only skill metadata and location, not bodies.
  Evidence: `src/prompts/mod.rs` calls `SkillCatalog::to_prompt_xml()`, and `SkillCatalog::to_prompt_xml()` emits `<name>`, `<description>`, and `<location>`.
- Observation: The real eager-read bug is in discovery, not prompt construction.
  Evidence: `src/config/skills.rs::Skill::parse` currently calls `std::fs::read_to_string(path)` before parsing frontmatter.
- Observation: Activation currently uses the generic `Read` tool output for known skill files and relies on `active`/`pending` sets for one activation per session.
  Evidence: `src/clients/agent.rs::execute_tool_with_skill_dedup` canonicalizes the read path, checks `skill_locations`, reserves the skill name, executes `Read`, then marks the skill active.
- Observation: The first frontmatter-only reader draft treated an indented `---` inside a YAML block scalar as a closing delimiter.
  Evidence: The deslop pass identified that `line.trim() == "---"` would close on a line like `  ---`; the final code uses `line.trim_end() == "---"` and has `skill_parse_multiline_description_with_indented_separator_text`.

## Decision Log

- Decision: Keep the model-facing activation trigger as the existing `Read` tool against the skill location.
  Rationale: ADR 002 explicitly rejected a dedicated skill activation tool and chose `Read` for activation; this task can fix lazy body loading without changing the tool surface.
  Date/Author: 2026-05-14 / Codex.
- Decision: Store full `Skill` values in the agent's skill-location map instead of only names.
  Rationale: The activation path needs the production `Skill::load_body` API and persisted `SkillActivated` records still need the name and canonical path. Reusing the parsed `Skill` avoids reparsing metadata or inventing a parallel type.
  Date/Author: 2026-05-14 / Codex.
- Decision: Use the existing per-session `active`/`pending` sets as the cache policy rather than retaining skill body strings.
  Rationale: After a skill body is returned once, it is already in the conversation history. Keeping another copy in memory would work against the memory-reduction goal.
  Date/Author: 2026-05-14 / Codex.
- Decision: Load and return only markdown body content for known skill activations, without YAML frontmatter or generic line numbering.
  Rationale: The frontmatter is already in the skill catalog and the task specifically points to `Skill::load_body`, whose contract is body content after frontmatter. This gives activation the actual instructions while avoiding metadata duplication.
  Date/Author: 2026-05-14 / Codex.

## Outcomes & Retrospective

Completed on 2026-05-14. Discovery now reads only frontmatter, and the new invalid-body-byte test proves body bytes are not decoded during discovery. Known skill `Read` calls now load markdown body content through `Skill::load_body`, while the existing per-session active/pending state prevents duplicate body loads. The skill design doc was updated to state that known skill activation returns body content after frontmatter, not the full frontmatter-plus-body file.

## Context and Orientation

Cake is a Rust 2024 binary CLI. Skills are instruction bundles stored as `SKILL.md` files. Each skill file begins with YAML frontmatter between `---` delimiters and then markdown body content. The frontmatter contains the `name` and `description` fields shown to the model at startup; the body contains the full instructions and should be read only if the model activates the skill.

The relevant files are `src/config/skills.rs`, `src/prompts/mod.rs`, `src/clients/agent.rs`, `src/main.rs`, `docs/adr/002-agent-skills.md`, and `docs/design-docs/skills.md`.

## Plan of Work

First, change `Skill::parse` in `src/config/skills.rs` to use a frontmatter-only reader. The helper should open the file, read line by line until it has found the opening `---` and the closing `---`, and then parse only the YAML text between them. It should report the same user-facing diagnostics for unreadable files, missing frontmatter, unclosed frontmatter, and missing required fields. A body containing invalid UTF-8 after a valid closing delimiter should not make discovery fail, proving the body was not decoded.

Second, remove `#[cfg(test)]` from `Skill::load_body` and `SkillCatalog::get_skill_by_location`. Update the `load_body` doc comment to state that activation uses it to read markdown body content from disk and that per-session deduplication lives in `Agent`, not in `Skill`.

Third, update `src/clients/agent.rs` so `skill_locations` maps canonical `SKILL.md` paths to cloned `Skill` values. `execute_tool_with_skill_dedup` should still only intercept the `Read` tool. For non-skill paths, invalid paths, and malformed read arguments it should fall back to the normal tool execution. For known skill paths, it should reserve by skill name, call `skill.load_body()`, mark the skill complete, and return the body text. If `load_body` fails, it should clear the pending reservation and return an error string. Active and pending duplicate behavior should remain unchanged.

Fourth, update `src/main.rs` so `CodingAssistant::skill_locations` builds `HashMap<PathBuf, Skill>` with canonical skill paths if possible. Keeping canonical keys matches the activation path, which canonicalizes user-provided `Read` paths before lookup.

Fifth, add tests. In `src/config/skills.rs`, add a test where the body after valid frontmatter contains invalid UTF-8 bytes and assert `Skill::parse` succeeds. In `src/clients/agent.rs`, update existing skill activation tests to use real `Skill` values and assert the returned output contains body content but not frontmatter. Keep or adapt the concurrent duplicate test to prove only one activation returns the body while the other returns the pending message.

Finally, run focused tests, perform the deslop review pass, run `just ci`, update `.agents/.tasks/125.md`, `.agents/.tasks/index.md`, and this plan with the final outcome, then commit with a Conventional Commit message.

## Concrete Steps

From `/Users/travisennis/Projects/cake`, inspect the relevant code with:

    sed -n '1,320p' src/config/skills.rs
    sed -n '500,760p' src/clients/agent.rs
    sed -n '500,680p' src/main.rs

After implementation, run focused tests:

    cargo test config::skills
    cargo test skill_read
    cargo test active_skill_read_does_not_reload_body

Then run the repository's full check:

    just ci

Focused validation completed successfully before final CI:

    cargo test config::skills
    cargo test skill_read
    cargo test active_skill_read_does_not_reload_body

Final validation completed successfully:

    just ci

The final CI run reported 575 unit tests, 12 exit-code integration tests, and 10 stdin integration tests passing, plus rustfmt, clippy with `-D warnings`, toolchain pin verification, and import lint.

## Validation and Acceptance

The task is accepted when discovery no longer reads the skill body, activation of a known skill path reads the body exactly once per session, and `load_body` plus `get_skill_by_location` are production APIs without `#[cfg(test)]`.

The most important proof is a test where `Skill::parse` succeeds even though the bytes after frontmatter are invalid UTF-8. That test would fail when discovery used `std::fs::read_to_string` on the whole file. The activation proof is a test where `execute_tool_with_skill_dedup` receives a `Read` request for a known skill location, returns body instructions, records a `SkillActivation`, and suppresses a duplicate concurrent read.

## Idempotence and Recovery

The edits are local and safe to rerun. If a focused test fails after the parser change, inspect whether the failure is in frontmatter delimiter compatibility before changing activation behavior. If the activation test fails, verify that `src/main.rs::skill_locations` and the test both use canonical paths, because `execute_tool_with_skill_dedup` canonicalizes read arguments before lookup.

No destructive migration or user data rewrite is involved. Existing sessions that contain `SkillActivated` records continue to restore activated skill names through `Session::activated_skills()`.

## Artifacts and Notes

Current behavior evidence before code changes:

    src/config/skills.rs::Skill::parse reads the whole file with std::fs::read_to_string(path).
    src/clients/agent.rs::execute_tool_with_skill_dedup stores HashMap<PathBuf, String> and calls the generic Read tool for first activation.

## Interfaces and Dependencies

At the end of this plan, these interfaces should exist:

- `src/config/skills.rs::Skill::parse(path: &Path, scope: SkillScope) -> Result<Skill, SkillDiagnostic>` reads only frontmatter.
- `src/config/skills.rs::Skill::load_body(&self) -> Result<String, std::io::Error>` is available in production and returns markdown body content after frontmatter.
- `src/config/skills.rs::SkillCatalog::get_skill_by_location(&self, path: &Path) -> Option<&Skill>` is available in production.
- `src/clients/agent.rs::Agent::with_skill_locations` accepts the skill-location map needed for activation deduplication.
- `src/clients/agent.rs::execute_tool_with_skill_dedup` activates known skills through `Skill::load_body` and leaves non-skill reads on the generic `Read` path.

Revision note, 2026-05-14 / Codex: Created the plan after reading task 125 and the relevant source. The plan records that the prompt is already metadata-only and that the concrete defect is discovery-time full-file reads plus an unused production body-loading API.

Revision note, 2026-05-14 / Codex: Completed the implementation, recorded the deslop delimiter finding, updated validation evidence, and moved the plan to completed.

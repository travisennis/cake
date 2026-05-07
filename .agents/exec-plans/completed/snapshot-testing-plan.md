# Add snapshot testing to improve test coverage and catch regressions

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document must be maintained in accordance with `.agents/PLANS.md`.

## Purpose / Big Picture

After this change, cake will have deterministic snapshot tests covering the system prompt, the skill catalog XML embedded in that prompt, the Chat Completions message translation layer including reasoning placeholder injection, the Responses API serialization helpers (`to_api_input` and `to_streaming_json`), and one request-payload snapshot for each provider-specific request builder. A developer who changes prompt wording, skill catalog formatting, `ConversationItem` to `ChatMessage` grouping, placeholder injection, or the final JSON request shape sent to an API provider will see a focused snapshot diff that shows exactly what changed.

The observable outcome is simple. Running `cargo test` will produce snapshot failures with `.snap.new` files when output changes locally, alongside the existing assertion-based tests. Running `cargo insta review` is the preferred way to inspect and accept intentional changes, but `INSTA_UPDATE=always cargo test` remains a non-interactive fallback when `cargo-insta` is not installed. The tests will pass when output is unchanged and fail with a clear diff when output changes.

## Progress

- [x] (2026-04-28) Created the ExecPlan from codebase investigation into snapshot testing opportunities.
- [x] (2026-04-30) Tightened the plan after design review: made prompt snapshots deterministic, expanded scope to include `to_streaming_json` and request-boundary snapshots, and simplified the snapshot acceptance workflow.
- [x] (2026-04-30) Add `insta` to dev-dependencies with only the features required by the planned assertions.
- [x] (2026-04-30) Add shared snapshot helpers for prompt normalization and explicit snapshot naming.
- [x] (2026-04-30) Implement system prompt and `SkillCatalog::to_prompt_xml` snapshot tests.
- [x] (2026-04-30) Implement Chat Completions `build_messages` and placeholder-injection snapshot tests, plus at least one full `ChatRequest` snapshot.
- [x] (2026-04-30) Implement Responses API `to_api_input`, `to_streaming_json`, and full `Request` snapshot tests.
- [x] (2026-04-30) Run `just ci` and verify all snapshot tests pass with accepted, stable snapshots.

## Surprises & Discoveries

- Observation: `build_system_prompt` embeds the current local date directly into the returned string, so naive string snapshots will fail every day even when the prompt template is unchanged.
  Evidence: `src/prompts/mod.rs` appends `Local::now().format("%Y-%m-%d")` near the end of `build_system_prompt`.

- Observation: The codebase already depends on `similar` (the diffing component inside `insta`) for the Edit tool's unified diff output, so `insta` will not introduce an unfamiliar dependency at the algorithmic level.
  Evidence: `Cargo.toml` lists `similar = { version = "3.1.0", features = ["inline"] }` and `src/clients/tools/edit.rs` calls `similar::TextDiff::from_lines`.

- Observation: The project has substantial test coverage (roughly 300+ test functions across 20 files) but zero existing snapshot tests. All string and JSON outputs are verified with substring assertions or individual field equality checks.
  Evidence: Running `rg "insta|snapshot" src/ Cargo.toml` returns no matches.

- Observation: Helper-level serialization snapshots alone would still miss the final provider payload shape because the request builders add omit-if-none behavior, tool selection fields, provider filters, and reasoning options after the helper output is produced.
  Evidence: `src/clients/chat_completions.rs` builds `ChatRequest` before calling `.json(&request)`, and `src/clients/responses.rs` builds `Request` before calling `.json(&prompt)`.

- Observation: `serde_json::Value` snapshots are a good fit for `types.rs`, but `to_api_input` is only one of two serialization surfaces in that file. `to_streaming_json` also has dedicated behavior and existing tests.
  Evidence: `src/clients/types.rs` defines `ConversationItem::to_api_input` and `ConversationItem::to_streaming_json`, with separate tests for both.

- Observation: The system prompt test in `src/prompts/mod.rs` only checks for substring presence. A snapshot of the full prompt would catch unintended changes to the prompt template, skill XML, or AGENTS.md formatting that substring checks would miss.
  Evidence: `src/prompts/mod.rs` tests use patterns like `assert!(prompt.contains("## Additional Context:"))` which would not catch a misspelling of `Context` or a reordering of sections.

- Observation: `build_messages` in `chat_completions.rs` has extensive field-level tests for grouping, reasoning propagation, and edge cases, but none verify the complete serialized output shape.
  Evidence: The tests in `src/clients/chat_completions.rs` check individual fields like `msgs[0].role == "user"` and `msgs[1].tool_calls.unwrap().len() == 2` but never serialize the full message vector.

- Observation: In `insta` 1.47.2, `with_settings!` date filters are gated behind the `filters` feature rather than the `json` feature.
  Evidence: Compiling the prompt snapshot helper with only `features = ["json"]` failed with `no method named filters`; the local crate metadata lists `filters = ["regex"]`.

## Decision Log

- Decision: Add `insta` as a dev-dependency rather than building custom snapshot infrastructure.
  Rationale: `insta` is the standard Rust snapshot testing crate. It provides `assert_snapshot!`, `assert_json_snapshot!`, and `cargo insta review` out of the box. Building custom infrastructure would duplicate this work with no benefit. The crate is lightweight and shares the `similar` diffing dependency already present in the tree. The initial dependency should enable only the `json` feature because this plan does not use YAML snapshots.
  Date/Author: 2026-04-28 / Amp

- Decision: Make prompt snapshots deterministic by normalizing the date line through a shared `insta::with_settings!` filter rather than weakening the assertions.
  Rationale: The prompt includes the current date by design, so a raw snapshot would fail daily. Redacting only the date preserves a full-string snapshot of the prompt structure while removing the one known source of volatility. Putting this in a helper avoids repeating the filter in every prompt test and keeps the acceptance workflow predictable.
  Date/Author: 2026-04-30 / Amp

- Decision: Start with the highest-value string and JSON surfaces plus one request-boundary snapshot per provider rather than attempting to snapshot every output in the repository.
  Rationale: The strongest early wins are the system prompt, skill catalog XML, `build_messages`, `to_api_input`, `to_streaming_json`, and the final request payloads assembled in `chat_completions.rs` and `responses.rs`. Together they cover prompt construction, intermediate translation, and the exact JSON sent to external APIs without expanding into lower-value snapshot territory such as every diff or sandbox profile immediately.
  Date/Author: 2026-04-30 / Amp

- Decision: Do not replace existing assertion tests. Add snapshot tests alongside them.
  Rationale: The existing tests document intent (for example, `reasoning content is preserved for assistant messages`). Removing them would lose that documentation value. The snapshot tests complement them by verifying the complete output shape. An assertion test that says `reasoning content appears on the assistant message` and a snapshot that shows the exact JSON are both valuable.
  Date/Author: 2026-04-28 / Amp

- Decision: Use `insta::assert_json_snapshot!` for JSON outputs and `insta::assert_snapshot!` for plain text.
  Rationale: `assert_json_snapshot!` pretty-prints the JSON and normalizes the structure so the diff is readable. `assert_snapshot!` works for strings like the system prompt and skill catalog XML. Both macros produce `.snap` files that `cargo insta review` can manage.
  Date/Author: 2026-04-28 / Amp

- Decision: Use explicit snapshot names for the new assertions instead of relying on test-function-derived names.
  Rationale: The planned suite spans several modules and some tests may intentionally emit more than one snapshot over time. Explicit names keep snapshot filenames stable when tests are renamed, make code review easier, and avoid accidental churn if the test structure changes later.
  Date/Author: 2026-04-30 / Amp

- Decision: Keep `cargo-insta` optional local tooling instead of making it a mandatory setup step.
  Rationale: Snapshot tests should run under plain `cargo test` with no extra installation. `cargo insta review` is still the preferred review UI, but the plan must remain executable for a novice who only has Cargo and the checked-out repository. `INSTA_UPDATE=always cargo test` is an acceptable fallback when intentional updates need to be accepted non-interactively.
  Date/Author: 2026-04-30 / Amp

- Decision: Keep snapshot files in `src/**/snapshots/` using `insta`'s default behavior rather than a custom snapshot directory.
  Rationale: `insta` creates `snapshots/` directories adjacent to test modules automatically. This keeps snapshots co-located with the code they verify, making them easy to find during code review.
  Date/Author: 2026-04-28 / Amp

## Outcomes & Retrospective

Implemented the first snapshot testing pass with `insta` accepted snapshots for prompt construction, skill catalog XML, Chat Completions message/request serialization, Responses API helper serialization, and Responses request payloads.

`just ci` passes after the change. The final implementation uses `insta = { version = "1", features = ["filters", "json"] }` because prompt date normalization needs `filters`; this is still the narrow required feature set for the planned assertions.

## Context and Orientation

`cake` is a Rust CLI for AI-assisted coding. It sends conversation history to LLM providers via two API formats (Responses API and Chat Completions), executes tools, and manages sessions.

The codebase already has substantial test coverage: roughly 300+ test functions across 20 source files, plus 2 integration test files. The tests use standard Rust assertion macros (`assert_eq!`, `assert!`, `matches!`) and check individual fields or substring presence.

Snapshot testing is a technique where a test records the complete output of a function into a file (a `snapshot`) and future test runs compare the output against that stored file. If output matches, the test passes. If output differs, the test fails and shows a diff. The developer then either fixes the code (if the change is a bug) or accepts the new output (if the change is intentional). This is useful for functions that produce complex strings, JSON, or other structured text where writing field-by-field assertions is tedious and error-prone.

The `insta` crate is the standard Rust snapshot testing library. It provides:

- `insta::assert_snapshot!(value)` for plain text strings
- `insta::assert_json_snapshot!(value)` for JSON, which pretty-prints and normalizes
- `cargo insta review` to interactively accept or reject snapshot changes
- Automatic `.snap` file management in `snapshots/` directories next to test modules

The primary targets for this first snapshot pass are listed below.

`src/prompts/mod.rs` contains `build_system_prompt(working_dir, agents_files, skill_catalog) -> String`. This function constructs a multi-paragraph system prompt that includes base instructions, optional AGENTS.md content from multiple files, an optional XML skill catalog, the current working directory, and today's date. The current tests use substring presence checks. The prompt structure is sensitive to changes in formatting, skill catalog XML generation, and AGENTS.md section construction.

`src/config/skills.rs` contains `SkillCatalog::to_prompt_xml(&self) -> String`. This method generates the XML catalog block that is injected into the system prompt. It handles XML escaping of special characters and produces a structured `<available_skills>` block. A localized snapshot here will keep XML-specific regressions easy to review instead of forcing every skill-related diff to be inspected only inside the larger prompt snapshot.

`src/clients/chat_completions.rs` contains `build_messages(history: &[ConversationItem]) -> Vec<ChatMessage>`. This function translates the internal `ConversationItem` representation into the Chat Completions API wire format. It handles complex grouping: consecutive `FunctionCall` items are merged into a single assistant message with multiple `tool_calls`, `Reasoning` items are propagated to the following assistant message's `reasoning_content` field, and empty or missing fields have specific serialization behaviors. The current tests verify individual fields on the resulting `Vec<ChatMessage>` but never serialize the full output to JSON.

`src/clients/chat_types.rs` defines `ChatRequest` and `ChatMessage`, the serializable request data transfer objects sent to the Chat Completions API. `src/clients/chat_completions.rs` assembles a `ChatRequest` in `send_request` immediately before it is serialized with `reqwest::RequestBuilder::json`. Snapshotting one complete `ChatRequest` will verify the exact omit-if-none and tool-related payload shape at the API boundary, not just the intermediate `build_messages` output.

`src/clients/types.rs` contains `ConversationItem::to_api_input(&self) -> serde_json::Value` and `ConversationItem::to_streaming_json(&self) -> serde_json::Value`. These methods serialize each conversation item variant (Message, FunctionCall, FunctionCallOutput, Reasoning) to the JSON formats used for the Responses API request payload and the stream-json or session-record output. The current tests check individual JSON fields with scattered `assert_eq!` calls for both helpers.

`src/clients/responses.rs` assembles the exact `Request` payload sent to the Responses API. It collects `ConversationItem::to_api_input()` output into `input`, then adds provider filters, reasoning configuration, tool lists, and request overrides before serializing with `.json(&prompt)`. Snapshotting one or two complete `Request` values here will catch interactions that helper-level snapshots cannot see.

The repository also has other string-heavy candidates for later snapshot adoption, such as settings merges, session JSONL, unified diff output, and macOS sandbox profiles. Those are intentionally left out of this first plan so the initial adoption stays high-value and easy to review.

## Plan of Work

The first milestone adds `insta` to the project with only the `json` feature enabled. There is no need for a throwaway smoke test. The first real snapshot assertions added in later milestones will prove the dependency is wired correctly.

The second milestone adds a small snapshot helper in `src/prompts/mod.rs` that normalizes the `Today's date:` line with `insta::with_settings!` before asserting. With that helper in place, the milestone adds prompt snapshots for several configurations: empty AGENTS files, one project AGENTS file, both user and project AGENTS files, a populated skill catalog, and the combination of AGENTS files with skills. This milestone also adds one localized snapshot for `SkillCatalog::to_prompt_xml` in `src/config/skills.rs` so that XML-only changes stay easy to review.

The third milestone adds snapshot tests for `build_messages` in `src/clients/chat_completions.rs`. These snapshots should cover the full serialized JSON output of the `Vec<ChatMessage>` for the edge cases already covered by the existing tests: simple conversation, grouped function calls, reasoning propagation to assistant text, reasoning propagation to tool calls, combined tool calls with assistant text, empty history, and the reasoning placeholder injection path. The same milestone also adds a snapshot of one complete `ChatRequest` value so the final payload shape at the API boundary is locked down.

The fourth milestone adds snapshot tests for both `ConversationItem::to_api_input` and `ConversationItem::to_streaming_json` in `src/clients/types.rs`. One snapshot test per important variant or variant-shape difference will capture the complete `serde_json::Value` output while the existing assertion tests continue to document the intended semantics.

The fifth milestone adds one or two Responses API request snapshots in `src/clients/responses.rs`. These will serialize the fully assembled `Request` value after helper output, provider filters, reasoning configuration, tool lists, and overrides have all been applied.

The final milestone runs `just ci` to confirm formatting, linting, and all tests pass. The snapshot files will be committed to the repository.

## Concrete Steps

All commands run from the repository root at `/Users/travisennis/Projects/cake`.

### Milestone 1: Add `insta` dependency

Add `insta` to the `[dev-dependencies]` section of `Cargo.toml`:

    insta = { version = "1", features = ["json"] }

Do not add a temporary smoke-test file. The first real snapshot assertions added below are sufficient proof that the dependency compiles and works.

    cargo test

If the machine already has `cargo-insta`, it can be used later for interactive review. Do not treat that installation as a required repository step.

### Milestone 2: System prompt and skill catalog snapshots

In `src/prompts/mod.rs`, inside the existing `#[cfg(test)] mod tests` block, add a helper that normalizes the date line before asserting a snapshot. Use explicit snapshot names so the generated snapshot filenames remain stable even if test names change later.

    fn assert_prompt_snapshot(name: &str, prompt: String) {
        insta::with_settings!({
            filters => vec![(r"Today's date: \d{4}-\d{2}-\d{2}", "Today's date: [DATE]")]
        }, {
            insta::assert_snapshot!(name, prompt);
        });
    }

Then add snapshot tests that capture the complete output of `build_system_prompt`. Add these tests after the existing ones.

For the test `snapshot_empty_prompt`, call `build_system_prompt` with an empty agents file list and empty skill catalog, then snapshot the result:

    let prompt = build_system_prompt(Path::new("/tmp"), &[], &SkillCatalog::empty());
    assert_prompt_snapshot("prompt_empty", prompt);

For the test `snapshot_with_project_agents`, call with one project-level AGENTS.md file:

    let files = vec![AgentsFile {
        path: "./AGENTS.md".to_string(),
        content: "You are a Rust expert. Follow all project conventions.".to_string(),
    }];
    let prompt = build_system_prompt(Path::new("/project"), &files, &SkillCatalog::empty());
    assert_prompt_snapshot("prompt_with_project_agents", prompt);

For the test `snapshot_with_user_and_project_agents`, call with two files:

    let files = vec![
        AgentsFile {
            path: "~/.cake/AGENTS.md".to_string(),
            content: "User-level global instructions.".to_string(),
        },
        AgentsFile {
            path: "./AGENTS.md".to_string(),
            content: "Project-level overrides.".to_string(),
        },
    ];
    let prompt = build_system_prompt(Path::new("/project"), &files, &SkillCatalog::empty());
    assert_prompt_snapshot("prompt_with_user_and_project_agents", prompt);

For the test `snapshot_with_skill_catalog`, create a catalog with one skill:

    let mut catalog = SkillCatalog::empty();
    catalog.skills.push(Skill {
        name: "debugging".to_string(),
        description: "How to debug Rust programs".to_string(),
        location: PathBuf::from("/project/.agents/skills/debugging/SKILL.md"),
        base_directory: PathBuf::from("/project/.agents/skills/debugging"),
        scope: SkillScope::Project,
    });
    let prompt = build_system_prompt(Path::new("/project"), &[], &catalog);
    assert_prompt_snapshot("prompt_with_skill_catalog", prompt);

For the test `snapshot_with_agents_and_skills`, combine AGENTS files with a skill catalog and assert it with the name `prompt_with_agents_and_skills`.

In `src/config/skills.rs`, add a localized snapshot test for `SkillCatalog::to_prompt_xml` using an explicit snapshot name such as `skill_catalog_single_skill_xml`. Keep the existing assertion tests that verify XML escaping and basic structure.

After adding these tests, accept the initial snapshots:

    cargo test prompts::tests
    cargo test config::skills::tests

If `cargo-insta` is installed, run:

    cargo insta review

If it is not installed, accept the snapshots non-interactively with:

    INSTA_UPDATE=always cargo test prompts::tests config::skills::tests

### Milestone 3: Chat Completions build_messages snapshot tests

In `src/clients/chat_completions.rs`, inside the existing `#[cfg(test)] mod tests` block, add snapshot tests for `build_messages`. Serialize the output `Vec<ChatMessage>` to JSON before snapshotting so the tests cover omit-if-none behavior and field shape in addition to logical correctness. Use explicit snapshot names.

For the test `snapshot_simple_conversation`, reuse the same history from the existing `build_messages_simple_conversation` test but snapshot the serialized output:

    let history = vec![
        ConversationItem::Message {
            role: Role::System,
            content: "You are helpful.".to_string(),
            id: None, status: None, timestamp: None,
        },
        ConversationItem::Message {
            role: Role::User,
            content: "Hello".to_string(),
            id: None, status: None, timestamp: None,
        },
    ];
    let msgs = build_messages(&history);
    insta::assert_json_snapshot!("build_messages_simple_conversation", msgs);

For the test `snapshot_grouped_function_calls`, reuse the history from `build_messages_groups_consecutive_function_calls`:

    // Same history as the existing test
    let msgs = build_messages(&history);
    insta::assert_json_snapshot!("build_messages_grouped_function_calls", msgs);

For the test `snapshot_reasoning_with_assistant_text`, reuse the history from `build_messages_preserves_reasoning_content_for_assistant_messages` and snapshot it with the name `build_messages_reasoning_with_assistant_text`.

For the test `snapshot_reasoning_with_tool_calls`, reuse the history from `build_messages_preserves_reasoning_content_for_assistant_tool_calls`:

    let msgs = build_messages(&history);
    insta::assert_json_snapshot!("build_messages_reasoning_with_tool_calls", msgs);

For the test `snapshot_assistant_text_with_tool_calls`, reuse the history from `build_messages_combines_tool_calls_with_assistant_text` and snapshot it with the name `build_messages_assistant_text_with_tool_calls`.

For the test `snapshot_empty_history`:

    let msgs = build_messages(&[]);
    insta::assert_json_snapshot!("build_messages_empty_history", msgs);

For the placeholder path, reuse the history from `inject_reasoning_placeholders_adds_placeholder_for_tool_calls_without_reasoning`, run `build_messages`, then call `inject_reasoning_placeholders(&mut msgs)` and snapshot the resulting vector with the name `build_messages_with_reasoning_placeholder`.

After the helper-output snapshots, add one full request-boundary snapshot in the same module. Construct a `ChatRequest` directly inside the test using a Kimi model string, a history that yields tool calls without reasoning content, and a non-empty tool list. Apply the same placeholder logic used in `send_request`, then snapshot `serde_json::to_value(&request)` with a name such as `chat_request_kimi_tool_calls`.

Accept the snapshots:

    cargo test chat_completions::tests

Then review with `cargo insta review`, or accept with `INSTA_UPDATE=always cargo test chat_completions::tests` if `cargo-insta` is unavailable.

### Milestone 4: Responses API helper serialization snapshots

In `src/clients/types.rs`, inside the existing `#[cfg(test)] mod tests` block, add snapshot tests for both `ConversationItem::to_api_input` and `ConversationItem::to_streaming_json`. Each test creates a variant, calls the serialization helper, and snapshots the resulting `serde_json::Value`. Use explicit snapshot names.

For the test `snapshot_user_message`:

    let item = ConversationItem::Message {
        role: Role::User,
        content: "Hello".to_string(),
        id: None, status: None, timestamp: None,
    };
    insta::assert_json_snapshot!("to_api_input_user_message", item.to_api_input());

For the test `snapshot_assistant_message_with_id_and_status`:

    let item = ConversationItem::Message {
        role: Role::Assistant,
        content: "Hi there".to_string(),
        id: Some("msg-1".to_string()),
        status: Some("completed".to_string()),
        timestamp: None,
    };
    insta::assert_json_snapshot!(
        "to_api_input_assistant_message_with_id_and_status",
        item.to_api_input()
    );

For the test `snapshot_system_message`:

    let item = ConversationItem::Message {
        role: Role::System,
        content: "You are cake".to_string(),
        id: None, status: None, timestamp: None,
    };
    insta::assert_json_snapshot!("to_api_input_system_message", item.to_api_input());

For the test `snapshot_function_call`:

    let item = ConversationItem::FunctionCall {
        id: "fc-1".to_string(),
        call_id: "call-1".to_string(),
        name: "bash".to_string(),
        arguments: r#"{"cmd":"ls"}"#.to_string(),
        timestamp: None,
    };
    insta::assert_json_snapshot!("to_api_input_function_call", item.to_api_input());

For the test `snapshot_function_call_output`:

    let item = ConversationItem::FunctionCallOutput {
        call_id: "call-1".to_string(),
        output: "file.txt\nother.txt".to_string(),
        timestamp: None,
    };
    insta::assert_json_snapshot!("to_api_input_function_call_output", item.to_api_input());

For the test `snapshot_reasoning_with_summary`:

    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: vec!["thinking...".to_string()],
        encrypted_content: None,
        content: None,
        timestamp: None,
    };
    insta::assert_json_snapshot!("to_api_input_reasoning_with_summary", item.to_api_input());

For the test `snapshot_reasoning_with_encrypted_content`:

    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: vec!["thinking...".to_string()],
        encrypted_content: Some("gAAAAABencrypted...".to_string()),
        content: None,
        timestamp: None,
    };
    insta::assert_json_snapshot!(
        "to_api_input_reasoning_with_encrypted_content",
        item.to_api_input()
    );

For the test `snapshot_reasoning_with_content_array`:

    let item = ConversationItem::Reasoning {
        id: "r-1".to_string(),
        summary: vec!["thinking...".to_string()],
        encrypted_content: None,
        content: Some(vec![ReasoningContent {
            content_type: "reasoning_text".to_string(),
            text: Some("deep analysis".to_string()),
        }]),
        timestamp: None,
    };
    insta::assert_json_snapshot!("to_api_input_reasoning_with_content_array", item.to_api_input());

Add matching `to_streaming_json` snapshots for the variants whose streaming format materially differs from `to_api_input`: a message with `id` and `status`, a reasoning item with plain-string summary output, a function call, and a function call output. Use names such as `to_streaming_json_message_with_id_and_status` and `to_streaming_json_reasoning_plain_summary`.

Keep the existing field-level assertions in this module. The snapshots are complementary and should not replace those intent-focused tests.

Accept the snapshots:

    cargo test types::tests

Then review with `cargo insta review`, or accept with `INSTA_UPDATE=always cargo test types::tests` if needed.

### Milestone 5: Responses API request payload snapshots

In `src/clients/responses.rs`, add one or two request-payload snapshot tests that serialize the fully assembled `Request` value before it is handed to `reqwest`. Build the `input` array from `history.iter().map(ConversationItem::to_api_input)`, then populate the same `provider`, `reasoning`, `tools`, `tool_choice`, and override fields used in `send_request`.

The first request snapshot should be a minimal baseline request with no tools, no provider filter, and no reasoning configuration. Snapshot it with a name such as `responses_request_minimal`.

The second request snapshot should exercise the interesting omit-if-none and override behavior: include tools, a provider filter, reasoning configuration, and an override for `max_output_tokens`. Snapshot it with a name such as `responses_request_with_tools_provider_and_reasoning`.

Accept the snapshots:

    cargo test responses::tests

Then review with `cargo insta review`, or accept with `INSTA_UPDATE=always cargo test responses::tests` if needed.

### Milestone 6: Final verification

Run the full CI check:

    just ci

The expected result is that formatting, clippy, and all tests (including snapshot tests) pass with no errors. The snapshot files will be generated in `src/prompts/snapshots/`, `src/config/snapshots/`, and `src/clients/snapshots/` (or similar paths as determined by `insta`).

## Validation and Acceptance

The implementation is acceptable when the following behavior is demonstrable.

Running `cargo test` from the repository root passes all tests, including the new snapshot tests. The snapshot files exist in the repository tree under `snapshots/` directories adjacent to the test modules.

Running `cargo test` locally after an intentional snapshot-affecting change produces `.snap.new` files and a readable diff. Running `cargo insta review` accepts or rejects those changes. If `cargo-insta` is not available, `INSTA_UPDATE=always cargo test` updates the snapshots non-interactively.

Changing the wording of the system prompt in `src/prompts/mod.rs` (for example, changing `You are cake` to `You are cupcake`) causes `cargo test` to fail the corresponding snapshot test with a diff showing the changed text. Running `cargo insta review` allows accepting the new output.

Changing the skill catalog XML formatting in `src/config/skills.rs` causes the localized XML snapshot to fail even if the larger system prompt test is not the one a developer inspects first.

Changing the Kimi reasoning placeholder logic in `src/clients/chat_completions.rs` causes either the `build_messages_with_reasoning_placeholder` snapshot or the full `chat_request_kimi_tool_calls` snapshot to fail with a diff showing the missing or changed `reasoning_content` field.

Adding a new serialized field to `ConversationItem::Message` or `ConversationItem::Reasoning` causes the relevant `to_api_input`, `to_streaming_json`, or full Responses `Request` snapshot tests to fail with a diff showing the new field. This catches unintended API surface changes.

Changing provider-filter, reasoning, or tool-choice request wiring in `src/clients/responses.rs` or `src/clients/chat_completions.rs` causes the corresponding full request snapshot to fail, demonstrating that the final API-boundary payload is protected and not only the helper output.

The existing assertion tests continue to pass alongside the new snapshot tests. No existing test is removed or modified.

## Idempotence and Recovery

All steps are additive. The new tests and snapshots can be added and accepted incrementally. If a review is interrupted, delete only the generated `.snap.new` files and rerun the affected test command. Do not delete committed `.snap` files unless intentionally resetting the snapshot baseline.

The `insta` crate supports environment variable control: setting `INSTA_UPDATE=new` will write new snapshots without overwriting accepted ones. Setting `INSTA_UPDATE=always` will overwrite existing snapshots. For development, the default (no auto-update) is preferred.

The prompt snapshots are safe to rerun because the one volatile field, the date line, is normalized before assertion. The request and JSON helper snapshots use fixed test data and should not require redaction.

If CI ever needs stricter snapshot hygiene later, `cargo insta test --unreferenced=auto` can be added as a follow-up improvement. That is intentionally out of scope for this first adoption pass.

## Artifacts and Notes

After Milestone 1, `Cargo.toml` gains one line in `[dev-dependencies]`:

    insta = { version = "1", features = ["filters", "json"] }

After Milestone 2, `src/prompts/mod.rs` gains approximately 5 snapshot tests and `src/config/skills.rs` gains 1 localized XML snapshot test. New files appear at `src/prompts/snapshots/` and `src/config/snapshots/` containing the expected prompt and XML text. A sample prompt snapshot file might look like:

```
---
source: src/prompts/mod.rs
expression: prompt
---
You are cake. You are running as a coding agent in a CLI on the user's computer.

## Additional Context:

Project and user instructions are shown below. Be sure to adhere to these instructions. IMPORTANT: These instructions OVERRIDE any default behavior and you MUST follow them exactly as written.

### ./AGENTS.md

<instructions>
You are a Rust expert. Follow all project conventions.
</instructions>

Current working directory: /project
Today's date: [DATE]
```

The prompt helper should normalize the date line with `insta::with_settings!` and a filter:

    insta::with_settings!({
        filters => vec![(r"Today's date: \d{4}-\d{2}-\d{2}", "Today's date: [DATE]")]
    }, {
        insta::assert_snapshot!("prompt_name", prompt);
    });

After Milestone 3, `src/clients/chat_completions.rs` gains approximately 7 helper-output snapshots and 1 request-payload snapshot. New files appear at `src/clients/snapshots/` containing JSON representations of `Vec<ChatMessage>` and `ChatRequest`. The JSON snapshots will be pretty-printed by `insta`.

After Milestone 4, `src/clients/types.rs` gains approximately 12 snapshot tests covering both `to_api_input` and `to_streaming_json`. New files appear at `src/clients/snapshots/` alongside the chat completions snapshots.

After Milestone 5, `src/clients/responses.rs` gains 1 or 2 request-payload snapshots. These live in the same `src/clients/snapshots/` directory with distinct explicit snapshot names.

## Interfaces and Dependencies

The `insta` crate version `1` with the `json` feature enabled. The `json` feature provides `insta::assert_json_snapshot!` which pretty-prints JSON, normalizes whitespace, and provides readable diffs. No additional feature flags are required for this first pass.

No changes to existing public interfaces are required. All changes are confined to `#[cfg(test)]` modules.

The `insta::with_settings!` macro will be imported in test modules that need date normalization:

    use insta::with_settings;

The `insta::assert_snapshot!` macro will be used for plain text snapshots (system prompt and localized skill catalog XML). The `insta::assert_json_snapshot!` macro will be used for JSON snapshots (build_messages output, `to_api_input` output, `to_streaming_json` output, and full request payloads).

Revision note (2026-04-30): Tightened the plan after design review. This revision makes prompt determinism explicit, adds localized skill XML coverage, expands scope to the second serialization surface in `src/clients/types.rs`, adds provider request-boundary snapshots, and removes speculative or unnecessary workflow guidance so the plan is easier for a novice to execute correctly.

Revision note (2026-05-07): Moved this completed ExecPlan from `.agents/.plans/` to `.agents/exec-plans/completed/` during the ExecPlan directory migration.

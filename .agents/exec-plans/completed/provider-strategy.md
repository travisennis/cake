# Move Provider Quirks Behind Provider Strategy

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This plan follows `.agents/PLANS.md` in this repository. Keep it self-contained and update it whenever implementation discoveries change the work.

## Purpose / Big Picture

Cake sends model requests through either the Chat Completions API or the Responses API. Today the request builders contain provider-specific details directly: OpenRouter attribution headers are sent to every base URL, OpenRouter provider routing is built in the Responses backend, and the Kimi/Moonshot chat message compatibility workaround is controlled by a model-name helper inside `src/clients/chat_completions.rs`.

After this change, provider-specific behavior lives behind one strategy layer. A user configuring a non-OpenRouter endpoint will no longer send OpenRouter-only headers or routing fields. A user configuring OpenRouter will keep the same attribution headers and provider routing support. Kimi/Moonshot chat models will still receive the placeholder `reasoning_content` field needed for assistant tool-call messages.

## Progress

- [x] (2026-05-09 00:36Z) Read task 052, `.agents/PLANS.md`, current backend code, model config types, and tests.
- [x] (2026-05-09 00:36Z) Create this ExecPlan and link it from task metadata before code edits.
- [x] (2026-05-09 00:42Z) Add a provider strategy module and route current provider quirks through it.
- [x] (2026-05-09 00:42Z) Update Chat Completions and Responses request builders to use the strategy.
- [x] (2026-05-09 00:42Z) Add focused tests for OpenRouter-only headers/routing and Kimi message transformation.
- [x] (2026-05-09 00:45Z) Run formatting and the required full CI check.
- [x] (2026-05-09 00:47Z) Mark task 052 and this plan complete, then commit the finished work.

## Surprises & Discoveries

- Observation: The existing configuration has no explicit provider kind field.
  Evidence: `src/config/model.rs` and `src/config/settings.rs` only carry `model`, `api_type`, `base_url`, `api_key_env`, generation settings, reasoning settings, and `providers`.

- Observation: The default sandbox cannot bind the local ports used by existing `wiremock` response parsing tests.
  Evidence: `cargo test clients::chat_completions` and `cargo test clients::responses` initially failed with `Failed to bind an OS port for a mock server.: Operation not permitted`; rerunning those same commands with escalated permissions passed.

## Decision Log

- Decision: Detect OpenRouter from the configured `base_url` host instead of adding a new config field.
  Rationale: Task 052 asks to move existing quirks and apply OpenRouter behavior only when configured. The only current OpenRouter signal is the endpoint URL, so host detection preserves compatibility without requiring a settings migration. A later task can add explicit structured provider header configuration.
  Date/Author: 2026-05-09 / Codex

## Outcomes & Retrospective

Task 052 is complete. Provider-specific request behavior now lives in `src/clients/provider_strategy.rs`. Chat Completions and Responses request builders both call the strategy for headers, and Responses also calls it for OpenRouter provider routing. The Kimi/Moonshot reasoning placeholder compatibility transform moved out of the Chat Completions backend into the strategy layer. Focused strategy tests and the full `just ci` check passed.

## Context and Orientation

The relevant request backends are `src/clients/chat_completions.rs` and `src/clients/responses.rs`. Both expose `send_request`, which takes a `reqwest::Client`, a `ResolvedModelConfig`, conversation history, tool definitions, and per-turn retry overrides. `ResolvedModelConfig` is defined in `src/config/model.rs`; its `config.base_url` chooses the HTTP endpoint and its `config.model` chooses the provider model name. `config.providers` is a list of OpenRouter routing hints used to limit which upstream providers OpenRouter may select.

OpenRouter attribution headers are HTTP headers named `HTTP-Referer` and `X-Title`. They are useful for OpenRouter rankings and observability, but they are provider-specific and should not be sent to arbitrary OpenAI-compatible endpoints. Kimi/Moonshot chat models require an assistant message with `tool_calls` to also have a `reasoning_content` field, even when cake has no reasoning text to preserve. The existing workaround injects a single-space placeholder.

## Plan of Work

Create `src/clients/provider_strategy.rs` as a small internal module. It should expose `ProviderStrategy::from_config(&ResolvedModelConfig)`. The strategy should detect OpenRouter by parsing `config.config.base_url` and checking whether the host is `openrouter.ai` or a subdomain of `openrouter.ai`. If parsing fails, it should treat the provider as generic.

The strategy should provide three hooks. First, `apply_headers` takes a `reqwest::RequestBuilder` and returns a builder with provider-specific headers applied. For OpenRouter it adds the existing `HTTP-Referer: https://github.com/travisennis/cake` and `X-Title: cake`; for generic providers it returns the builder unchanged. Second, `responses_provider_config` returns the current OpenRouter provider routing payload only for OpenRouter and only when `providers` is not empty and is not exactly `["all"]`. Third, `transform_chat_messages` takes mutable Chat Completions messages and applies the Kimi/Moonshot `reasoning_content` placeholder when the model name contains `kimi`, preserving existing non-empty reasoning content.

Wire the new module into `src/clients/mod.rs`. In `src/clients/chat_completions.rs`, build messages, call `ProviderStrategy::from_config(config).transform_chat_messages(&mut messages)`, and use `strategy.apply_headers(client.post(...).json(...))` before bearer auth and send. In `src/clients/responses.rs`, replace inline provider routing with the strategy method and use the same header hook. Keep existing request JSON shapes for OpenRouter configurations.

Tests should cover the strategy directly and one or both request builders at the behavior boundary. The focused acceptance tests are: generic base URLs do not receive OpenRouter headers; OpenRouter base URLs do receive them; OpenRouter provider routing is omitted for generic base URLs but present for OpenRouter with non-`all` provider hints; the Kimi placeholder is still inserted through the strategy and not inserted for non-Kimi models.

## Concrete Steps

Run commands from `/Users/travisennis/Projects/cake`.

1. Inspect current state:

       sed -n '1,260p' .agents/.tasks/052.md
       sed -n '1,260p' .agents/PLANS.md
       sed -n '1,320p' src/clients/chat_completions.rs
       sed -n '1,320p' src/clients/responses.rs

2. Edit task and plan metadata:

       update .agents/.tasks/052.md with the ExecPlan path
       update .agents/exec-plans/active/index.md with provider-strategy.md

3. Implement `src/clients/provider_strategy.rs` and update `src/clients/mod.rs`, `src/clients/chat_completions.rs`, and `src/clients/responses.rs`.

4. Run targeted tests during development:

       cargo test provider_strategy
       cargo test clients::chat_completions
       cargo test clients::responses

5. Run final validation:

       cargo fmt
       just ci

## Validation and Acceptance

The task is complete when tests show the provider strategy owns the provider-specific behavior and the full project CI passes. `cargo test provider_strategy` should pass with tests proving OpenRouter detection, header gating, provider routing gating, and Kimi placeholder transformation. `just ci` should complete successfully.

Observable behavior after implementation: a request configured with `base_url = "https://api.example.com"` sends no `HTTP-Referer` or `X-Title` attribution headers and does not include an OpenRouter `provider` routing object. A request configured with `base_url = "https://openrouter.ai/api/v1"` keeps both headers and, when `providers = ["anthropic"]`, includes `provider.only = ["anthropic"]`.

## Idempotence and Recovery

The edits are additive and local to provider request construction. Re-running tests and formatting is safe. If a strategy change breaks snapshots, inspect whether the serialized request changed for OpenRouter scenarios; OpenRouter snapshots should remain behaviorally equivalent except for any intentional test refactor. Do not revert unrelated user changes in the working tree.

## Artifacts and Notes

Validation evidence:

       just ci
       Rust toolchain pins match rust-toolchain.toml (1.95.0)
       cargo clippy --all-targets --all-features -- -D warnings
       cargo test --quiet
       test result: ok. 499 passed; 0 failed
       test result: ok. 12 passed; 0 failed
       test result: ok. 8 passed; 0 failed
       Import lint passed!
       All checks passed!

## Interfaces and Dependencies

`src/clients/provider_strategy.rs` should define an internal strategy type available only inside `src/clients`:

    pub(super) struct ProviderStrategy<'a> {
        config: &'a ResolvedModelConfig,
        kind: ProviderKind,
    }

    impl<'a> ProviderStrategy<'a> {
        pub(super) fn from_config(config: &'a ResolvedModelConfig) -> Self;
        pub(super) fn apply_headers(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder;
        pub(super) fn responses_provider_config(&self) -> Option<ProviderConfig>;
        pub(super) fn transform_chat_messages(&self, messages: &mut [ChatMessage<'_>]);
    }

The exact private enum names may vary, but the public-to-module hooks should remain stable enough for both backends to use. No new third-party dependencies are required because `reqwest` already exposes URL parsing through its existing dependency graph.

## Revision Notes

- 2026-05-09 / Codex: Created the plan for task 052 before implementation. The plan chooses base URL host detection for OpenRouter because current settings do not carry an explicit provider kind.
- 2026-05-09 / Codex: Completed the plan after adding `ProviderStrategy`, updating both request backends, adding focused tests, and passing `just ci`.

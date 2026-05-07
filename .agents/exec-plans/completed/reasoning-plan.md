# Implementation Plan: Reasoning Effort Control

This ExecPlan is a living document. The sections `Progress`, `Surprises & Discoveries`, `Decision Log`, and `Outcomes & Retrospective` must be kept up to date as work proceeds.

This document follows `.agents/PLANS.md` from the repository root. It was migrated from the former `.agents/.plans/` location after implementation evidence showed reasoning configuration and runtime overrides are present in the current codebase.

## Purpose / Big Picture

Cake should let users control reasoning-capable models through settings and one-off CLI flags. After this work, a user can set reasoning effort or a reasoning token budget in `settings.toml`, override it with `--reasoning-effort` or `--reasoning-budget`, and have cake pass the resolved values to the Responses API or Chat Completions backend as appropriate.

The behavior is observable by reading generated request snapshots, running the reasoning-related unit tests, and using the documented CLI flags shown in `README.md` and `docs/design-docs/settings.md`.

## Progress

- [x] (2026-05-07 18:40Z) Confirmed model and settings structs expose `reasoning_effort`, `reasoning_summary`, and `reasoning_max_tokens`.
- [x] (2026-05-07 18:40Z) Confirmed `src/main.rs` parses and applies `--reasoning-effort` and `--reasoning-budget` overrides.
- [x] (2026-05-07 18:40Z) Confirmed `src/clients/responses.rs` sends `ReasoningConfig` and `src/clients/chat_completions.rs` sends `reasoning_effort`.
- [x] (2026-05-07 18:40Z) Confirmed docs in `README.md`, `docs/design-docs/settings.md`, and `docs/design-docs/cli.md` describe reasoning configuration.
- [x] (2026-05-07 18:40Z) Migrated this completed plan to `.agents/exec-plans/completed/reasoning-plan.md` and added the required ExecPlan lifecycle sections.

## Surprises & Discoveries

- Observation: The current implementation also includes reasoning budget recovery during retry handling.
  Evidence: `src/clients/retry.rs` tracks `reasoning_max_tokens` overrides when recovering from context-overflow errors.

- Observation: Chat Completions no longer simply skips reasoning in all cases; provider-specific reasoning content can be preserved in the message translation layer.
  Evidence: `src/clients/chat_completions.rs` contains reasoning content conversion and tests around `ReasoningContent`.

## Decision Log

- Decision: Classify this plan as completed during the ExecPlan migration.
  Rationale: The current repository contains settings, CLI overrides, backend request construction, response parsing, docs, and tests for the core reasoning-control behavior.
  Date/Author: 2026-05-07 / Codex

## Outcomes & Retrospective

Reasoning configuration is implemented across settings, CLI overrides, model resolution, request construction, response parsing, and documentation. The implementation grew beyond the original note by also participating in retry recovery and by preserving richer reasoning content in chat translation for providers that return it.

This document outlines the implementation plan for adding reasoning effort control to cake, based on the research findings.

## Overview

**Goal**: Enable users to configure and control reasoning effort for models via settings.toml and optionally at runtime.

**Current State**:
- Reasoning output parsing works for Responses API (including OpenRouter via Responses API)
- Reasoning tokens tracked for Responses API (hardcoded to 0 for Chat Completions)
- No reasoning input configuration exists

**Scope Decisions**:
- OpenRouter will only be supported via Responses API (not Chat Completions)
- Runtime CLI flags will be implemented for flexibility
- Budget-style configuration (`reasoning_max_tokens`) will be supported

---

## Phase 1: Configuration Layer

### 1.1 Update Data Structures

**File**: `src/config/types.rs` (or appropriate config module)

Add new fields to `ModelDefinition` and `ModelConfig`:

```rust
// In ModelDefinition
pub reasoning_effort: Option<String>,      // "none"|"low"|"medium"|"high"|"xhigh"
pub reasoning_summary: Option<String>,     // "concise"|"detailed"|"auto" (Responses API only)
pub reasoning_max_tokens: Option<u32>,     // Budget-style (Anthropic/Gemini via OpenRouter)
```

### 1.2 Update Settings Parsing

**File**: `src/config/settings.rs` (or appropriate settings module)

- Parse new `reasoning_effort`, `reasoning_summary`, `reasoning_max_tokens` fields from TOML
- Validate effort values against allowed enum: `["none", "minimal", "low", "medium", "high", "xhigh"]`
- Note: `minimal` is Chat Completions specific; map to `low` for Responses API if needed

### 1.3 Documentation Updates

**Files to update**:
- [ ] `README.md` - Add reasoning configuration to configuration section
- [ ] `docs/configuration.md` (if exists) - Add detailed reasoning configuration options
- [ ] `example-settings.toml` (if exists) - Add example with reasoning configuration

**Example documentation**:
```toml
[[models]]
name = "o4-mini-reasoning"
model = "openai/o4-mini"
api_type = "responses"
reasoning_effort = "high"        # Optional: none|low|medium|high|xhigh
reasoning_summary = "concise"    # Optional: concise|detailed|auto (Responses API only)

[[models]]
name = "claude-reasoning"
model = "anthropic/claude-3.7-sonnet"
reasoning_max_tokens = 8000     # Budget-style for Anthropic (via OpenRouter)
```

---

## Phase 2: Request Construction

### 2.1 Responses API Request

**File**: `src/clients/types.rs`

Add `ReasoningConfig` struct and update `Request`:

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ReasoningConfig {
    pub effort: Option<String>,
    pub summary: Option<String>,
}

// In Request struct
pub reasoning: Option<ReasoningConfig>,
```

**Implementation**:
- Build `ReasoningConfig` from `ModelConfig` when constructing requests
- Serialize as nested object: `{"reasoning": {"effort": "high", "summary": "concise"}}`

### 2.2 Chat Completions API Request

**File**: `src/clients/chat_types.rs`

Update `ChatRequest`:

```rust
// Standard OpenAI field
pub reasoning_effort: Option<String>,
```

**Implementation**:
- Serialize `reasoning_effort` as top-level string for Chat Completions API
- Note: OpenRouter is only supported via Responses API, so no OpenRouter-specific Chat Completions handling needed
- Standard OpenAI-compatible providers use the `reasoning_effort` field directly

### 2.3 Provider-Specific Normalization

**File**: `src/clients/mod.rs` or new file `src/clients/reasoning.rs`

Create a normalization layer that:
- Maps effort levels to provider-specific parameters
- Handles Anthropic budget conversion (effort → token budget) for Responses API via OpenRouter
- Handles Gemini `thinkingLevel` and `thinkingBudget` mapping for Responses API via OpenRouter

```rust
pub fn normalize_reasoning_for_provider(
    config: &ModelConfig,
    provider: &Provider,
) -> ProviderReasoningConfig;
```

**Note**: Normalization applies to Responses API (including OpenRouter). Chat Completions uses standard `reasoning_effort` field without provider-specific extensions.

### 2.4 Documentation Updates

**Files to update**:
- [ ] `docs/api.md` (if exists) - Document the reasoning parameters sent to each API
- [ ] Inline code comments - Document the mapping between effort levels and provider-specific values

---

## Phase 3: Response Parsing Improvements

### 3.1 Chat Completions Token Parsing

**File**: `src/clients/chat_completions.rs`

**Current issue**: `reasoning_tokens` hardcoded to 0

**Fix**:
- Parse `usage.completion_tokens_details.reasoning_tokens` from response
- Handle missing field gracefully (default to 0)

```rust
// In response parsing
let reasoning_tokens = response
    .usage
    .completion_tokens_details
    .reasoning_tokens
    .unwrap_or(0);
```

### 3.2 OpenRouter Reasoning Content Parsing (Responses API Only)

**File**: `src/clients/responses.rs`

OpenRouter reasoning support is implemented via the Responses API (not Chat Completions).
Reasoning content parsing for OpenRouter already works through the standard Responses API
handling in `src/clients/responses.rs`.

No additional changes needed here since OpenRouter Chat Completions is not supported.

### 3.3 Documentation Updates

**Files to update**:
- [ ] `docs/responses.md` (if exists) - Document how reasoning tokens are extracted from each API
- [ ] Code comments - Document the different response shapes

---

## Phase 4: Runtime Override (Optional)

### 4.1 CLI Flag Support

**File**: `src/cli.rs` (or appropriate CLI module)

Add optional CLI flag:
```rust
--reasoning-effort <effort>    # Override configured reasoning effort
--reasoning-budget <tokens>    # Override with token budget
```

### 4.2 Runtime Configuration Merge

**File**: `src/config/runtime.rs` (new or existing)

Implement merge logic:
```rust
pub fn merge_reasoning_config(
    model_config: &ModelConfig,
    cli_override: Option<&str>,
) -> ReasoningConfig;
```

Priority: CLI override > Model config > Default

### 4.3 Documentation Updates

**Files to update**:
- [ ] `README.md` - Add CLI flags to usage section
- [ ] `docs/cli.md` (if exists) - Document reasoning CLI options

---

## Phase 5: Testing & Validation

### 5.1 Unit Tests

**Files to create/update**:
- [ ] `tests/config/reasoning_test.rs` - Test TOML parsing of reasoning config
- [ ] `tests/clients/reasoning_test.rs` - Test request construction for each API type

### 5.2 Integration Tests

- [ ] Test Responses API with reasoning effort
- [ ] Test Chat Completions with reasoning_effort
- [ ] Test OpenRouter with extended reasoning config
- [ ] Test token counting from responses

### 5.3 Documentation Updates

**Files to update**:
- [ ] `CONTRIBUTING.md` (if exists) - Add testing guidelines for reasoning features
- [ ] `CHANGELOG.md` (if exists) - Document new reasoning capabilities

---

## Implementation Order

| Phase | Priority | Effort | Dependencies |
|-------|----------|--------|--------------|
| Phase 1.1-1.2 | High | Low | None |
| Phase 2.1 | High | Low | Phase 1 |
| Phase 2.2 | High | Medium | Phase 1 |
| Phase 3.1 | High | Low | None |
| Phase 1.3 | Medium | Low | Phase 1.1-1.2 |
| Phase 2.3 | Medium | Medium | Phase 2.1, 2.2 |
| Phase 3.2 | Medium | Medium | Phase 3.1 |
| Phase 4 | Low | Medium | Phase 1, 2 |
| Phase 2.4, 3.3 | Medium | Low | Phase 2, 3 |
| Phase 5 | High | High | All phases |

---

## Open Questions to Resolve

1. **Runtime override**: Should `--reasoning-effort` CLI flag be implemented? 
   - **Decision**: **YES** - Implement in Phase 4 for flexibility

2. **OpenRouter extended config**: Should cake support OpenRouter's `reasoning` object for Chat Completions?
   - **Decision**: **NO** - With OpenRouter, only supporting the Responses API (not Chat Completions)
   - **Impact**: Phase 2.2 can be simplified; no OpenRouter-specific Chat Completions handling needed

3. **Budget-style configuration**: Should `reasoning_max_tokens` be exposed?
   - **Decision**: **YES** - Implement for Anthropic/Gemini support via OpenRouter

---

## Files to Create/Modify Summary

### New Files
- `src/clients/reasoning.rs` - Reasoning configuration normalization
- `tests/config/reasoning_test.rs` - Configuration tests
- `tests/clients/reasoning_test.rs` - Request construction tests

### Modified Files
- `src/config/types.rs` - Add reasoning fields to ModelDefinition/ModelConfig
- `src/config/settings.rs` - Parse reasoning configuration
- `src/clients/types.rs` - Add ReasoningConfig to Request
- `src/clients/chat_types.rs` - Add reasoning_effort field (standard OpenAI only)
- `src/clients/chat_completions.rs` - Parse reasoning tokens from response
- `src/cli.rs` - Add CLI flags (Phase 4)

### Documentation Files
- `README.md` - Add reasoning configuration section
- `docs/configuration.md` - Detailed reasoning options
- `docs/api.md` - API parameter documentation
- `docs/responses.md` - Response parsing documentation
- `docs/cli.md` - CLI flag documentation
- `example-settings.toml` - Example configuration
- `CHANGELOG.md` - Feature changelog entry

---

## Estimated Timeline

| Phase | Duration | Notes |
|-------|----------|-------|
| Phase 1 | 1-2 days | |
| Phase 2 | 1-2 days | Reduced from 2-3 days (no OpenRouter Chat Completions handling) |
| Phase 3 | 1 day | Reduced (no OpenRouter Chat Completions parsing needed) |
| Phase 4 | 1 day | |
| Phase 5 | 2-3 days | |
| **Total** | **6-9 days** | Reduced from 7-11 days due to simplified OpenRouter scope |

---

## Success Criteria

- [ ] Users can configure reasoning effort in settings.toml
- [ ] Users can configure `reasoning_max_tokens` for budget-style reasoning
- [ ] Requests to Responses API include `reasoning.effort` parameter
- [ ] Requests to Chat Completions API include `reasoning_effort` parameter
- [ ] Chat Completions responses correctly parse `reasoning_tokens`
- [ ] CLI flags `--reasoning-effort` and `--reasoning-budget` work for runtime override
- [ ] Documentation covers all new configuration options
- [ ] Unit and integration tests pass

**Note**: OpenRouter reasoning support is via Responses API only (not Chat Completions).

## Revision Notes

- 2026-05-07 / Codex: Migrated this historical plan into the new completed ExecPlan directory and added lifecycle sections required by `.agents/PLANS.md`. The original phase breakdown above remains as historical context.

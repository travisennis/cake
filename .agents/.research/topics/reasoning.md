# Reasoning

I want to provide better support for controlling the reasoning effort of models in cake. This is currently in the research phase. The reasoning effort for models can be configured when creating a model via settings.toml.

## Current State in cake

- **Reasoning output parsing** works for the Responses API: reasoning items (with `summary`, `encrypted_content`, and `content`) are captured as `ConversationItem::Reasoning` and echoed back for multi-turn.
- **Reasoning tokens** are tracked in `OutputTokensDetails::reasoning_tokens` for the Responses API; hardcoded to `0` for Chat Completions.
- **No reasoning *input* configuration** exists: neither `ModelConfig`, `ModelDefinition`, nor `Request`/`ChatRequest` includes a reasoning effort or budget field. The model will only reason if it decides to on its own.

## Research Findings

### 1. Responses API — `reasoning` parameter

The Responses API accepts a top-level `reasoning` object on the request:

```json
{
  "model": "openai/o4-mini",
  "input": [...],
  "reasoning": {
    "effort": "high",      // ReasoningEffortEnum
    "summary": "concise"   // ReasoningSummaryEnum (optional)
  }
}
```

**`effort`** (enum): `"none"` | `"low"` | `"medium"` | `"high"` | `"xhigh"`
- Controls how much computational effort the model spends on reasoning before answering.
- `"none"` disables reasoning entirely. `"xhigh"` is maximum effort.

**`summary`** (enum, optional): `"concise"` | `"detailed"` | `"auto"`
- Controls whether the response includes a human-readable summary of the reasoning.
- Currently cake already parses summaries from response output; this parameter lets you *request* them.

**Response shape**: Reasoning appears as an output item of `type: "reasoning"` with `encrypted_content` (opaque, must be echoed back) and `summary` (array of summary text). Usage includes `output_tokens_details.reasoning_tokens`. cake already handles all of this.

**Streaming events**: `response.reasoning.delta`, `response.reasoning.done`, `response.reasoning_summary_part.added`, `response.reasoning_summary_part.done`. (Not relevant until cake supports streaming.)

### 2. Chat Completions API — `reasoning_effort` parameter

The Chat Completions API accepts a top-level `reasoning_effort` string:

```json
{
  "model": "o4-mini",
  "messages": [...],
  "reasoning_effort": "high"
}
```

**`reasoning_effort`** (string): `"none"` | `"minimal"` | `"low"` | `"medium"` | `"high"` | `"xhigh"`
- Same semantic as the Responses API `effort`, but it's a flat string rather than a nested object.
- `gpt-5.1` defaults to `"none"` (no reasoning); earlier models default to `"medium"`.
- `"xhigh"` only supported for models after `gpt-5.1-codex-max`.

**Response shape**: Reasoning tokens are reported in `usage.completion_tokens_details.reasoning_tokens`. The Chat Completions API does *not* return reasoning text/traces in the response body — it only reports the token count. cake currently hardcodes `reasoning_tokens: 0` for Chat Completions and should instead parse `completion_tokens_details` from the response.

### 3. OpenRouter's Unified `reasoning` Parameter (Chat Completions)

When using OpenRouter as the base URL with the Chat Completions API, OpenRouter accepts a unified `reasoning` object (via `extra_body`):

```json
{
  "model": "...",
  "messages": [...],
  "reasoning": {
    "effort": "high",           // "xhigh"|"high"|"medium"|"low"|"minimal"|"none"
    "max_tokens": 2000,         // Alternative to effort (Anthropic-style budget)
    "exclude": false,           // Hide reasoning from response but still use it
    "enabled": true             // Simple toggle
  }
}
```

- `effort` and `max_tokens` are mutually exclusive — use one or the other.
- `max_tokens` is for Anthropic/Gemini models that support a token budget rather than effort levels.
- `exclude: true` hides reasoning output but still lets the model reason internally.
- OpenRouter normalizes these across providers (OpenAI, Anthropic, Google, etc.).

**Response shape with OpenRouter Chat Completions**: Reasoning text appears in `choices[0].message.reasoning` (a string) and structured detail in `choices[0].message.reasoning_details` (array). cake does not currently parse either of these fields.

### 4. Provider-Specific Nuances

| Provider | Effort support | Budget support | Notes |
|----------|---------------|----------------|-------|
| OpenAI (o-series, GPT-5) | `effort` ✅ | ❌ | Does not return reasoning text, only token counts |
| Anthropic (Claude 3.7+) | `effort` → mapped to budget | `max_tokens` ✅ | Budget range: 1024–128000 tokens. `max_tokens` must exceed budget. |
| Google Gemini 3 | `effort` → `thinkingLevel` | `max_tokens` → `thinkingBudget` | Token consumption determined internally by Google |
| Grok (xAI) | `effort` ✅ | ❌ | |
| DeepSeek R1 | via `exclude` | ❌ | Reasons by default; use `exclude` to hide |

### 5. Recommended Implementation Plan

#### Phase 1: Configuration (settings.toml + ModelConfig)

Add to `ModelDefinition` and `ModelConfig`:

```toml
[[models]]
name = "o4-mini"
model = "openai/o4-mini"
api_type = "responses"
reasoning_effort = "high"     # none|low|medium|high|xhigh
```

New fields:
- `reasoning_effort: Option<String>` — the effort level enum value
- Consider also: `reasoning_summary: Option<String>` — for Responses API summary control

#### Phase 2: Request Construction

**Responses API** (`src/clients/types.rs` → `Request`):
- Add `reasoning: Option<ReasoningConfig>` where `ReasoningConfig { effort: Option<String>, summary: Option<String> }`.
- Serialize as `{"reasoning": {"effort": "high", "summary": "concise"}}`.

**Chat Completions API** (`src/clients/chat_types.rs` → `ChatRequest`):
- Add `reasoning_effort: Option<String>`.
- Serialize as `{"reasoning_effort": "high"}`.

**OpenRouter Chat Completions** (if targeting OpenRouter via Chat Completions):
- Add `reasoning: Option<OpenRouterReasoningConfig>` with `effort`, `max_tokens`, `exclude` fields.
- This is an OpenRouter extension; decide whether to support it or stick to standard APIs.

#### Phase 3: Response Parsing Improvements

**Chat Completions** (`src/clients/chat_completions.rs`):
- Parse `completion_tokens_details.reasoning_tokens` from the response instead of hardcoding to 0.
- Optionally parse `reasoning` string and `reasoning_details` array from OpenRouter responses.

#### Open Questions

1. **Should `reasoning_effort` also be overridable at runtime?** (e.g., CLI flag `--reasoning-effort high`)
2. **Should OpenRouter's extended `reasoning` object be supported for Chat Completions**, or should cake only support the standard `reasoning_effort` string?
3. **Should `reasoning.max_tokens` (budget-style) be exposed**, or should effort levels be the only interface?
4. **Should `reasoning.exclude` be supported** for cases where reasoning is wanted internally but not in output?

## References

### Responses API

- https://openrouter.ai/docs/api/reference/responses/reasoning.md
- https://openrouter.ai/docs/guides/best-practices/reasoning-tokens.md
- https://www.openresponses.org/openapi/openapi.json

### Chat Completions API

- https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create/index.md

# Providers, Models, And Settings

## Scope

Read this before changing Responses API or Chat Completions backends, provider
strategy, retry behavior, model config, settings loading, profiles, provider
headers, API request shaping, or reasoning options.

## Compatibility Surfaces

- `settings.toml` keys, precedence, and profile merge behavior.
- `ApiType` backend selection and provider inference.
- OpenRouter provider headers and request patches.
- Retry classification, backoff, and context-overflow recovery.
- Request/response normalization into shared usage and conversation types.

## Required Checks

- Add focused tests for config precedence, request shaping, or retry behavior
  when touched.
- Snapshot API request construction when changing serialized backend payloads.
- Do not update model defaults, provider behavior, or settings shape without
  explicit scope and docs.

## Common Failure Modes

- Fixing one backend while silently changing the other.
- Adding a settings key without documenting precedence or examples.
- Leaking provider-specific behavior into backend-neutral types.
- Treating OpenRouter behavior as universal OpenAI-compatible behavior.

## Related Docs

- [settings.md](../design-docs/settings.md)
- [api-retry-strategy.md](../design-docs/api-retry-strategy.md)
- [conversation-types.md](../design-docs/conversation-types.md)
- [responses-api.md](../references/responses-api.md)
- [chat-completions-api.md](../references/chat-completions-api.md)
- [ADR 003: Settings Profiles](../adr/003-settings-profiles.md)
- [ADR 008: Structured Provider Headers](../adr/008-structured-provider-headers.md)

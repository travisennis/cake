---
status: accepted
date: 2026-05-19
---

# Structured Provider Headers

## Context

Cake supports OpenAI-compatible providers through shared Chat Completions and Responses request builders. Some providers need provider-specific request metadata. OpenRouter attribution headers were previously hardcoded behind URL detection, which kept behavior compatible but left the provider header contract implicit and unavailable to settings.

The settings model already carries provider-adjacent values such as `api_type`, `base_url`, and OpenRouter routing hints. Provider headers should be represented as structured configuration, not as ad hoc request-builder literals.

## Decision

Model settings may declare a provider kind with `provider = "openrouter"`. If omitted, cake may infer OpenRouter from an `openrouter.ai` base URL for backward compatibility.

Model settings may also declare an optional `provider_headers` table:

```toml
provider = "openrouter"
provider_headers = { http_referer = "https://example.com", x_title = "my-app" }
```

`provider_headers` is interpreted only by provider strategies that understand those fields. For OpenRouter, `http_referer` maps to the `HTTP-Referer` request header and `x_title` maps to `X-Title`. If an OpenRouter model omits `provider_headers`, cake sends the existing default attribution headers. If the model provides an empty `provider_headers` table, no OpenRouter attribution headers are sent.

## Rationale

- Keeps provider-specific HTTP behavior in `ProviderStrategy`, where existing provider routing and compatibility transforms already live.
- Preserves existing OpenRouter configurations while making the header contract visible in settings.
- Avoids arbitrary user-defined HTTP headers, which would require broader validation, redaction, and security policy.

## Consequences

- **Positive**: OpenRouter attribution headers are configurable, testable, and documented as part of model configuration.
- **Positive**: Generic OpenAI-compatible providers continue to avoid OpenRouter-only headers.
- **Negative**: Only the known OpenRouter header fields are supported initially; new provider headers require schema changes.

## Alternatives Considered

- **Arbitrary header map**: Rejected because it would introduce unclear security and redaction obligations for auth-like or provider-specific headers.
- **URL detection only**: Rejected because it leaves provider header behavior implicit and not configurable from settings.

## References

- Task 104: Add Structured Provider Header Configuration
- `src/config/model.rs`
- `src/config/settings.rs`
- `src/clients/provider_strategy.rs`

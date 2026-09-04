---
status: accepted
date: 2026-09-04
decision-makers: Travis Ennis
informed: issue 444
---

# OpenCode Session Header

## Context and Problem Statement

OpenCode Zen (`https://opencode.ai/zen`) requires callers to send `x-opencode-session` with the current session ID, and will start erroring on missing headers imminently. Cake sends no such header today. The header must reach OpenCode on both wire backends (Chat Completions and Responses), for both agent turns and LLM-judge evaluations, without becoming a generic arbitrary-header mechanism.

Prior behavior: `ProviderStrategy::apply_headers` sends OpenRouter-only `HTTP-Referer` / `X-Title` from static `provider_headers` TOML, with explicit `provider = "openrouter"` plus `openrouter.ai` URL fallback (see ADR-008). Cake's session UUID is the `~/.local/share/cake/sessions/{uuid}.jsonl` identity; a run always has one (generated) even when the session is not persisted to disk.

## Decision Drivers

- OpenCode must receive `x-opencode-session: <cake session UUID>` on every agent provider POST when the model targets OpenCode Zen.
- Generic (non-OpenCode) providers must send no such header.
- The value is dynamic per run and cannot live in static `provider_headers` TOML.
- Judge evaluations reuse the same `Backend` but carry no session identity, so they need explicit header behavior.
- Only `x-opencode-session` is required; ADR-008 already rejected a generic arbitrary-header escape hatch.
- The session ID is not secret; it needs no log/diagnostic/telemetry redaction (unlike the API key and OpenRouter headers).

## Considered Options

- **Static `provider_headers` entry for the session ID:** Rejected. The value is the run's live session UUID, not a configured constant; TOML cannot supply it.
- **Generic user-defined header map:** Rejected, per ADR-008. It reintroduces unclear validation, redaction, and security obligations for auth-like headers.
- **Explicit `provider = "opencode"` plus `opencode.ai` URL fallback, with dynamic per-request injection (chosen):** Mirrors the OpenRouter precedent, keeps provider-specific HTTP behavior in `ProviderStrategy`, and threads the live session UUID at request-build time on both backends.

## Decision Outcome

Chosen option: explicit `provider = "opencode"` plus URL fallback, with dynamic per-request injection, because it follows the established provider strategy boundary and supplies a value TOML cannot.

- New `ModelProvider::OpenCode` (`provider = "opencode"`). When unset, Cake infers OpenCode from an `opencode.ai` base-URL host (exact plus dot-subdomains, mirroring the OpenRouter rule); this covers `https://opencode.ai/zen` and its versioned paths.
- `ProviderStrategy::apply_headers` takes the request's session UUID and, for OpenCode only, sets `x-opencode-session` to its string form. Generic providers ignore the UUID and send no such header.
- Agent requests send the run's session UUID (`Agent::session_id`), stable for the run whether or not the session is persisted.
- Each judge call generates its own fresh unique UUID per logical evaluation (shared across its at-most-one bounded retry), since the judge carries no session identity.
- The header applies on both Chat Completions and Responses backends via the shared `Backend` send path.
- No secret-handling changes: the session ID is not added to diagnostic or telemetry redaction.

### Consequences

- **Positive:** OpenCode Zen receives its required session header on every agent and judge POST without new static configuration.
- **Positive:** Generic providers are unaffected; no arbitrary-header surface is introduced.
- **Negative:** A new provider variant plus request-path threading touches both backends and the judge observer; focused header tests must guard it.

## More Information

- Issue 444: Send `x-opencode-session` session header for OpenCode Zen provider.
- ADR-008: Structured Provider Headers (explicit-vs-inferred precedent, rejection of arbitrary headers).
- `src/config/model.rs`, the provider and header-shape authority.
- `src/clients/provider_strategy.rs`, the header-application authority.
- `src/clients/chat_completions.rs` and `src/clients/responses.rs`, the wire paths.
- `src/clients/judge_observer.rs`, the judge header behavior.
- [Configuration](../configuration.md), the new `provider` value.

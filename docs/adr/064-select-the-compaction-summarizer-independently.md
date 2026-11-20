---
status: accepted
date: 2026-11-20
decision-makers: Travis Ennis
---

# Select the compaction summarizer independently

## Context and Problem Statement

Core pre-request compaction (ADR 047, task 034g) summarizes through the conversation's resolved model configuration. The production summary adapter clones that configuration and changes only `max_output_tokens`, so the summary request carries the main thread's provider, credentials, and reasoning effort. A session running a high-effort model therefore pays the same effort for every checkpoint, even though summary generation is a bounded, tool-free side call.

External `cake-compact` already lets a caller choose a summarizer model, profile, and reasoning effort for between-turn compaction. Core compaction has no equivalent: `--model` and `--profile` move the main thread and the summarizer together, which is not a summarizer override. The configuration shape is a durable compatibility surface, so the new keys need an explicit decision.

## Decision Drivers

- Let users choose a cheaper or faster model and effort for checkpoint summaries without changing the main thread.
- Preserve current behavior when no summarizer selection is configured.
- Reuse the existing `[[models]]` and profile vocabulary instead of introducing a second model registry or provider surface.
- Keep summary calls tool-free, hook-free, and schema-free, and keep the bounded generation cap and Codex exclusion unchanged.
- Fail at settings load for unknown names rather than at the first checkpoint.

## Considered Options

- **Keep the main model (status quo).** Simple and always consistent, but couples summary cost and latency to the main model's effort.
- **Add only `[compaction] model`.** Covers explicit selection but cannot reuse a profile's existing model choice.
- **Add `model`, `profile`, and `reasoning_effort` (chosen).** Mirrors the external wrapper's vocabulary and lets each dimension override independently.
- **Apply the named profile's full settings to summary calls.** Would pull in skills, tools, sandbox paths, and nested compaction policy that a tool-free summary call cannot use, and would widen the trust surface.

## Decision Outcome

Chosen: add three optional keys to `[compaction]`:

- `model` names a `[[models]]` entry, the same vocabulary as `default_model` and `--model`.
- `profile` names a profile. Only that profile's effective `default_model` participates; its skills, tools, sandbox, directories, and compaction budgets do not apply to summary calls.
- `reasoning_effort` overrides the summary call's effort.

Selection precedence is `model`, then the named profile's `default_model`, then the conversation configuration. The first two select a full model entry, including its provider, credentials, and declared effort. `reasoning_effort` overrides the selected entry's effort. With neither `model` nor `profile`, an effort override clones the conversation configuration and changes only the effort. With no keys at all, the conversation configuration is used exactly as before.

The summarizer selection resolves once at run start into a separate `ResolvedModelConfig`. `AgentSummaryCaller` receives it and falls back to the conversation configuration when absent. `summary_config` still changes only `max_output_tokens`, so the summary call remains a bounded, tool-free request. When automatic compaction is enabled, backend support is validated against the selected summarizer configuration, not only the conversation model; the Codex Responses backend remains unable to create checkpoints. Disabled policy skips credential resolution and preserves restoration of existing checkpoints.

Settings validation rejects an unknown `model` or `profile` at load time. `[compaction]` remains low-authority: the keys can change request size, cost, and provider choice for a summary call, but cannot add filesystem authority, execute code, or read host files.

### Consequences

- Good, because summary cost and latency become independent of the main thread.
- Good, because the keys reuse existing model and profile names and follow the same per-key global/project/profile precedence as the budget keys.
- Bad, because one session can now make requests to two providers, expanding the credential and failure surface; the existing summary failure suppression and bounded attempt rules limit that exposure.
- Neutral, because checkpoint records still carry the main conversation model identity; the summarizer identity is not persisted in this change.

## More Information

The implementation lands in task 184. `docs/configuration.md` owns the user surface. ADR 047 remains the record for checkpoint persistence and the opt-in automatic policy; this ADR only extends its summarizer selection.

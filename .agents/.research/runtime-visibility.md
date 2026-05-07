# Runtime Visibility And Validation

## Original Request

The user asked:

> there are certain things in this app where I don't feel like I have good visibility. for example, what is the effective value of settings when cake reads both project and user settings. or what messages are actually being sent to the model at any given time. what would be good practices for being to see and validate those things?

## Context From The Codebase

`cake` already has some raw observability mechanisms:

- Settings are loaded and merged in `src/config/settings.rs`.
- Prompt context is built in `src/prompts/mod.rs`.
- Responses API payloads are constructed in `src/clients/responses.rs`.
- Chat Completions payloads are constructed in `src/clients/chat_completions.rs`.
- Request payloads are logged at `trace` level before sending.
- Session JSONL records already include messages, function calls, function outputs, reasoning, task records, and `prompt_context` audit records.
- `prompt_context` records are intentionally audit-only; they are appended to session files but are not replayed on continue/resume/fork.

The main gap is that these mechanisms require knowing where to look and how to reconstruct behavior. The better practice is to make effective runtime state a first-class inspection surface.

## Recommended Practices

Good practice here is to make "effective runtime state" a first-class inspectable artifact, not something inferred from logs.

For `cake`, add these surfaces.

### 1. Config Inspection

Add a command such as:

```bash
cake config inspect --profile review --json
cake config inspect --profile review --explain
```

It should show the final merged settings plus provenance:

```json
{
  "default_model": {
    "value": "zen",
    "source": ".cake/settings.toml",
    "overrode": "~/.config/cake/settings.toml"
  },
  "models.zen.temperature": {
    "value": 0.7,
    "source": "~/.config/cake/settings.toml"
  },
  "directories": [
    {
      "value": "../shared-libs",
      "source": "profile:review"
    }
  ]
}
```

This maps directly to `src/config/settings.rs`, where settings are already merged but source/provenance is currently discarded.

Important behavior to expose:

- Which settings files were discovered.
- Which settings files were missing.
- Which model definitions came from global settings.
- Which model definitions came from project settings.
- Which project model definitions overrode global ones.
- Which `default_model` won.
- Which profile overlays were applied.
- Which profile fields came from global settings versus project settings.
- Which CLI flags overrode settings, such as `--model`, `--skills`, `--no-skills`, `--reasoning-effort`, `--reasoning-budget`, and `--add-dir`.
- The final selected model config after CLI overrides.
- The final skill discovery mode.
- The final sandbox directory set from persistent settings and CLI flags.

The CLI should support both human and machine formats:

```bash
cake config inspect
cake config inspect --profile review
cake config inspect --json
cake config inspect --explain
```

The `--json` output should be stable enough for tests and scripting. The `--explain` output should be optimized for humans.

### 2. Prompt And Message Inspection

Add a dry-run command that shows what would be sent without calling the model:

```bash
cake prompt inspect "fix this bug"
cake prompt inspect --api-format responses "fix this bug"
cake prompt inspect --api-format chat-completions "fix this bug"
cake prompt inspect --json "fix this bug"
```

It should distinguish:

- Internal `ConversationItem` history.
- Prompt context from `AGENTS.md`, skills, current working directory, and date.
- Final provider-specific payload.

That distinction matters because Responses sends developer context differently than Chat Completions. Responses puts the system prompt into the top-level `instructions` field and sends remaining messages in `input`. Chat Completions folds developer context into the first user message for compatibility with providers that do not consistently support developer-role messages.

Useful inspection modes:

```bash
cake prompt inspect "prompt"
cake prompt inspect --internal "prompt"
cake prompt inspect --provider-payload "prompt"
cake prompt inspect --api-format responses "prompt"
cake prompt inspect --api-format chat-completions "prompt"
cake prompt inspect --json "prompt"
```

The inspection output should make these things obvious:

- The exact system prompt.
- Each developer context message.
- Which `AGENTS.md` files contributed content.
- Which skill catalog entries were included.
- Whether skills were disabled or filtered.
- The current environment context message.
- How the user prompt was combined with stdin, if applicable.
- Whether this is a new, continued, resumed, or forked session.
- Which prior conversation items are being replayed.
- Which audit-only `prompt_context` records are not being replayed.
- The final provider payload shape.

### 3. Request Capture With Redaction

There is already a partial version of this: `RUST_LOG=cake=trace cake ...` logs the serialized request payload before sending it. Formalize it as a deliberate debugging feature:

```bash
cake --dump-request /tmp/cake-request.json "prompt"
cake --dump-request - --no-send "prompt"
```

Recommended behavior:

- Redact API keys and authorization headers.
- Optionally redact file contents and tool outputs.
- Clearly mark whether redaction occurred.
- Include request metadata such as provider URL, API type, model, selected tools, and session ID.
- Support `--no-send` so users can validate payloads without making a paid/model call.
- If full prompt content is included, warn that the dump may contain sensitive information.

This should be distinct from trace logging. Trace logs are useful for developers, but a named request dump is more discoverable, easier to test, and easier to attach to bug reports.

### 4. Session Viewer

Sessions already persist useful JSONL records. Add a friendly frontend:

```bash
cake session show latest
cake session show latest --messages
cake session show latest --tools
cake session show latest --prompt-context
cake session show latest --request-shape
```

This would replace common manual `jq` workflows such as:

```bash
jq 'select(.type == "message")' ~/.local/share/cake/sessions/{uuid}.jsonl
jq 'select(.type == "function_call" or .type == "function_call_output")' ~/.local/share/cake/sessions/{uuid}.jsonl
jq 'select(.type == "prompt_context")' ~/.local/share/cake/sessions/{uuid}.jsonl
```

Recommended views:

- `--summary`: task IDs, timestamps, model, status, token usage.
- `--messages`: user and assistant messages.
- `--tools`: function calls and outputs correlated by call ID.
- `--reasoning`: reasoning summaries and reasoning metadata.
- `--prompt-context`: prompt context used for each task.
- `--raw`: raw JSONL records.
- `--json`: stable structured output.

The viewer should preserve the distinction between durable conversation history and audit-only records. That is especially important for prompt context because fresh context is rebuilt on each invocation.

### 5. Validation Tests And Snapshots

Keep using snapshot tests, but make the inspect commands snapshot-backed.

Useful test cases:

- Given global settings plus project settings, assert the exact effective config.
- Given global settings plus project settings plus profile, assert the exact effective config.
- Given CLI overrides, assert the exact final selected model config.
- Given `AGENTS.md` files plus skill catalog plus user prompt, assert exact internal prompt context.
- Given internal history, assert exact Responses payload.
- Given internal history, assert exact Chat Completions payload.
- Given continue/resume/fork, assert which records are replayed and which are audit-only.

The repo already has snapshots for prompts and API request construction, so this fits the current testing style.

### 6. Make Provenance A Data Model, Not Just Display Text

For settings, avoid bolting provenance onto final display code only. Instead, introduce an internal model that tracks source information while merging.

Example shape:

```rust
struct Sourced<T> {
    value: T,
    source: SettingsSource,
}

enum SettingsSource {
    GlobalSettings { path: PathBuf },
    ProjectSettings { path: PathBuf },
    GlobalProfile { path: PathBuf, profile: String },
    ProjectProfile { path: PathBuf, profile: String },
    CliFlag { name: String },
    Default,
}
```

Then expose:

- Existing `LoadedSettings` for runtime behavior.
- New diagnostic/effective settings output for inspection.

This keeps the runtime API simple while making diagnostics accurate.

### 7. Prefer Dry-Run Validation Over Log Scraping

Logs should remain useful, but users should not need to inspect logs to answer basic questions. The best hierarchy is:

1. `cake config inspect` for effective settings.
2. `cake prompt inspect` for prompt/message construction.
3. `cake --dump-request --no-send` for exact provider payload validation.
4. `cake session show` for after-the-fact auditing.
5. `RUST_LOG=cake=trace` for developer-level debugging.

That gives users increasingly deeper tools without forcing everyone into trace logs or raw JSONL files.

## Highest-Value Implementation

The highest-value implementation would be:

```bash
cake config inspect --explain
cake prompt inspect --json --no-send
```

Those two commands would answer most "what is cake actually doing?" questions without requiring trace logs or live API calls.

## Suggested Implementation Order

1. Add internal structs for sourced/effective settings diagnostics.
2. Add `cake config inspect --json` and `cake config inspect --explain`.
3. Extract provider request construction into testable functions that can be called without sending HTTP requests.
4. Add `cake prompt inspect --json --no-send`.
5. Add `--dump-request` for actual runtime request capture.
6. Add `cake session show` as a convenience layer over existing JSONL session files.
7. Add snapshot tests for config, prompt, and provider payload inspection.

## Design Principle

The user should be able to answer these questions without reading source code:

- What settings files did `cake` read?
- Which setting won and why?
- Which profile and CLI overrides were applied?
- What model configuration is active?
- What prompt context is active?
- What prior session history is being replayed?
- What exact provider payload would be sent?
- What did the model actually receive on a past turn?

If those answers are exposed through stable commands, debugging becomes validation instead of archaeology.

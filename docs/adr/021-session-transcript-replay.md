---
status: accepted
date: 2026-08-14
decision-makers: Travis Ennis
informed: issue 59
---

# Session Transcript Replay

## Context and Problem Statement

Terminal clients such as `cake-repl` want to reopen a session with visible history before continuing it. `cake --resume <uuid>` already reloads conversation history into the provider context, but a client cannot render the prior transcript itself: it would have to read Cake's private JSONL session files, parse human-readable output, or import Cake internals. `cake-repl` intentionally treats Cake as an external engine, so none of those are acceptable. The stream-json output vocabulary exists for the current task only, and live streams deliberately omit session metadata, prompt context, skill activation, and prior tasks.

## Decision Drivers

- Replay must be a first-class, read-only client operation with no LLM call, no session mutation, no lock, and no network.
- Replay output must reuse the machine-readable `StreamRecord` vocabulary so existing stream-json consumers can parse it with no new contract.
- Record kinds that exist only in the persisted session file (`session_meta`, `prompt_context`, `skill_activated`) must be emitted by replay, or clients still cannot render a full transcript.
- Failures must be explicit and structured: both a process exit code and a stream event, so a parser learns the failure without waiting for exit status.
- New event kinds and fields must be additive and versionable so old clients can decode forward-compatibly.

## Considered Options

- **`--replay <uuid>` flag on `CodingAssistant`.** Rejected: the flag would be parsed alongside prompt-related flags and would need validation against conflicting combinations (`--continue`, `--fork`, a `PROMPT` argument).
- **`cake debug replay <uuid>`.** Rejected: replay is a primary client operation, not a diagnostic, and `cake replay <uuid>` is self-documenting.
- **Replay-only wrapper event types.** Rejected: a single typed event vocabulary is easier for stream-json clients to consume than replay-only wrappers, and additive `StreamRecord` variants preserve existing producers and records.
- **Top-level `cake replay <uuid>` subcommand (chosen).** Clean argument validation, no conflict with the coding-assistant flags, and the use case is primary rather than diagnostic.

## Decision Outcome

Chosen option: a top-level `cake replay <uuid>` subcommand that emits the session transcript as stream-json and exits `0`. The command requires `--output-format stream-json`; any other format is an input error. Replay opens the session file read-only (no advisory lock, no append), parses it with the same record vocabulary as the persisted format, and re-emits every record as the matching `StreamRecord`.

`StreamRecord` gains additive variants for the persisted metadata kinds replay must surface: `session_meta`, `prompt_context`, and `skill_activated`. Live streams never emit them; replay emits them interleaved with the conversation records in their original order. `StreamRecord` also gains a `replay_error` variant that replay emits before exiting non-zero, carrying a machine-readable `kind`, a human-readable `error`, the affected `session_id` when known, and the accompanying `exit_code`.

### Error Protocol

  | Failure                                | `replay_error` kind  | Exit code         |
  | -------------------------------------- | -------------------- | ----------------- |
  | `--output-format` is not `stream-json` | `output_format`      | `3` (input error) |
  | Invalid session UUID                   | `invalid_uuid`       | `3` (input error) |
  | Session not found                      | `session_not_found`  | `3` (input error) |
  | Unreadable or corrupt session file     | `corrupt`            | `1` (agent error) |
  | Unsupported format version             | `unsupported_format` | `1` (agent error) |
  | Permission denied opening the file     | `permission`         | `1` (agent error) |

The typed `ReplayError` drives both the event's `exit_code` field and the process exit code through the existing exit-code classifier, so the two channels cannot diverge.

### Consequences

- **Positive**: Clients can hydrate full multi-task history from real session data at zero LLM cost, and can integration-test stream parsing against a full transcript.
- **Positive**: Replay is provably side-effect-free: the read path never locks, appends, mutates, or touches session discovery.
- **Positive**: The stream vocabulary stays single-typed; additive variants preserve existing producers and records.
- **Negative**: `replay` is reserved as a top-level subcommand name.
- **Negative**: Live stream output and replay output now share `session_meta` as a record name; the distinction is that replay includes it and live streams never do, documented in the integrations contract.

## More Information

- Issue 59: Add session transcript replay/export for stream-json clients.
- [Integration contracts](../integrations.md), the Stream JSON and Persisted Sessions sections.
- `src/cli/replay.rs`, the command and error protocol authority.
- `src/types/session.rs`, the `StreamRecord` vocabulary authority.

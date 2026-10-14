---
status: accepted
date: 2026-10-13
decision-makers: Travis Ennis
---

# Report run state with the OSC 7501 program status protocol

## Context and Problem Statement

Cake runs long agent tasks while the user is elsewhere. A terminal cannot learn cake's state from the pty: tools that show many programs at once (Herdr, cmux, Agent Deck) classify an agent by heuristics --- regex over window titles or spinners --- or by a per-tool socket integration.

[OSC 7501, the Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status) (revision 0.4 at the time of this decision) is a terminal escape sequence by which a program reports what it is doing. `state` is `idle`, `working`, `done`, `blocked`, or `error`, and `clear` removes a record. Optional keys carry `kind` (`permission`, `question`, `auth`), `progress`, `app`, `title`, and a base64 single-line `msg`. Ghostty (through libghostty) and Rex implement the consumer side; the protocol rides the pty, so it works over SSH and in containers, and terminals that do not know it ignore it.

The decision needed is whether cake reports its own state, on which channels, and behind which gate.

## Decision Drivers

- A consumer must learn cake's real state without a manifest, a per-tool integration, or title heuristics.
- Machine-readable stdout is a compatibility surface: completion JSON, stream JSON, and exit behavior must stay byte-identical.
- The protocol is young (0.1 to 0.4 within a month). Binding an unstable spec to a settings key would turn every revision into a configuration compatibility question.
- The sequence is inert where unsupported, so emitting it unconditionally costs nothing in a terminal that ignores it.

## Considered Options

- Report unconditionally, gated only on stderr being a terminal (chosen).
- Report only when a setting enables it. Rejected: adds a configuration key for behavior that is inert when unsupported, and freezes a moving spec into cake's configuration shape.
- Do nothing and leave classification to heuristics. Rejected: cake knows its own state exactly, and consumers already implement the protocol.

## Decision Outcome

Chosen: unconditional, TTY-gated reporting, because the sequence is inert where unsupported and no setting should outlive the spec's shape.

Concretely:

- A new `src/program_status.rs` module formats and writes root-record reports (no `id`; cake reports one record for itself). Every report carries `app=cake`.
- Reporting goes to stderr only, and only when stderr is a terminal. Nothing is written when stderr is a pipe, so capture tools, `2>file`, and CI transcripts never see a sequence.
- Sequences never enter stdout, session records, telemetry, hooks, or the model-visible conversation.
- States map to the run lifecycle: `working` when the agent turn starts and when a wait for piped input ends, `done` when the turn produced a result, `error` with the sink-safe error text when a turn that began failed, `idle` when the user interrupts, and `blocked` with `kind=question` while cake waits for a prompt on piped stdin.
- A failure before the agent turn (input validation, configuration, worktree setup) leaves no final record: the process exits with the error on stderr, and the terminal drops the transient record it was showing.
- `msg` values are normalized to a single line (whitespace and control characters collapse to spaces), truncated to the protocol's 2048-byte decoded limit, and base64-encoded.
- There is no `clear` report. A `working` or `blocked` record's lifetime already ends at process exit, and `done` and `error` are meant to survive it so the user finds the outcome --- which is the point of reporting at all. Cake exits as soon as it finishes, so it never owns a stale record to remove.
- Out of scope: feature detection (`OSC 7501;?` replies are not read) and consuming reports from child processes cake runs through the Bash tool.

### Security and Compatibility Impact

The change adds no filesystem, network, or command-execution surface, and no new dependency (base64 is already in the tree). It writes only to cake's own stderr. The sequence is not model-visible, cannot reach a session record or a stream-json event, and cannot change an exit code: a failed write is ignored. Terminal-side consumers must treat `msg` as untrusted text, which the spec already requires of them. Reporting on interrupt or failure does not change exit behavior. Because records escape to the user's terminal, `msg` carries the same sink-safe error text the run already prints on stderr, which excludes assistant text for limit-exceeded errors.

### Consequences

- Good, because any OSC 7501 consumer shows cake's live state with no per-tool integration, over SSH and in containers.
- Good, because the feature is inert without a consumer, so no setting is added and no configuration shape is created.
- Bad, because the protocol is experimental and may change; cake follows the spec, and a later revision can move under a consumer. Accepted: consuming the sequence is optional and `msg` carries nothing the run does not already print.
- Bad, because the TTY check is the only gate, so a user who wants no sequence must hide stderr (for example `2>/dev/null`). Accepted for an inert sequence.
- Accepted limitation: cake reports the root record only. Per-id records exist in the protocol and stay unused until cake has concurrent work to report.

## More Information

- Spec: [Program Status Protocol (OSC 7501)](https://www.superlogical.com/rex/docs/build/program-status).
- Announcement: [Program status protocol](https://mitchellh.com/writing/program-status-osc7501).
- Task 107, migrated from [#707](https://github.com/travisennis/cake/issues/707), carries the acceptance criteria.

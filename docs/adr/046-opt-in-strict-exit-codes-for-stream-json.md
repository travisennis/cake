---
status: accepted
date: 2026-10-15
decision-makers: Travis Ennis
---

# Opt-in strict exit codes for stream-json runs

## Context and Problem Statement

`--output-format stream-json` reports a task's outcome in its final `task_complete` record, and the process exits `0` even when that record carries an agent, provider, or tool failure or a limit outcome. That is the documented contract ([Integrations](../integrations.md)), and text and JSON consumers depend on the same swallow only insofar as their own modes already exit nonzero.

The consequence is that a pipeline cannot act on `$?`. A `cake --output-format stream-json ... | jq ...` step treats a failed task as success unless the consumer parses the records, which makes the exit code useless exactly where a machine reads the stream.

Changing the default would break every existing stream-json consumer, so the strict behavior must be chosen.

## Considered Options

- Add an opt-in `--fail-on-error` flag that returns the documented exit class of the failure (chosen).
- Return exit `1` for any failed `task_complete`. Rejected: the failure classes in [Integrations](../integrations.md) are the machine contract, and collapsing an authentication, rate-limit, or network failure into `1` would contradict the documented meaning of `2` that sibling work is making precise.
- Make stream-json exit nonzero by default. Rejected: a compatibility break for every existing consumer, against a contract that names exit `0` as the in-stream outcome.
- Leave the exit code alone and tell consumers to parse `task_complete`. Rejected: the shell channel stays unusable, and `set -e` pipelines keep failing open.

## Decision Outcome

Chosen: an opt-in `--fail-on-error` root agent-run flag, because it gives machine consumers a reliable `$?` without moving any existing behavior.

Concretely:

- The flag is a `bool` on the root run, accepted for any output format but meaningful only for `stream-json`; text and JSON already exit nonzero on a failed run. It is rejected with exit `3` when combined with a subcommand, like `--output-schema`.
- Without the flag, behavior is unchanged byte for byte: an in-stream failure exits `0`, a broken pipe exits `0`, and an interruption exits `130`.
- With the flag, the run's final outcome decides the status. `CliOutputSink` propagates the turn error instead of swallowing it, and `finish_run` --- the single classifier that already serves every output mode --- maps it: `1` for an agent, tool, cut-off, or limit outcome and for a provider failure that is none of the classes below (`5xx`, for example), `2` for an authentication, rate-limit, or network failure, and `3` for an input error. No second exit-code path is introduced.
- Broken-pipe cancellation still exits `0`, because `finish_run` maps the closed-output error to success, and interruption still exits `130` on the interrupt path that precedes rendering.
- The stream is unchanged in both modes: the final `task_complete` record remains the outcome channel, so a consumer can observe the failure and the exit code from one run.

### Security and Compatibility Impact

No new filesystem, network, or command-execution surface, and no new dependency. Exit behavior is a documented compatibility surface: the default is untouched, so no existing consumer changes behavior. A consumer that opts in reads a nonzero exit where it previously read `0`, which is the requested change. The flag cannot turn a successful run into a failure, and it cannot suppress a failure that already exits nonzero.

### Consequences

- Good, because `cake --fail-on-error --output-format stream-json` composes with `set -e`, `&&`, and CI steps that check only `$?`.
- Good, because the strict exit code carries the failure class, so a consumer can distinguish a retryable network failure (`2`) from a broken task (`1`) without parsing records.
- Good, because the swallow stays the default, so the change is additive for every existing consumer.
- Bad, because the flag is accepted but inert for text and JSON output. Accepted: rejecting it would add an input-error path whose only effect is to tell a user that the mode they chose already does what they asked.
- Accepted limitation: only an opt-in run gets the strict contract, so a consumer that never passes the flag keeps the failure-open pipeline this decision set out to fix.

## More Information

- Task 047 carries the acceptance criteria, migrated from [#433](https://github.com/travisennis/cake/issues/433).
- [Integrations](../integrations.md) is the living contract for exit codes; this record explains why an opt-in flag was chosen over changing the default.

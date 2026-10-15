---
status: accepted
date: 2026-10-15
decision-makers: Travis Ennis
---

# Serialize same-path reads with same-path mutations

## Context and Problem Statement

[ADR-013](013-per-path-serialization-of-mutating-tool-calls.md) groups mutating tool calls by canonical target path, but `Read` declares no target and stays a singleton that runs concurrently with a same-path `Edit` or `Write`. Results are still re-emitted in the model's issue order, so the transcript can present a read that appears to follow a mutation while having observed bytes from before it. `Read` has no way to know it raced: no prompt describes scheduling (ADR-013 deliberately removed that guidance), and `edit-description.txt` tells the model that "each successful edit invalidates any earlier read of that file", so a stale read presented later in the transcript contradicts the stated contract rather than teaching anything.

Session-corpus evidence for task 057: the pattern is rare --- 2 turns in \~92k tool-call turns across 2,205 persisted sessions --- and neither observed turn produced a wrong outcome, but the failure it can produce is invisible and unresolvable to the model. The same review found `ScheduledToolCall::target` promising the group's path on every serialized member while `schedule_tool_calls` used `target.take()`, leaving group leaders without it (latent today because only followers are read, for the compensation telemetry event).

## Decision Drivers

- Transcript truthfulness: a call's recorded result should reflect the state implied by its issue-order position, not by whichever future won a race.
- Mechanism over judgment ([ADR-013](013-per-path-serialization-of-mutating-tool-calls.md)): fix the harness's concurrency instead of adding prompt rules or guards the model must remember.
- Preserve independent-read concurrency, result ordering, per-call attribution, and the session and stream-json compatibility surfaces documented in [Integrations](../integrations.md).
- Keep the cost proportional to the race: only same-path calls need ordering.

## Considered Options

- Serialize a read with same-path mutations in issue order when the turn mutates that path.
- Make every read observe turn-start state regardless of issue order.
- Leave reads unserialized and document that same-turn read results are racy.
- Serialize every call in a turn (cancelled task 202, rejected in ADR-013).

## Decision Outcome

Chosen option: serialize a read with same-path mutations in issue order when the turn mutates that path, because it fixes the observation race without constraining model batching and without reducing concurrency anywhere the race cannot occur.

The concrete semantics:

- A turn's executable calls that resolve a canonical path share that path's scheduling group whenever the turn also mutates the path, in issue order: a `Read` issued before a same-path `Edit` or `Write` observes the pre-mutation state, and one issued after observes the mutation's effects. `ReadImage` declares the same observing target as `Read`.
- Reads of paths the turn does not mutate stay independent singleton groups, so repeated reads and reads of different paths keep running concurrently, as do mutations of distinct paths.
- Registration declares the target: `mutates_path` (Edit, Write) and `observes_path` (Read, ReadImage) record a resolver plus how the call touches the path (`PathTouch::Mutate` / `PathTouch::Observe`), and `ToolRegistry::path_target` serves scheduling. Name matching is still not used.
- `resolve_path_for_read_scheduling` resolves not-yet-existing files to the same key as `resolve_path_for_write_scheduling`, so a `Read` and a same-turn `Write` of a file that does not exist yet serialize instead of racing on whether the file exists.
- Every member of a serialized group retains the group's canonical path in `ScheduledToolCall::target`, leader included; the leader's metadata is no longer dropped when the group is created.
- Hook-blocked calls, calls whose arguments or paths cannot be resolved, Bash, and toolbox tools remain unserialized, exactly as in ADR-013; a blocked or failed call does not abort later calls in its group.

### Consequences

- Good, because a same-turn read can no longer present stale bytes for a path the turn mutated, so the transcript stays truthful.
- Good, because no prompt text, guard, or rejection was added; the mechanism is the whole fix.
- Good, because independent reads and unrelated mutations keep their concurrency, and result ordering, per-call attribution, and session and stream records are unchanged; a serialized read emits the existing `same_path_serialization` compensation event, with no new record shape.
- Bad, because a turn that both reads and mutates one path loses that read's concurrency with the mutation, and two reads of such a path serialize with each other; the cost is bounded to the same-path group.
- Neutral, because Bash commands that read or write a path remain unserialized against in-process calls for it; this predates the decision and stays out of scope.

## More Information

- Partially supersedes [ADR-013](013-per-path-serialization-of-mutating-tool-calls.md): its "groups run concurrently with each other and with all other calls" clause no longer covers reads of paths the turn mutates. ADR-013's mutation-only grouping remains accepted.
- Task 057: Make same-path scheduling serialize reads with mutations and preserve targets (source findings A1 and A2; GitHub issue 460).
- Key code: `src/clients/tools/scheduling.rs` (grouping), `src/clients/tools/mod.rs` (`PathTouch`, `ToolEntry::observes_path`, `ToolRegistry::path_target`, `resolve_path_for_read_scheduling`), `src/clients/tools/read.rs` and `read_image.rs` (path targets), `src/clients/agent/agent_loop.rs` (`Agent::run_tool_plans`).
- Current boundary: [ARCHITECTURE.md](../../ARCHITECTURE.md); implementation and tests: `src/clients/tools/scheduling.rs`.

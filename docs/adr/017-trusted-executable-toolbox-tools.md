---
status: accepted
date: 2026-07-14
decision-makers: Travis Ennis
---

# Trusted Executable Toolbox Tools

## Context and Problem Statement

Cake's built-in tools have fixed schemas and execution paths. Users need a way to add local tools without recompiling cake, while provider-facing schemas, read-only guarantees, subprocess lifetimes, and the trust boundary remain explicit and enforceable.

Toolbox discovery executes user-provided files during both description and invocation. Those processes are not wrapped in the Bash tool's Seatbelt or Landlock sandbox, and malformed descriptions can invalidate a provider request or advertise calls the execution protocol cannot encode.

## Decision Drivers

- Extend the existing `ToolRegistry` rather than add a second dispatch path.
- Keep provider-facing tool names and JSON Schemas valid.
- Make discovery deterministic and broken tools non-fatal.
- Bound subprocess time and output, including descendant processes.
- Preserve the `read-only` policy's no-mutation guarantee.
- Support JSON and line-based text protocols compatible with existing toolbox scripts.

## Considered Options

- Register trusted executable tools as ordinary `ToolEntry` values, with strict discovery-time validation and no registration under `read-only`.
- Run toolbox executables inside the Bash sandbox.
- Add a separate toolbox registry and `tb__*` fallback dispatcher.
- Do not support executable extensions.

## Decision Outcome

Chosen option: register trusted executable tools as ordinary `ToolEntry` values. Cake discovers executables from configured toolbox directories, runs `TOOLBOX_ACTION=describe`, validates and normalizes their top-level object schema, prefixes registered names with `tb__`, and captures each executable's state in its registry executor closure.

Toolbox processes intentionally run outside cake's Bash sandbox. Cake therefore skips discovery and registration entirely under `SandboxPolicy::ReadOnly`, including the describe action. Under other policies, users are responsible for trusting configured toolbox executables.

Each describe or execute invocation starts in its own Unix process group. Timeout and output-cap failures terminate the entire group. Stdout and stderr are bounded. Text arguments reject names or values that the `key=value` line protocol cannot encode without changing record structure.

### Consequences

- Good, because toolbox calls use the same provider registration, hooks, scheduling, transcript, and result path as built-in tools.
- Good, because malformed or unencodable descriptions are skipped before they can invalidate a provider request or advertise unusable tools.
- Good, because runaway descendants cannot continue after cake reports a timeout or output-cap failure.
- Good, because `read-only` continues to mean that the agent is not offered unsandboxed mutation paths.
- Bad, because toolbox executables have the user's ambient filesystem and network authority under `workspace-write` and `danger-full-access`; directory configuration is a trust decision.
- Bad, because the text protocol cannot represent multiline values and cake rejects them instead of silently changing their structure.
- Neutral, because toolbox calls receive singleton scheduling groups: cake cannot infer their mutation targets for per-path serialization.

## More Information

- Task 121: Implement User-Defined Toolbox Tools.
- ExecPlan: `.agents/exec-plans/active/toolboxes-plan.md`.
- `docs/design-docs/tools.md` (Toolbox Tools).
- `docs/design-docs/sandbox.md` (Toolbox trust boundary).
- ADR-013: Per-Path Serialization of Mutating Tool Calls.
- ADR-014: Sandbox Policy CLI Flag.

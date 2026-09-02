---
status: accepted
date: 2026-09-01
decision-makers: Travis Ennis
informed: issue 347
---

# Tool Replay Safety Declarations

## Context and Problem Statement

An interrupted Cake task can persist a model `function_call` without its `function_call_output`. Recovery currently closes that gap with a synthetic `not executed` output. A future recovery path may safely re-execute some read-only calls, but it needs a durable declaration from the tool registry and must not infer safety from a tool name or from a later version of the tool.

## Decision Drivers

- Tool replay must fail closed: an omitted or unknown declaration must mean `never`.
- The declaration must describe the registered executor, not a name-based convention.
- Persisted sessions must retain the declaration that was in force when Cake handled the call, while old records remain readable.
- Toolbox executables are trusted extensions, so they remain `never` unless their describe manifest explicitly declares `safe`.
- This change must not re-execute interrupted calls; it only supplies the metadata a later recovery change can consult.

## Considered Options

- **Infer safety from tool names or mutation scheduling:** Rejected. Names and same-path mutation metadata do not describe all side effects, especially for Bash and external toolbox processes.
- **Persist a boolean or a required field:** Rejected. A boolean hides the fail-closed vocabulary, and a required field would make historical sessions incompatible.
- **Typed registry declaration with an additive optional record field (chosen):** `safe` and `never` are represented by a shared enum. Registry entries default to `never`; live function-call and function-call-output records optionally carry the snapshot, preserving older JSONL records.

## Decision Outcome

Each registered tool carries a typed replay declaration with the values `safe` or `never`. The default for new entries and unknown names is `never`. The built-in Read tool declares `safe`; Bash, Edit, and Write declare `never`. A toolbox describe manifest may use `replay: "safe"` or `replay: "never"` in JSON, or the corresponding `replay: safe`/`replay: never` line in text; omission means `never`.

Cake adds an optional `replay` field to the persisted and stream-visible function-call and function-call-output records. The field is the registry snapshot used while handling the call and is omitted from historical records and synthetic recovery outputs. A future automatic replay must require both the persisted snapshot and the current registry declaration to be `safe`. Current continue, resume, and fork repair remains append-only and continues to write synthetic `not executed` outputs.

### Consequences

- **Positive:** Future recovery can make a version-aware, fail-closed replay decision without guessing from tool names or mutating old session bytes.
- **Positive:** Existing session consumers remain compatible because the new record fields are optional and old records still deserialize.
- **Negative:** The declaration is a safety contract supplied by a toolbox executable's manifest; Cake cannot independently prove that an external executable is side-effect-free.
- **Negative:** This release records metadata but does not reduce interruption recovery work or replay any calls.

## More Information

- Issue 347: Declare tool replay safety and persist it beside tool outputs.
- [Integration contracts](../integrations.md), persisted sessions and toolbox protocol.
- `src/clients/tools/mod.rs`, the registry declaration authority.
- `src/config/toolbox.rs`, the toolbox describe-manifest parser.
- `src/types/session.rs`, the persisted record field authority.

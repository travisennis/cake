# Agent Loop, Tools, And Tool Execution

## Scope

Read this before changing agent loop control flow, tool definitions, tool-call concurrency, tool dispatch, tool result handling, transcript construction, or conversation item translation.

## Compatibility Surfaces

- Tool JSON schemas and tool names.
- Tool execution semantics, including path validation and error strings visible to the model.
- `ConversationItem` representation and backend translation behavior.
- Transcript ordering and tool-call/result pairing.

## Required Checks

- Add or update focused tests for changed tool behavior or conversation translation.
- Run snapshot tests when serialized conversation, prompt, request, or output records change.
- Use `cargo test <module_or_test_name>` first, then follow [CONTRIBUTING.md](../../CONTRIBUTING.md) for final verification.

## Common Failure Modes

- Changing a tool schema without updating backend request construction.
- Returning tool results in an order that no longer matches model tool calls.
- Treating Chat Completions and Responses API conversation translation as interchangeable.
- Letting path validation drift between Read, Edit, Write, and Bash.

## Related Docs

- [ARCHITECTURE.md](../../ARCHITECTURE.md)
- [tools.md](../design-docs/tools.md)
- [conversation-types.md](../design-docs/conversation-types.md)
- [session-management.md](../design-docs/session-management.md)

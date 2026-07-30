---
status: accepted
date: 2024-01-15
---

# Agent Loop Architecture

## Context

The agent needs a mechanism to interact with an LLM API, execute tools based on model responses, and continue until a final response is generated. We evaluated several approaches for orchestrating this loop.

## Decision

We use an iterative agent loop with tool execution:

1. Send messages to the LLM
2. Receive response
3. If response contains tool calls, execute them and append results
4. If response is a final message, return it
5. If a successful response has no final message and is not explicitly non-retryable, append one final-answer-only continuation with tools disabled
6. Repeat from step 1

The incomplete-turn continuation is allowed once per user invocation. It remains in the same session and task, and its provider turn, usage, conversation records, and telemetry accumulate normally. A second incomplete response returns the existing cut-off outcome rather than extending the loop again.

## Rationale

- **Simplicity**: Clear, predictable control flow
- **Debuggability**: Each step is observable
- **Extensibility**: New tools are easily added
- **Resumability**: Sessions can be persisted between steps

## Consequences

- **Positive**: Minimal abstraction overhead, easy to trace execution
- **Positive**: Bounded recovery preserves context when a provider stops after partial reasoning
- **Negative**: Higher latency due to sequential tool execution
- **Negative**: Requires careful handling of tool call limits to prevent infinite loops
- **Negative**: An incomplete provider response can add one extra request and final-answer latency

## Alternatives Considered

- **Batch tool execution**: Execute all tool calls in parallel. Rejected because tool dependencies require sequential execution in many cases.
- **Streaming with immediate execution**: Stream responses and execute tools as they arrive. Rejected due to complexity in handling partial tool call data.

## References

- OpenAI Tool Use documentation
- Anthropic Model Context Protocol

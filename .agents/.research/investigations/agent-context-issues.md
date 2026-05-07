# Agent Context Issues

## Summary

Analysis of session `7d2665fd-c396-4e0c-8cf3-37756d0bcccf` revealed that the agent (using `glm-5` model) started with irrelevant tool calls before orienting to the actual task and project.

## Observed Behavior

### Session Task
The user requested: "when using the --verbose flag, acai should output when it is thinking and showing thinking/reasoning blocks..."

### First Tool Calls Made
1. `find /home -name "*.py" -type f 2>/dev/null | head -20` - Looking for Python files in `/home`
2. `ls -la /home/` - Listing the `/home` directory
3. `pwd && ls -la` - Finally checking the actual working directory

### Problems Identified

| Issue | Description |
|-------|-------------|
| Wrong language | Searched for Python files (`.py`) when this is a Rust project |
| Wrong directory | Searched in `/home` instead of the working directory `/Users/travisennis/Projects/acai` |
| Wrong platform | `/home` doesn't exist on macOS (uses `/Users`) |
| Sandbox error | Second call hit sandbox restriction: `Operation not permitted` |

## Hypothesis

**The agent is not properly loading or reading the system prompt that includes AGENTS.md context.**

The system prompt should provide:
1. Project type identification (Rust project)
2. Working directory context
3. Available tools and their usage patterns
4. Project-specific context from AGENTS.md

### Evidence Supporting Hypothesis

1. **No awareness of project type**: A properly initialized agent would know this is a Rust project from `Cargo.toml` in the working directory

2. **No awareness of working directory**: The system prompt includes the working directory path, yet the agent searched in `/home`

3. **No awareness of platform**: The sandbox configuration and working directory clearly indicate macOS, yet the agent used Linux-style paths

4. **Third tool call succeeded**: Only after the sandbox error did the agent call `pwd && ls -la` to discover its actual context

5. **Model recovered**: After discovering the working directory, the agent proceeded correctly with:
   - `find src -name "*.rs"` - Correct for Rust project
   - Read appropriate source files
   - Made correct edits to implement the feature

## Root Cause Analysis

The `glm-5` model appears to either:

1. **Not receive the system prompt** - The system prompt with AGENTS.md context may not be included in the request

2. **Not process the system prompt** - The model may receive it but not use it to inform initial actions

3. **Have poor context utilization** - The model may have received the context but has difficulty applying it before taking action

## Recommended Investigation

### 1. Verify System Prompt Construction

Check that the system prompt is being built correctly in the agent initialization:

```rust
// In src/clients/agent.rs
pub fn new(config: ResolvedModelConfig, system_prompt: &str) -> Self {
    Self {
        config,
        history: vec![ConversationItem::Message {
            role: Role::System,
            content: system_prompt.to_string(),
            // ...
        }],
        // ...
    }
}
```

### 2. Verify System Prompt Content

The system prompt should include:
- Project context from AGENTS.md files
- Working directory path
- Available tools and their descriptions
- Platform-specific information

### 3. Check API Request Construction

Verify the system message is included in API requests:

```rust
// In src/clients/responses.rs or src/clients/chat_completions.rs
// Ensure system message is first in the input array
```

### 4. Add Logging

Add debug logging to capture:
- The full system prompt content
- Whether it's included in API requests
- The model's first response before any tool calls

### 5. Model-Specific Testing

Test with different models to determine if this is:
- A `glm-5` specific issue (model doesn't follow system prompts well)
- A general issue affecting all models
- An intermittent issue

## Potential Fixes

### Short Term
1. Add explicit working directory context to the first user message
2. Include a "start by understanding your environment" instruction in the system prompt
3. Log the full system prompt for debugging

### Long Term
1. Evaluate model performance on system prompt adherence
2. Consider adding a "context check" step before the agent starts work
3. Add metrics to track how often agents make context-unaware first actions

## Session Timeline

| Time | Event |
|------|-------|
| 11:08:04 | Initial API request |
| 11:08:18 | Model response with tool calls (14s gap - slow model response) |
| 11:08:20 | First tool execution: `find /home -name "*.py"` - empty result |
| 11:08:20 | Second tool execution: `ls -la /home/` - sandbox error |
| 11:08:26 | Third API request (6s gap) |
| 11:08:30 | `pwd && ls -la` executed - agent now oriented |
| ... | Agent proceeds correctly with Rust project exploration |

## Model Performance Notes

The `glm-5` model showed:
- **Slow response times**: 6-14 seconds between API requests
- **Poor initial context awareness**: Made irrelevant tool calls first
- **Good recovery**: Once oriented, completed the task correctly
- **Successful implementation**: Tests pass, feature works as expected

## Related Files

- `src/clients/agent.rs` - Agent initialization and system prompt handling
- `src/prompts/` - System prompt construction
- `src/clients/responses.rs` - API request building for Responses API
- `src/clients/chat_completions.rs` - API request building for Chat Completions API
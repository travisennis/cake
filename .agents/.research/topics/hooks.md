# Hooks

Hooks is a feature we are considering adding to cake. We are currently in the research phase.

## Research Findings

This document summarizes hooks implementations across four major AI coding assistants: Claude Code, OpenAI Codex, Cursor, and GitHub Copilot. Each system takes a slightly different approach, but there are common patterns that can inform cake's implementation.

---

## Executive Summary

All four systems share a common architecture:

1. **JSON-based configuration** - Hooks are defined in JSON files at user, project, and/or system levels
2. **Event-driven lifecycle** - Hooks fire at specific points: session start/end, before/after tool execution, etc.
3. **stdio communication** - Hook scripts receive JSON input via stdin and return decisions via stdout
4. **Matcher/filter system** - Hooks can be filtered by tool name, event type, or custom conditions (except GitHub Copilot which handles filtering in scripts)
5. **Exit code semantics** - Exit 0 = success, Exit 2 = block (universally), other codes = non-blocking error

Claude Code has the most comprehensive implementation with 22+ hook events, 4 hook types (command, HTTP, prompt, agent), and rich decision control. GitHub Copilot has a focused implementation with cross-platform support (bash + PowerShell) and a unique `errorOccurred` hook. Cursor and Codex have simpler implementations focused on core use cases.

---

## Comparison Matrix

### Hook Events Supported

| Event | Claude Code | Codex | Cursor | GitHub Copilot |
|-------|:-----------:|:-----:|:------:|:--------------:|
| SessionStart | ✅ | ✅ | ✅ | ✅ |
| SessionEnd | ✅ | - | ✅ | ✅ |
| UserPromptSubmit | ✅ | ✅ | ✅ (beforeSubmitPrompt) | ✅ |
| PreToolUse | ✅ | ✅ | ✅ (preToolUse) | ✅ |
| PostToolUse | ✅ | ✅ | ✅ | ✅ |
| PostToolUseFailure | ✅ | - | ✅ | - |
| PermissionRequest | ✅ | - | - | - |
| Notification | ✅ | - | - | - |
| SubagentStart | ✅ | - | ✅ | - |
| SubagentStop | ✅ | - | ✅ | ✅ |
| Stop | ✅ | ✅ | ✅ | ✅ (agentStop) |
| StopFailure | ✅ | - | - | - |
| PreCompact | ✅ | - | ✅ | - |
| PostCompact | ✅ | - | - | - |
| ConfigChange | ✅ | - | - | - |
| CwdChanged | ✅ | - | - | - |
| FileChanged | ✅ | - | - | - |
| InstructionsLoaded | ✅ | - | - | - |
| TaskCreated | ✅ | - | - | - |
| TaskCompleted | ✅ | - | - | - |
| TeammateIdle | ✅ | - | - | - |
| Elicitation | ✅ | - | - | - |
| ElicitationResult | ✅ | - | - | - |
| WorktreeCreate | ✅ | - | - | - |
| WorktreeRemove | ✅ | - | - | - |
| ErrorOccurred | - | - | - | ✅ |
| beforeShellExecution | - | - | ✅ | - |
| afterShellExecution | - | - | ✅ | - |
| beforeMCPExecution | - | - | ✅ | - |
| afterMCPExecution | - | - | ✅ | - |
| beforeReadFile | - | - | ✅ | - |
| afterFileEdit | - | - | ✅ | - |
| afterAgentResponse | - | - | ✅ | - |
| afterAgentThought | - | - | ✅ | - |
| beforeTabFileRead | - | - | ✅ | - |
| afterTabFileEdit | - | - | ✅ | - |

### Hook Types Supported

| Type | Claude Code | Codex | Cursor | GitHub Copilot |
|------|:-----------:|:-----:|:------:|:--------------:|
| Command | ✅ | ✅ | ✅ | ✅ |
| HTTP | ✅ | - | - | - |
| Prompt (LLM-evaluated) | ✅ | - | ✅ | - |
| Agent (multi-turn with tools) | ✅ | - | - | - |

### Configuration Locations

| Location | Claude Code | Codex | Cursor | GitHub Copilot |
|----------|:-----------:|:-----:|:------:|:--------------:|
| User-level | `~/.claude/settings.json` | `~/.codex/hooks.json` | `~/.cursor/hooks.json` | - |
| Project-level | `.claude/settings.json` | `<repo>/.codex/hooks.json` | `<project>/.cursor/hooks.json` | `.github/hooks/*.json` |
| Local (gitignored) | `.claude/settings.local.json` | - | - | - |
| Enterprise/MDM | Managed policy settings | - | ✅ (system paths) | - |
| Team cloud sync | - | - | ✅ (Enterprise) | - |
| Plugin-based | `hooks/hooks.json` in plugins | - | - | - |
| Skill/Agent frontmatter | YAML frontmatter | - | - | - |

### Decision Control Capabilities

| Capability | Claude Code | Codex | Cursor | GitHub Copilot |
|------------|:-----------:|:-----:|:------:|:--------------:|
| Allow tool execution | ✅ | ✅ | ✅ | ✅ |
| Deny/block tool execution | ✅ | ✅ | ✅ | ✅ |
| Ask user for confirmation | ✅ | - | ✅ | ✅ (parsed but not processed) |
| Modify tool input before execution | ✅ | Parsed but unsupported | ✅ | - |
| Inject additional context | ✅ | ✅ | ✅ | - |
| Modify tool output | ✅ (MCP tools) | Parsed but unsupported | ✅ | - |
| Stop session entirely | ✅ | ✅ | - | - |
| Auto-continue with follow-up | ✅ (Stop hook) | ✅ (Stop hook) | ✅ (stop hook) | - |
| Modify permissions/rules | ✅ | - | - | - |

---

## Detailed Source Analysis

### 1. Claude Code Hooks

Claude Code has the most mature and feature-rich hooks implementation.

#### Configuration Format

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/script.sh",
            "timeout": 600,
            "statusMessage": "Checking command...",
            "if": "Bash(rm *)"
          }
        ]
      }
    ]
  }
}
```

#### Key Features

- **4 hook types**: `command`, `http`, `prompt`, `agent`
- **22+ hook events** covering the full agent lifecycle
- **Rich matcher system**: regex patterns on tool names, event-specific fields
- **`if` conditions**: Permission rule syntax for fine-grained filtering (e.g., `Bash(rm *)`)
- **Async hooks**: Run in background without blocking
- **Environment persistence**: `CLAUDE_ENV_FILE` for SessionStart, CwdChanged, FileChanged
- **MCP tool support**: Match MCP tools with `mcp__<server>__<tool>` pattern
- **Nested configuration**: matcher groups with multiple handlers

#### Input Schema (Common Fields)

```json
{
  "session_id": "abc123",
  "transcript_path": "/path/to/transcript.jsonl",
  "cwd": "/current/working/directory",
  "permission_mode": "default",
  "hook_event_name": "PreToolUse",
  "agent_id": "optional-subagent-id",
  "agent_type": "optional-agent-name"
}
```

#### Output Schema

**Exit code semantics:**
- `0` = success, parse stdout for JSON
- `2` = blocking error, stderr shown to Claude
- Other = non-blocking error, execution continues

**JSON output fields:**
```json
{
  "continue": true,
  "stopReason": "optional message",
  "suppressOutput": false,
  "systemMessage": "warning message",
  "decision": "block",
  "reason": "explanation",
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "allow|deny|ask",
    "permissionDecisionReason": "reason",
    "updatedInput": { "modified": "parameters" },
    "additionalContext": "context for Claude"
  }
}
```

#### PreToolUse Decision Control

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Destructive command blocked",
    "updatedInput": { "command": "safer alternative" },
    "additionalContext": "Context for Claude"
  }
}
```

#### PermissionRequest Decision Control

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PermissionRequest",
    "decision": {
      "behavior": "allow",
      "updatedInput": { "command": "npm run lint" },
      "updatedPermissions": [
        { "type": "setMode", "mode": "acceptEdits", "destination": "session" }
      ]
    }
  }
}
```

#### Best Practices (Claude Code)

1. Keep SessionStart hooks fast (run on every session)
2. Use `if` conditions to avoid unnecessary process spawns
3. Reference scripts with `$CLAUDE_PROJECT_DIR` for portability
4. Use `async: true` for non-blocking side effects
5. Return structured JSON for fine-grained control (vs exit codes)
6. HTTP hooks are non-blocking by default (fail-open)

---

### 2. OpenAI Codex Hooks

Codex has a simpler, experimental hooks implementation.

#### Configuration Format

```json
{
  "hooks": {
    "SessionStart": [
      {
        "matcher": "startup|resume",
        "hooks": [
          {
            "type": "command",
            "command": "python3 ~/.codex/hooks/session_start.py",
            "statusMessage": "Loading session notes",
            "timeout": 600
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/script.py"
          }
        ]
      }
    ]
  }
}
```

#### Key Features

- **Experimental status**: Behind feature flag `codex_hooks = true` in config.toml
- **4 main events**: SessionStart, PreToolUse, PostToolUse, UserPromptSubmit, Stop
- **Bash-only tool interception**: Currently only `Bash` tool is supported
- **Concurrent execution**: Multiple matching hooks run in parallel
- **Fail-open by default**: Non-blocking errors allow execution to continue

#### Input Schema

```json
{
  "session_id": "string",
  "transcript_path": "string | null",
  "cwd": "string",
  "hook_event_name": "string",
  "model": "string",
  "turn_id": "string (turn-scoped hooks)"
}
```

#### Output Schema

```json
{
  "continue": true,
  "stopReason": "optional",
  "systemMessage": "optional",
  "suppressOutput": false,
  "decision": "block",
  "reason": "explanation",
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "reason",
    "additionalContext": "context"
  }
}
```

#### Limitations

- Only Bash tool is intercepted (can be worked around by writing scripts to disk)
- Windows support temporarily disabled
- `updatedInput`, `additionalContext` parsed but not implemented
- `permissionDecision: "allow"` and `"ask"` not yet supported

#### Best Practices (Codex)

1. Use git-root-relative paths for repo-local hooks: `$(git rev-parse --show-toplevel)/.codex/hooks/`
2. Codex may start from subdirectory, so avoid relative paths
3. Default timeout is 600 seconds

---

### 3. Cursor Hooks

Cursor has a focused implementation with unique features for Tab (inline completions) and Agent modes.

#### Configuration Format

```json
{
  "version": 1,
  "hooks": {
    "afterFileEdit": [
      { "command": "./hooks/format.sh" }
    ],
    "beforeShellExecution": [
      {
        "command": "./scripts/approve-network.sh",
        "timeout": 30,
        "matcher": "curl|wget|nc",
        "type": "command",
        "failClosed": false
      }
    ]
  }
}
```

#### Key Features

- **Agent and Tab separation**: Different hooks for Agent (Cmd+K) vs Tab (inline completions)
- **Shell-specific hooks**: `beforeShellExecution`/`afterShellExecution` for shell commands
- **MCP-specific hooks**: `beforeMCPExecution`/`afterMCPExecution` for MCP tools
- **Prompt-based hooks**: LLM-evaluated conditions without custom scripts
- **Enterprise distribution**: MDM and cloud-based team distribution
- **`failClosed` option**: Block on failure for security-critical hooks
- **`loop_limit`**: Prevent infinite loops in stop/subagentStop hooks (default 5)

#### Hook Types

**Command hooks:**
```json
{
  "command": "./script.sh",
  "timeout": 30,
  "matcher": "pattern",
  "failClosed": false
}
```

**Prompt hooks (LLM-evaluated):**
```json
{
  "type": "prompt",
  "prompt": "Does this command look safe? Only allow read-only operations.",
  "timeout": 10,
  "model": "optional-model-override"
}
```

#### Input Schema (Common)

```json
{
  "conversation_id": "string",
  "generation_id": "string",
  "model": "string",
  "hook_event_name": "string",
  "cursor_version": "string",
  "workspace_roots": ["<path>"],
  "user_email": "string | null",
  "transcript_path": "string | null"
}
```

#### Output Schema

```json
{
  "permission": "allow|deny|ask",
  "user_message": "message shown to user",
  "agent_message": "message sent to agent",
  "updated_input": { "modified": "parameters" },
  "additional_context": "context to inject",
  "followup_message": "auto-continue message (stop hook)"
}
```

#### Exit Code Semantics

- `0` = success, use JSON output
- `2` = block (equivalent to `permission: "deny"`)
- Other = hook failed, action proceeds (fail-open) unless `failClosed: true`

#### Unique Features

**Tab-specific hooks:**
- `beforeTabFileRead` - Control file access for Tab completions
- `afterTabFileEdit` - Post-process Tab edits with detailed edit info

**Shell command matcher:**
- `beforeShellExecution` matcher runs against the full command string
- Example: `"matcher": "curl|wget|nc"` matches network commands

**Session environment:**
```json
{
  "env": { "KEY": "value" },
  "additional_context": "context for conversation"
}
```

#### Best Practices (Cursor)

1. Use `.cursor/hooks/script.sh` for project hooks (run from project root)
2. Use `./hooks/script.sh` for user hooks (run from `~/.cursor/`)
3. Set `failClosed: true` for security-critical hooks
4. Use `loop_limit` to prevent infinite stop hook loops
5. Prompt hooks are good for policy enforcement without custom scripts

---

### 4. GitHub Copilot Hooks

GitHub Copilot has a focused, cross-platform hooks implementation with unique error handling capabilities.

#### Configuration Format

```json
{
  "version": 1,
  "hooks": {
    "sessionStart": [
      {
        "type": "command",
        "bash": "./scripts/session-start.sh",
        "powershell": "./scripts/session-start.ps1",
        "cwd": "scripts",
        "env": { "LOG_LEVEL": "INFO" },
        "timeoutSec": 30
      }
    ],
    "preToolUse": [
      {
        "type": "command",
        "bash": "./scripts/security-check.sh",
        "powershell": "./scripts/security-check.ps1",
        "timeoutSec": 15
      }
    ]
  }
}
```

#### Key Features

- **Cross-platform support**: Separate `bash` and `powershell` fields for Unix/Windows
- **8 hook events**: sessionStart, sessionEnd, userPromptSubmitted, preToolUse, postToolUse, agentStop, subagentStop, errorOccurred
- **No matcher system**: Scripts handle their own filtering logic
- **Project-level only**: Hooks stored in `.github/hooks/*.json`
- **Multiple hooks per event**: Execute in order defined
- **Synchronous execution**: All hooks block agent execution

#### Input Schema (Common Fields)

```json
{
  "timestamp": 1704614400000,
  "cwd": "/path/to/project"
}
```

#### Event-Specific Input

**sessionStart:**
```json
{
  "source": "new|resume|startup",
  "initialPrompt": "optional initial prompt"
}
```

**sessionEnd:**
```json
{
  "reason": "complete|error|abort|timeout|user_exit"
}
```

**userPromptSubmitted:**
```json
{
  "prompt": "the user's prompt text"
}
```

**preToolUse:**
```json
{
  "toolName": "bash|edit|view|create",
  "toolArgs": "{\"command\":\"npm test\"}"
}
```

**postToolUse:**
```json
{
  "toolName": "bash",
  "toolArgs": "{\"command\":\"npm test\"}",
  "toolResult": {
    "resultType": "success|failure|denied",
    "textResultForLlm": "result text"
  }
}
```

**errorOccurred:**
```json
{
  "error": {
    "message": "Network timeout",
    "name": "TimeoutError",
    "stack": "stack trace"
  }
}
```

#### Output Schema

Only `preToolUse` supports output. Other hooks' output is ignored.

```json
{
  "permissionDecision": "allow|deny|ask",
  "permissionDecisionReason": "Explanation for the decision"
}
```

Note: Only `"deny"` is currently processed. `"allow"` and `"ask"` are parsed but not implemented.

#### Unique Features

**Cross-platform scripts:**
```json
{
  "type": "command",
  "bash": "./scripts/check.sh",
  "powershell": "./scripts/check.ps1"
}
```

**errorOccurred hook:**
- Unique to GitHub Copilot
- Fires on any error during agent execution
- Useful for logging, alerting, and error tracking

**Multiple hooks per event:**
```json
{
  "preToolUse": [
    { "type": "command", "bash": "./security-check.sh" },
    { "type": "command", "bash": "./audit-log.sh" },
    { "type": "command", "bash": "./metrics.sh" }
  ]
}
```

#### Limitations

- No user-level configuration (project-only)
- No matcher/filter system in config
- Only `deny` permission decision is processed
- Cannot modify tool input or inject context
- No HTTP or prompt hook types
- Synchronous execution only (no async option)

#### Best Practices (GitHub Copilot)

1. Keep hook execution under 5 seconds for responsiveness
2. Use asynchronous logging (append to files) rather than synchronous I/O
3. Always validate and sanitize JSON input from stdin
4. Use proper shell escaping to prevent injection vulnerabilities
5. Set appropriate timeouts to prevent resource exhaustion
6. Use `jq` in bash or `ConvertFrom-Json` in PowerShell for JSON parsing

---

## Recommendations for cake

Based on this research, here are recommendations for implementing hooks in cake:

### 1. Core Architecture

Adopt the common patterns shared by all three systems:

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│  Hook Config    │────▶│   Hook Runner    │────▶│  Tool Executor  │
│  (JSON)         │     │  (spawns hooks)  │     │  (if allowed)   │
└─────────────────┘     └──────────────────┘     └─────────────────┘
                               │
                               ▼
                        ┌──────────────────┐
                        │  Hook Script     │
                        │  (stdin/stdout)  │
                        └──────────────────┘
```

### 2. Configuration Schema

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash|Edit|Write",
        "hooks": [
          {
            "type": "command",
            "command": "/path/to/script.sh",
            "timeout": 60,
            "async": false
          }
        ]
      }
    ]
  }
}
```

### 3. Essential Hook Events

Start with these core events (prioritized):

1. **PreToolUse** - Block or modify tool calls before execution
2. **PostToolUse** - Audit, format, or inject context after success
3. **PostToolUseFailure** - Handle errors and provide recovery context
4. **SessionStart** - Inject context, set up environment
5. **Stop** - Auto-continue or cleanup
6. **UserPromptSubmit** - Validate or augment user prompts
7. **ErrorOccurred** - Log errors, send alerts, track error patterns

### 4. Input Schema

```json
{
  "session_id": "uuid",
  "transcript_path": "/path/to/transcript.jsonl",
  "cwd": "/current/working/directory",
  "hook_event_name": "PreToolUse",
  "tool_name": "Bash",
  "tool_input": {
    "command": "npm test"
  },
  "tool_use_id": "unique-id"
}
```

### 5. Output Schema

```json
{
  "permission": "allow|deny|ask",
  "reason": "Explanation for the decision",
  "updated_input": { "command": "modified command" },
  "additional_context": "Context for the LLM"
}
```

### 6. Exit Code Semantics

```
Exit 0 = Success, parse stdout for JSON
Exit 2 = Block the action, show stderr to LLM
Exit 1 (or other) = Non-blocking error, continue execution
```

### 7. Configuration Locations

| Location | Path | Priority |
|----------|------|----------|
| User | `~/.config/cake/hooks.json` | Lowest |
| Project | `.cake/hooks.json` | Medium |
| Local (gitignored) | `.cake/hooks.local.json` | Highest |

### 8. Rust Implementation Considerations

```rust
pub enum HookEvent {
    SessionStart,
    SessionEnd,
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    UserPromptSubmit,
    ErrorOccurred,
    Stop,
}

pub struct HookConfig {
    pub matcher: Option<Regex>,
    pub hooks: Vec<HookHandler>,
}

pub enum HookHandler {
    Command {
        command: String,
        timeout: Duration,
        r#async: bool,
    },
}

pub struct HookInput {
    pub session_id: String,
    pub cwd: PathBuf,
    pub hook_event_name: HookEvent,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
}

pub enum HookDecision {
    Allow,
    Deny { reason: String },
    Ask { reason: String },
    Modify { input: serde_json::Value, reason: String },
}
```

### 9. Sandbox Integration

Since cake uses OS-level sandboxing (macOS Seatbelt, Linux Landlock), hooks should:

1. **Run outside the sandbox** by default (for flexibility)
2. **Optionally run sandboxed** for security-sensitive deployments
3. **Inherit environment** from the cake session
4. **Receive sandbox context** in input (e.g., which sandbox profile is active)

### 10. Security Considerations

1. **Fail-open by default**: Non-blocking errors allow execution to continue
2. **Optional `failClosed`**: For security-critical hooks (like Cursor)
3. **Timeout enforcement**: Prevent hanging hooks (default 60s)
4. **Input validation**: Sanitize JSON input before passing to hooks
5. **Output validation**: Validate hook output before acting on it

### 11. Advanced Features to Consider

**From Claude Code:**
- `if` conditions using permission rule syntax
- HTTP hooks for remote validation services
- Prompt hooks (LLM-evaluated conditions)
- `CLAUDE_ENV_FILE` for environment persistence

**From Cursor:**
- `failClosed` option for security-critical hooks
- `loop_limit` for stop hook iteration caps
- Separate Tab vs Agent hooks
- Shell-specific hooks (`beforeShellExecution`)

**From Codex:**
- Git-root-relative path resolution
- Concurrent hook execution

**From GitHub Copilot:**
- Cross-platform script support (bash + PowerShell fields)
- `errorOccurred` hook for error handling and alerting
- Multiple hooks per event (ordered execution)
- `cwd` and `env` fields in hook configuration
- `timestamp` in all hook inputs (Unix milliseconds)

---

## Implementation Phases

### Phase 1: Core Hooks
- PreToolUse, PostToolUse, PostToolUseFailure
- Command hook type only
- JSON stdio communication
- Exit code semantics

### Phase 2: Lifecycle Hooks
- SessionStart, SessionEnd, Stop
- UserPromptSubmit
- ErrorOccurred (for error handling/alerting)
- Matcher/filter system

### Phase 3: Advanced Features
- HTTP hooks
- Prompt hooks (LLM-evaluated)
- `if` conditions
- Environment persistence
- Async hooks

### Phase 4: Enterprise Features
- Multiple configuration layers
- Hook validation and testing
- Telemetry and debugging

---

## References

- https://code.claude.com/docs/en/hooks.md
- https://code.claude.com/docs/en/hooks-guide.md
- https://developers.openai.com/codex/hooks.md
- https://cursor.com/docs/hooks.md
- https://docs.github.com/api/article/body?pathname=/en/copilot/concepts/agents/coding-agent/about-hooks
- https://docs.github.com/api/article/body?pathname=/en/copilot/reference/hooks-configuration

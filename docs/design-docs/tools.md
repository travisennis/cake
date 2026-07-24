# Tools Framework

The `clients::tools` module provides the tool execution framework that enables AI agents to interact with the filesystem and execute commands safely.

This document describes the contracts and design decisions of the tool framework. For exact schemas, output formats, limits, and error strings, the code and its tests are authoritative: each tool lives in its own module under `src/clients/tools/` (`bash`, `read`, `edit`, `write`), with its model-facing description in the adjacent `*-description.txt` file and its behavior pinned by the module's tests.

## Overview

Cake provides four built-in tools:

1. **Bash**: Execute shell commands with sandboxing
2. **Read**: Read file contents
3. **Edit**: Make targeted text replacements in files
4. **Write**: Create new files or overwrite existing ones

Users can extend this set with their own executable tools ("toolbox tools", registered with a `tb__` prefix); see [Toolbox Tools](#toolbox-tools).

Each tool defines a JSON schema for the API (name, description, parameters), validation logic for arguments, and execution logic with proper error handling.

## Tool Execution

Each tool is registered as a `ToolEntry` (model-facing definition plus executor closure) in a `ToolRegistry`; the registry dispatches calls by name via `ToolRegistry::execute`.

Execution flow:

1. Attempt strict JSON argument parsing using serde
2. If parsing fails, apply a conservative repair pass (`json_repair`) that handles raw control characters inside string literals and trailing garbage after a balanced top-level JSON value
3. Re-attempt parsing on the repaired text
4. Validate inputs (paths, etc.)
5. Execute the operation
6. Return `ToolResult` with output or error

Repairs are deterministic and lossless --- if a repair produces wrong content, the tool's own preflight validation still catches it.

Results are returned as strings so they can be included in API responses.

## Tool-Call Scheduling

Tool calls issued in one assistant turn execute concurrently, with one exception: calls that would mutate the same file are serialized (see ADR-013).

Before execution, the agent loop partitions the turn's tool calls into groups by canonical mutating target path (`ToolRegistry::mutating_target`, implemented for Edit and Write). Groups run concurrently with each other and with all non-mutating calls; calls within a group run sequentially in the order the model issued them, so each call observes the previous call's effects. Two Edits targeting the same file in one turn therefore both execute, and the second operates on the first's result.

Scheduling rules:

- Only Edit and Write calls with a determinable canonical target path are serialized. Relative and absolute paths to the same file resolve to the same group.
- Calls whose arguments fail to parse or whose path fails validation are not serialized; they execute as scheduled and surface their own errors.
- Hook-blocked calls resolve to immediate error results and never join a group. A blocked or failed call does not abort later calls in its group; each subsequent call runs against whatever state prior calls left and succeeds or fails on its own.
- Bash commands are never serialized against Edit/Write calls, even when they touch the same file.

Regardless of grouping, tool results are recorded and streamed in the model's issue order with per-call attribution, so transcript ordering, session records, and `stream-json` output are unaffected by scheduling.

## Path Validation

All filesystem tools validate paths before operating (`validate_path_in_cwd`). A path is allowed when it is within:

- the current working directory, or
- allowed temp directories (`/tmp`, `/var/folders`, `TMPDIR`), or
- directories added via the `--add-dir` CLI flag (read-only access), or
- registered skill directories (read-only; see [skills.md](./skills.md))

This prevents the AI from accessing sensitive files outside the project.

### Write Tool Path Handling

The Write tool has special handling for new files that don't exist yet (`validate_path_for_write`). Existing path components are resolved via `canonicalize()` (following symlinks); nonexistent trailing components are normalised lexically (`..` cancels the preceding normal component), and the resolved base directory must be within allowed directories. This allows creating new files in new subdirectories while maintaining security, correctly handles parent-directory (`..`) components across nonexistent ancestors, and preserves symlink resolution for existing path segments.

## Individual Tools

### Bash Tool

Executes shell commands in the project working directory (`ToolContext.cwd`) under the OS-level sandbox (Seatbelt on macOS, Landlock on Linux; see [sandbox.md](./sandbox.md)). Behavioral contract:

- **Output capping and truncation**: process output is read up to a hard cap and, above an inline limit, returned as a head+tail preview with the full output saved to a secure per-user temp directory (`0o700` on Unix, so other local users cannot pre-create it or read captured output). If the temp file cannot be written, a larger inline head+tail fallback is used. The limits and preview formats live in `clients::tools::bash`.
- **Metadata footer**: every result ends with a footer reporting exit code and adaptive duration formatting.
- **Stderr on success**: a command that exits 0 but writes to stderr gets a warning marker, because the tool cannot reliably distinguish harmless progress output from diagnostics.
- **Empty search miss**: a search command (`rg`, `grep`, and variants) that exits 1 with no output gets a `(no matches)` marker while keeping the original exit code visible, distinguishing an empty result from a tool or shell error.
- **Binary output detection**: output that looks binary (null bytes / high non-printable ratio) is never returned inline; it is saved to the secure temp directory, MIME-sniffed with the `infer` content-signature database, and reported with suggested inspection tools.
- **Timeout**: the command is killed and a timeout error returned.
- **Cancellation**: dropping an in-flight Bash execution, including on Ctrl-C, kills the command's entire process group so descendant processes do not survive cake's interrupted turn.

**Destructive Command Blocking**:

The Bash tool includes a narrow, best-effort pre-execution destructive command guard that blocks known-destructive commands before they reach the sandbox or process spawn. This complements the OS-level sandbox by catching destructive operations that are allowed within the sandbox's permitted zones --- for example, destructive git operations inside the repo or remote-affecting operations like force-push. It is not a shell security policy engine; the OS sandbox is the filesystem enforcement boundary.

Blocked categories:

- **Destructive git operations**: history- or worktree-destroying commands such as `git reset --hard`, working-tree-discarding `checkout`/`restore` forms, forced `clean`, `push --force` (`--force-with-lease` is allowed), force branch deletion, and stash deletion.
- **Git commit message corruption**: `git commit -m`/`--message` with backticks or `$()` inside a double-quoted value, which the shell would interpret as command substitution.
- **Irreversible filesystem deletion**: `rm -rf` outside literal `/tmp` or `/var/tmp` targets. Environment-variable or shell-expanded temp paths are blocked unless the guard deliberately supports and tests that exact form.

Additional protections: `bash -c`/`sh -c` wrappers are unwrapped and the inner script recursively checked; chained commands (`&&`, `||`, `;`, newlines) are split and each segment checked; data contexts such as commit messages, `echo`, and `printf` are skipped to avoid false positives.

Blocked commands return a structured `BLOCKED` message with a reason and a safe alternative. The authoritative rule set, matching logic, and error wording live in `clients::tools::bash_safety` and its tests.

> **Note**: Destructive command blocking is a best-effort guard, not a security boundary. The OS-level sandbox remains the primary enforcement mechanism. See [sandbox.md](./sandbox.md) for details.

### Read Tool

Reads file contents with line-numbered output and pagination (`start_line`/`end_line`, defaulting to the first 200 lines). Rejects directories with a clear error, rejects binary files (null-byte detection), truncates very large reads, and emits pagination hints for remaining lines. Defaults and caps live in `clients::tools::read` and `read-description.txt`.

### Edit Tool

Makes targeted text replacements in existing files via a list of `{old_text, new_text}` edits (bounded by `MAX_EDITS_PER_CALL`). Behavioral contract:

- **Preflight validation**: all edits are validated before any change is applied; a failing edit means no change at all.
- **Uniqueness**: each `old_text` must match exactly once; ambiguous matches produce capped, line-numbered candidate contexts so the model can disambiguate.
- **Overlap detection**: conflicting edits are rejected with the edit numbers involved.
- **Fidelity**: line endings (LF/CRLF) and UTF-8 BOM are preserved; matching is exact including whitespace; empty `new_text` deletes; identical `old_text`/`new_text` pairs are no-ops.
- **Diagnostics**: invalid JSON arguments produce a bounded context window around the serde failure offset with control characters visibly escaped, a caret marker, and a targeted hint keyed off the error kind. The result includes a unified diff of applied changes.

### Write Tool

Creates new files or overwrites existing ones, creating parent directories automatically. Output distinguishes create from overwrite and warns on overwrite (Edit is more precise for modifying existing files).

## Toolbox Tools

Toolbox tools are user-provided executables that extend the agent's tool set. Each tool is a single executable (any language) implementing a two-action protocol selected by the `TOOLBOX_ACTION` environment variable.

**Discovery** (`config::toolbox`):

- Directories come from `CAKE_TOOLBOX` (colon-separated, like `PATH`), with the `--toolbox <DIR>` CLI flag appending extra directories. When `CAKE_TOOLBOX` is unset, the default directory `~/.config/cake/tools` is scanned; when it is set to the empty string, environment-driven scanning is disabled. Relative entries are anchored to the directory cake was invoked from, before any `--worktree` cwd change (matching `--add-dir`).
- Hidden files, `.md`/`.txt` files, directories, and non-executable files are skipped. Project-level directories are only scanned when explicitly listed (no automatic `.cake/tools` pickup, so cloned repositories cannot inject tools).
- Earlier directories win tool-name conflicts; within a directory, filenames are scanned in sorted order.

**Describe protocol** (`TOOLBOX_ACTION=describe`, run once per tool at startup, 10s timeout, output read with a 64KB cap so a runaway tool is skipped instead of exhausting memory):

The tool prints its schema to stdout in either format (auto-detected by attempting JSON parse first; the detected format is reused for execute):

- **JSON**: `{"name": "...", "description": "...", "args": {"param": ["string", "description"]}}` (compact) or `{"name": ..., "description": ..., "inputSchema": {...}}` (full JSON Schema with a top-level object). A missing top-level `type` is normalized to `object`; explicit non-object types are rejected because toolbox calls always use named arguments. The resulting schema must compile as JSON Schema draft 2020-12 or the tool is skipped. A `?` suffix on a compact-form type (`"string?"`) marks the parameter optional. An optional `timeout` field (seconds) overrides the default 60s execute timeout (JSON format only).
- **Text**: line-based `name: ...`, one or more `description: ...` lines (concatenated with newlines), and `param: type description` parameter lines. Types are required (`string`, `number`, `integer`, `boolean`) and support the `?` optional suffix. Duplicate parameter names are rejected so the generated JSON Schema remains valid. Parameter names containing `=`, carriage returns, or newlines are also rejected during discovery because the execute protocol cannot encode them safely.

The `name` field from the describe output is authoritative (not the filename) and is registered with a `tb__` prefix (e.g. `tb__run_tests`). Names are restricted to `[A-Za-z0-9_-]` and at most 60 characters (the 64-character provider function-name limit minus the prefix). Broken tools (describe failure, invalid output, timeout, duplicate name) are skipped with a warning and never block startup.

**Execute protocol** (`TOOLBOX_ACTION=execute`):

- Arguments arrive on stdin: the raw JSON object for JSON-format tools, or `key=value` lines for text-format tools. Because the text protocol defines no escaping, argument names containing `=`, carriage returns, or newlines and string values containing carriage returns or newlines are rejected before spawn rather than being misinterpreted as additional arguments. Stdin is fed from a detached writer so a tool that never reads it cannot stall the call.
- The process runs in the session working directory with `AGENT=cake` and the session id in both `CAKE_THREAD_ID` and `AGENT_THREAD_ID`.
- stdout becomes the tool result. Output is read with a hard 50KB cap: once exceeded, the tool is stopped and the captured output is returned with a truncation marker. stderr goes to cake's log file for tool-author diagnostics (first 10KB kept).
- Exit code 0 is success; non-zero returns an error containing the exit status and stderr.
- Each invocation runs in its own Unix process group. The per-tool timeout (default 60s) covers the whole operation --- argument delivery, output capture, and process exit --- and timeout or output-cap paths terminate the entire process group, including descendants.

**Sandboxing**: toolbox tools run as separate processes *without* cake's OS sandbox --- they are user-provided and trusted. For that reason, under the read-only sandbox policy toolbox executables are never run at all: discovery and the describe action are skipped entirely (even describe executes user code that could mutate the workspace), and no `tb__*` tools are registered (same rationale as removing Edit/Write there: they would bypass the policy's no-mutation guarantee).

**Scheduling**: toolbox calls execute concurrently like other non-mutating calls; the scheduler cannot determine their mutation targets, so same-path serialization (ADR-013) does not apply to them.

## Sandboxing

The Bash tool integrates with the `tools::sandbox` module: when sandboxing is enabled, the platform strategy (Seatbelt or Landlock) is applied to the spawned command. See [sandbox.md](./sandbox.md) for the policy and implementation.

## Error Handling

Tools return `Result<ToolResult, String>` where `Ok` carries the output string and `Err` carries a descriptive message. Error messages are designed to be:

- Actionable (suggest what to do)
- Descriptive (include path, context)
- Safe (don't expose sensitive info)

Tool errors are returned to the model as function-call output rather than aborting the task, so the model can decide how to proceed. Exact error wording is pinned by each tool module's tests.

## Related Documentation

- [prompts.md](./prompts.md): Tool definitions are included in system prompts
- [cli.md](./cli.md): CLI layer triggers tool execution
- [sandbox.md](./sandbox.md): OS-level sandboxing implementation

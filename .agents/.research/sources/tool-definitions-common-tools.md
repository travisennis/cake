# Common Tool Definition Design Across Claude Code, PI, and DS4

Status: active
Created: 2026-05-24
Updated: 2026-05-24
Related tasks: -
Related plans: -
Confidence: high

## Summary

`TOOL_DEFINITIONS.md`, `TOOL_DEFINITIONS_PI.md`, and `TOOL_DEFINITIONS_DS4.md` all share four core tools by direct purpose: `bash`, `read`, `edit`, and `write`. They also contain analogous discovery/search tools, but those are not named or split the same way: Claude Code has `Glob` and `Grep`, PI has `find`, `grep`, and `ls`, and DS4 has `search` and `list`.

The shared core design pattern is:

- `bash` is the escape hatch for command execution, with timeout and truncation controls.
- `read` establishes file context and returns bounded chunks.
- `edit` is intended for targeted changes with guards against ambiguous or stale edits.
- `write` replaces whole-file content and is usually framed as a creation or full-rewrite tool.

The main design split is how much safety is enforced by the tool protocol itself. Claude Code and DS4 encode strong read-before-edit behavior, while PI uses simpler exact-replacement semantics and pluggable operation interfaces. DS4 is the most line-oriented and stateful; Claude Code is the richest API/UI integration; PI is the smallest and most portable adapter surface.

## Sources

- `TOOL_DEFINITIONS.md` - Claude Code tool catalog.
- `TOOL_DEFINITIONS_PI.md` - PI coding-agent tool summary.
- `TOOL_DEFINITIONS_DS4.md` - ds4-agent tool reference.

## Common Tool Matrix

| Capability | Claude Code | PI | DS4 |
|---|---|---|---|
| Shell command | `Bash` | `bash` | `bash`, plus `bash_status` and `bash_stop` |
| File read | `Read` | `read` | `read`, plus `more` continuation |
| Targeted edit | `Edit` | `edit` | `edit` |
| Whole-file write | `Write` | `write` | `write` |
| Content search | `Grep` | `grep` | `search` |
| File discovery/listing | `Glob` / Bash `ls` guidance | `find`, `ls` | `list` |

## Bash

### Shared Design

All three systems use a shell execution tool for general command execution and expose some form of timeout and output truncation. The design assumes the model needs enough output to reason about the result, but large output must be bounded and either summarized or persisted.

### Claude Code

Claude Code's `Bash` is a first-class `ToolDef` with Zod input/output schemas, UI rendering hooks, permission hooks, concurrency metadata, and API serialization. Its input includes:

- `command`
- optional `timeout` in milliseconds, capped at 900000
- optional `description`
- optional `run_in_background`
- optional `dangerouslyDisableSandbox`
- internal `_simulatedSedEdit`

The prompt carries extensive behavioral policy: prefer dedicated file/search tools over shell commands, use absolute paths, avoid `cd`, quote paths with spaces, and follow Git/PR safety rules. The output schema is broad: stdout/stderr, interruption state, background task metadata, image flags, sandbox override state, semantic return-code interpretation, and persisted output metadata.

### PI

PI's `bash` is much smaller. It accepts:

- `command`
- optional `timeout` in seconds

Its options emphasize portability and delegation:

- pluggable `operations.exec`
- optional `commandPrefix`
- optional `shellPath`
- optional `spawnHook` to adjust command, cwd, or env

Output is truncated to the last roughly 100 lines or 10KB; full output is saved to a temp file when truncated.

### DS4

DS4's `bash` is built around long-running job control. It runs the command through `/bin/sh -c`, creates an async process group, captures stdout/stderr to a temp file, and returns a job ID. It accepts:

- `command`
- optional `timeout_sec`, default 3600, clamped to 1..86400
- optional `refresh_sec`, default 60, clamped to 1..3600

DS4 splits command lifecycle into three tools:

- `bash` starts and observes initial output.
- `bash_status` polls running or completed jobs.
- `bash_stop` terminates a running job with SIGTERM, then SIGKILL if needed.

This is the strongest design for long-running commands because the lifecycle is explicit rather than a single call with optional backgrounding.

## Read

### Shared Design

All three read tools provide bounded file access with offset/range controls. The model is expected to read before editing so it can preserve exact context.

### Claude Code

Claude Code's `Read` requires an absolute `file_path` and supports:

- optional `offset`
- optional `limit`
- optional `pages` for PDFs

It reads up to 2000 lines by default and returns line-numbered `cat -n` style output for text. It is multimodal: text, images, notebooks, and PDFs each have distinct output variants. The prompt states that screenshots should be viewed through this tool, and large PDFs require a page range.

### PI

PI's `read` accepts relative or absolute `path`, plus:

- optional `offset`, 1-indexed
- optional `limit`

Text output is truncated to about 100 lines or 10KB. Images are supported and can be auto-resized to a 2000x2000 maximum. The operations layer is pluggable, with `readFile`, `access`, and `detectImageMimeType`.

### DS4

DS4's `read` is explicitly edit-oriented. It accepts:

- `path`
- optional `start_line`
- optional `max_lines`, default 500
- optional `whole`
- optional `raw`

Default output includes line-number prefixes. When more lines are available, the result includes `continue_offset=N`; the model is expected to continue through `more`. Every emitted line is stored in an internal file-view cache for stale-edit protection.

DS4 is the most deliberate about connecting read output to edit safety. Read is not just file access; it is how the model earns permission to perform line/range edits safely.

## Edit

### Shared Design

All three edit tools prefer targeted changes over whole-file rewrites. They reject ambiguous exact-text replacements and provide either diffs or post-edit context.

### Claude Code

Claude Code's `Edit` accepts:

- absolute `file_path`
- `old_string`
- `new_string`
- optional `replace_all`

The prompt requires that the file be read at least once before editing. By default, `old_string` must be unique. `replace_all` is available for intentional repeated replacement. The output includes original content, structured patch data, whether the user modified the proposed changes, replace-all state, and optional Git diff data.

### PI

PI's `edit` accepts a path and an array of edits:

- each edit has `oldText`
- each edit has `newText`

Each `oldText` must uniquely match a non-overlapping region in the original file. All edits are matched against the original file, not sequentially against previous edits in the same call. Nearby or overlapping changes are expected to be merged by the caller.

PI's edit operation is compact and batch-friendly, but less stateful than Claude Code or DS4. Its safety relies on exact unique matches and non-overlap constraints rather than a prior-read cache.

### DS4

DS4's `edit` supports three modes:

- line/range mode with `line`, `start_line`, `end_line`, or `range`
- whole-file mode with `range="all"`
- old/new mode with exact `old` and `new`

Line/range mode is preferred and only works if the affected lines were recently shown by `read` or `search` and have not changed since. DS4 uses a CRC32 stale-edit guard tied to its file-view cache. After a successful edit, DS4 returns a context window so the model can see shifted line numbers immediately.

This is the strongest edit model for line-addressed coding agents because stale context is checked by the tool rather than left to model discipline.

## Write

### Shared Design

All three write tools replace a whole file and are therefore higher blast-radius than edit. They are best suited for new files, generated artifacts, or intentional full rewrites.

### Claude Code

Claude Code's `Write` accepts:

- absolute `file_path`
- `content`

The prompt says existing files must be read first and recommends `Edit` for modifications. It also explicitly discourages creating documentation files unless the user requests them. Output includes create/update type, written content, structured patch, original file content when applicable, and optional Git diff.

### PI

PI's `write` accepts:

- `path`
- `content`

It creates parent directories automatically and overwrites existing files. The operations layer contains `writeFile` and recursive `mkdir`, making it easy to redirect writes to local or remote storage.

### DS4

DS4's `write` accepts:

- `path`
- `content`

After a successful write, DS4 records the entire file in its file-view cache so subsequent line/range edits can proceed safely. This makes `write` part of the same stale-edit safety model as `read`, `search`, and `edit`.

## Search and Listing Analogs

These tools are not identical across the three documents, but the capability is shared.

Claude Code has `Grep` for content search and `Glob` for file matching. Its `Bash` prompt tells the model to prefer `Glob` over shell `find` or `ls`, and `Grep` over shell `grep` or `rg`.

PI has explicit `grep`, `find`, and `ls` tools:

- `grep` returns matching lines with paths and line numbers, respects `.gitignore`, supports regex/literal modes, glob filtering, context, case-insensitivity, and result limits.
- `find` searches by glob pattern, respects `.gitignore`, and returns relative paths.
- `ls` lists one directory, sorted alphabetically, with directory suffixes.

DS4 has `search` and `list`:

- `search` supports literal or POSIX extended regex modes, optional glob filtering, bounded context, result limits, binary-file skipping, `.git` skipping, and max recursion depth. Emitted lines are added to the file-view cache for edit safety.
- `list` lists one directory compactly with type, size, and name.

The important design difference is that DS4 search participates in stale-edit guarding, while PI search/list are primarily discovery tools. Claude Code's search/list equivalents are integrated into a larger tool permission and API orchestration framework.

## Cross-System Design Findings

### 1. The smallest common useful set is Bash, Read, Edit, Write

All three systems converge on the same minimum coding-agent surface: run commands, inspect files, apply targeted edits, and write whole files. Search/list tools are important but less standardized.

### 2. Read-before-edit is a core safety pattern

Claude Code enforces read-before-edit in prompt/tool behavior. DS4 enforces it mechanically for line/range edits through a file-view cache and CRC32 stale checks. PI does not require a prior read in the summarized definition, but exact unique `oldText` matching still prevents many accidental edits.

### 3. Exact replacement is the shared edit primitive

Claude Code and PI center exact old/new replacement. DS4 supports exact old/new replacement as a fallback, but prefers line/range edits anchored to recently viewed content.

### 4. Output truncation is universal, but persistence differs

All three systems constrain output size. Claude Code has schema fields for persisted large outputs. PI saves full truncated command output to temp files. DS4 always writes bash output to a temp file and returns bounded observations.

### 5. DS4 treats tool state as part of correctness

DS4's `read`, `search`, `write`, and `edit` are linked by the file-view cache. This makes stale context a tool-level concern. The tradeoff is a more stateful protocol that requires continuation tools like `more` and job tools like `bash_status`.

### 6. PI optimizes for adapter portability

PI exposes pluggable operations for each tool, which makes local, remote, SSH, or test-backed execution straightforward. This design is smaller than Claude Code's `ToolDef` and less stateful than DS4's cache, but very practical for embedding.

### 7. Claude Code optimizes for product integration

Claude Code's tools are not just functions. They include prompt text, input/output schemas, UI renderers, permission checks, API serialization, concurrency flags, deferred loading behavior, and user-facing summaries. This makes the tool layer part of the product runtime, not just an execution adapter.

## Implications for cake

cake already has the same conceptual core: Bash, Read, Edit, and Write. The comparison suggests several design directions worth preserving or considering:

- Keep the core tool set small and predictable.
- Prefer dedicated tools over shell commands for file reads, searches, edits, and writes.
- Make read-before-edit safety enforceable by the tool layer where possible, not only by prompt instruction.
- Consider whether line/range edits should be backed by a read-cache or content hash guard, as DS4 does.
- Preserve output truncation with a path to full output for commands that produce large logs.
- If cake needs remote execution or testability improvements, PI's pluggable operations interfaces are a clean model.
- If cake needs richer UI or model-facing tool descriptions, Claude Code's `ToolDef` structure is the most complete reference.

## Follow-ups

- Compare cake's current Bash, Read, Edit, and Write implementations against the safety properties above.
- Decide whether cake should add stale-read guards for edit operations.
- Decide whether command output persistence should be uniform across Bash and other tools.
- Consider a task to document cake's own tool contracts in the same matrix format.

# CLAUDE.md Management: Architecture and Flow

This document describes how Claude Code discovers, loads, processes, and injects CLAUDE.md files into the model's context. It is written for someone evaluating whether to adopt a similar approach in a different project.

---

## 1. Memory File Types and Priority

Claude Code defines six types of memory files, loaded in a specific order. Later files have higher priority because the model pays more attention to content that appears later in the prompt.

| Order | Type | Location | Scope | Checked In? |
|-------|------|----------|-------|-------------|
| 1 | `Managed` | `/etc/claude-code/CLAUDE.md` (system-managed) | All users, all projects | Admin-controlled |
| 2 | `User` | `~/.claude/CLAUDE.md` + `~/.claude/rules/*.md` | One user, all projects | No (private) |
| 3 | `Project` | `CLAUDE.md`, `.claude/CLAUDE.md`, `.claude/rules/*.md` (walked from CWD up to root) | All contributors to a repo | Yes |
| 4 | `Local` | `CLAUDE.local.md` (walked from CWD up to root) | One user, one project | No (gitignored) |
| 5 | `AutoMem` | `memory.md` entrypoint (auto-memory feature) | One user, across conversations | No |
| 6 | `TeamMem` | Team memory entrypoint (org sync feature) | Entire organization | Synced |

Each type can be individually disabled via the `settingSources` configuration (e.g., `projectSettings`, `userSettings`, `localSettings`). The `--bare` flag disables all auto-discovery except `--add-dir` directories.

---

## 2. File Discovery: Eager Loading

The core discovery function is `getMemoryFiles()` in `src/utils/claudemd.ts`. It is **memoized** (via `lodash-es/memoize`) so the filesystem walk happens once per session, unless the cache is explicitly invalidated.

### Discovery Algorithm

```
1. Managed: read /etc/claude-code/CLAUDE.md (always loaded)
2. Managed: read .md files in managed rules directory
3. User: read ~/.claude/CLAUDE.md (if userSettings enabled)
4. User: read ~/.claude/rules/*.md (unconditional + conditional)
5. Project: walk CWD → root, at each directory:
     a. Read CLAUDE.md (if projectSettings enabled)
     b. Read .claude/CLAUDE.md
     c. Read .claude/rules/*.md (unconditional + conditional)
6. Local: walk CWD → root, at each directory:
     a. Read CLAUDE.local.md (if localSettings enabled)
7. Additional directories (--add-dir): same as Project walk, if env var enabled
8. AutoMem: read memory.md entrypoint (if feature enabled)
9. TeamMem: read team memory entrypoint (if feature enabled)
```

The walk goes from CWD upward to root, then the results are reversed so that files closer to CWD appear later (higher priority). In a git worktree scenario, Project-type files from directories above the worktree but within the main repo are skipped to avoid duplication.

### Path Exclusion

The `claudeMdExcludes` setting (glob patterns) can exclude specific User, Project, or Local files. Managed, AutoMem, and TeamMem files are never excluded. Patterns are resolved for symlinks (handles macOS `/tmp` → `/private/tmp`).

### External Includes

If a memory file uses `@include` to reference a file outside the project directory (`pathInOriginalCwd` check), it is treated as an "external include." These are only loaded if the user has approved them (`hasClaudeMdExternalIncludesApproved`) or if `forceIncludeExternal` is passed (used only for approval checks, not for building context).

---

## 3. File Processing: What Happens to Each File

Each discovered file goes through `processMemoryFile()` which:

### 3.1 Frontmatter Parsing

YAML frontmatter is extracted and parsed. The `paths:` field is the only recognized directive:

```yaml
---
paths: src/**/*.ts, tests/**
---
Rule content here...
```

Files with `paths:` are **conditional rules**. They are only loaded when the model touches a file matching those glob patterns. Files without `paths:` are **unconditional** and always loaded.

### 3.2 HTML Comment Stripping

Block-level HTML comments (`<!-- ... -->`) are stripped from the content using the `marked` lexer. Comments inside inline code spans and fenced code blocks are preserved. Unclosed comments are left intact.

### 3.3 @include Directive Resolution

Memory files can reference other files using `@` syntax:

- `@path` or `@./relative/path` — relative to the including file
- `@~/home/path` — relative to home directory
- `@/absolute/path` — absolute path

Includes are resolved recursively up to depth 5. Circular references are prevented by tracking processed paths. Only files with allowed text extensions (`.md`, `.ts`, `.json`, etc.) can be included; binary files are skipped silently.

Included files are added as separate entries **before** the including file, with a `parent` field pointing back to the file that included them.

### 3.4 Content Truncation

AutoMem and TeamMem entrypoints are truncated to line and byte caps via `truncateEntrypointContent()`. Other file types are not truncated.

### 3.5 Deduplication

A `processedPaths` set (normalized for case-insensitive filesystems) prevents the same file from being loaded twice, even when reached via different include chains or directory walks.

---

## 4. Conditional Rules (paths frontmatter)

Files in `.claude/rules/*.md` can have a `paths:` frontmatter directive. These are **conditional rules** that only activate when the model operates on a matching file.

### How Conditional Rules Are Loaded

Conditional rules are **not** eagerly loaded at session start. Instead, they are loaded lazily via the nested memory attachment system (see section 6). When the model reads or edits a file, the attachment system checks whether any conditional rules match that file's path.

### Glob Matching

- For **Project** rules: glob patterns are relative to the directory containing `.claude/`
- For **Managed/User** rules: glob patterns are relative to the original CWD
- Matching uses the `ignore` library (gitignore-style matching)
- Patterns ending in `/**` have the suffix stripped (the `ignore` library treats `path` as matching both the path itself and everything inside it)

### Eager vs Lazy Split

During the eager `getMemoryFiles()` walk:
- **Unconditional** `.claude/rules/*.md` files (no `paths:` frontmatter) are loaded eagerly
- **Conditional** `.claude/rules/*.md` files (with `paths:` frontmatter) are skipped

Conditional rules are loaded later via `processConditionedMdRules()` when a file path triggers them.

---

## 5. Eager Injection into the Conversation

### 5.1 Formatting: `getClaudeMds()`

The `getClaudeMds()` function takes the array of `MemoryFileInfo` objects and formats them into a single string:

```
Codebase and user instructions are shown below. Be sure to adhere to these instructions. IMPORTANT: These instructions OVERRIDE any default behavior and you MUST follow them exactly as written.

Contents of /path/to/CLAUDE.md (project instructions, checked into the codebase):

<file content>

Contents of ~/.claude/CLAUDE.md (user's private global instructions for all projects):

<file content>
```

Each file gets a description label based on its type. TeamMem content is additionally wrapped in `<team-memory-content source="shared">` tags.

If the feature flag `tengu_paper_halyard` is enabled, Project and Local files are skipped entirely (token optimization for certain deployment scenarios).

If no memory files are found, `getClaudeMds()` returns an empty string.

### 5.2 User Context Assembly: `getUserContext()`

`getUserContext()` (in `src/context.ts`, memoized) calls:

```ts
const claudeMd = shouldDisableClaudeMd
  ? null
  : getClaudeMds(filterInjectedMemoryFiles(await getMemoryFiles()))
```

`filterInjectedMemoryFiles()` removes AutoMem and TeamMem entries from the eager load when the `tengu_moth_copse` feature flag is on (those are instead surfaced via a separate relevant-memories prefetch system).

The result is a dict:
```ts
{
  claudeMd: "<formatted string from getClaudeMds()>",
  currentDate: "Today's date is 2026-04-08."
}
```

### 5.3 Prepending to Messages: `prependUserContext()`

`prependUserContext()` (in `src/utils/api.ts`) takes the user context dict and creates a synthetic **user message** (not a system message) prepended before the user's actual message:

```xml
<system-reminder>
As you answer the user's questions, you can use the following context:
# claudeMd
<the formatted CLAUDE.md content>

# currentDate
Today's date is 2026-04-08.

IMPORTANT: this context may or may not be relevant to your tasks. You should not respond to this context unless it is highly relevant to your task.
</system-reminder>
```

This message is marked `isMeta: true`, which means it is hidden from the user in the UI but visible to the model. Because it is the first message in the conversation and stable across turns, it benefits from prompt caching.

**Key point**: CLAUDE.md content is injected as a **user message**, not as part of the system prompt. The system prompt is assembled separately via `getSystemPrompt()`.

---

## 6. Lazy Injection: Nested Memory Attachments

CLAUDE.md files in subdirectories below CWD and conditional rules with `paths:` frontmatter are loaded lazily, only when the model touches a relevant file.

### 6.1 Trigger Mechanism

When a tool (Read, Edit, Write, etc.) operates on a file, the file path is added to `nestedMemoryAttachmentTriggers` on the `ToolUseContext`. This is a `Set<string>` that accumulates paths across a turn.

### 6.2 Attachment Processing

`getNestedMemoryAttachments()` processes each trigger path through `getNestedMemoryAttachmentsForFile()`, which runs four phases:

1. **Managed/User conditional rules**: Load conditional rules from `/etc/claude-code/rules/` and `~/.claude/rules/` whose `paths:` globs match the target file.

2. **Nested directories** (CWD → target): For each directory between CWD and the target file, load:
   - `CLAUDE.md`
   - `.claude/CLAUDE.md`
   - Unconditional `.claude/rules/*.md`
   - Conditional `.claude/rules/*.md` matching the target file

3. **CWD-level directories** (root → CWD): Only conditional rules (unconditional ones were already loaded eagerly).

### 6.3 Deduplication

`memoryFilesToAttachments()` uses two dedup mechanisms:

- **`loadedNestedMemoryPaths`**: A `Set<string>` on `ToolUseContext` that never evicts. Once a CLAUDE.md path has been injected, it is never injected again in the same session.
- **`readFileState`**: An LRU cache (100 entries) that tracks which files have been "read." If a memory file's path is already in `readFileState`, it is skipped.

The `loadedNestedMemoryPaths` set exists because `readFileState` is an LRU that evicts entries in busy sessions. Without the set, an evicted entry would cause the same CLAUDE.md to be re-injected on every subsequent turn.

### 6.4 Rendering

`nested_memory` attachments are rendered as user messages wrapped in `<system-reminder>`:

```xml
<system-reminder>
Contents of /path/to/subdir/CLAUDE.md:

<file content>
</system-reminder>
```

These are appended to the conversation as attachments on the turn where the trigger file was accessed, not prepended at the start.

---

## 7. Subagent Optimization

Subagents (Explore, Plan, etc.) can have `omitClaudeMd: true` in their definition. When this flag is set and the feature gate `tengu_slim_subagent_claudemd` is enabled (defaults to true), the `claudeMd` key is stripped from the subagent's `userContext`:

```ts
const { claudeMd: _omittedClaudeMd, ...userContextNoClaudeMd } = baseUserContext
const resolvedUserContext = shouldOmitClaudeMd
  ? userContextNoClaudeMd
  : baseUserContext
```

This saves approximately 5-15 Gtok/week across 34M+ Explore spawns, since read-only agents don't need commit/PR/lint guidelines from CLAUDE.md. The main agent has full context and interprets their output.

---

## 8. Caching and Invalidation

### Caches

| Cache | Location | Scope |
|-------|----------|-------|
| `getMemoryFiles` | `claudemd.ts` | Memoized; cleared on compaction, `/clear`, worktree changes, settings sync |
| `getUserContext` | `context.ts` | Memoized; cleared on compaction, `/clear`, system prompt injection changes |
| `getSystemContext` | `context.ts` | Memoized; cleared on `/clear` |
| `readFileState` | `Tool.ts` (per-session) | LRU(100); tracks nested memory dedup |
| `loadedNestedMemoryPaths` | `Tool.ts` (per-session) | Set; never evicted; tracks what has been injected |

### Cache Clearing Functions

- **`clearMemoryFileCaches()`**: Clears `getMemoryFiles` memoize cache. Does NOT fire `InstructionsLoaded` hooks. Used for correctness-only invalidation (worktree enter/exit, settings sync, `/memory` dialog).

- **`resetGetMemoryFilesCache(reason)`**: Clears `getMemoryFiles` cache AND arms the `InstructionsLoaded` hook to fire on the next load. Used when instructions are actually reloaded into context (compaction).

- **`getUserContext.cache.clear()`**: Clears the outer memoize layer. Must be called alongside `resetGetMemoryFilesCache()` during compaction, otherwise the outer cache returns stale data.

### When Caches Are Cleared

| Event | What's Cleared |
|-------|---------------|
| `/clear` command | `getUserContext`, `getSystemContext`, `getGitStatus`, `getMemoryFiles` |
| Compaction (auto or manual) | `getUserContext`, `getMemoryFiles` (with hook) |
| Worktree enter/exit | `getMemoryFiles` (no hook) |
| Settings sync | `getMemoryFiles` (no hook) |
| Writing to CLAUDE.md | Analytics event logged; caches not explicitly cleared (content is stable within a session) |

---

## 9. Hook: InstructionsLoaded

When memory files are loaded, an `InstructionsLoaded` hook can fire for observability/audit purposes. This is gated by `hasInstructionsLoadedHook()` which checks if any hook is configured.

The hook fires with a `load_reason`:
- `session_start`: Initial eager load
- `compact`: Re-load after compaction
- `include`: File was `@include`d by another file
- `path_glob_match`: Conditional rule matched a file path
- `nested_traversal`: File discovered during nested directory walk

AutoMem and TeamMem types are excluded from hook dispatch (they are a separate memory system).

---

## 10. Feature Flags Affecting CLAUDE.md Loading

| Flag | Effect |
|------|--------|
| `tengu_paper_halyard` | Skips Project and Local memory files entirely (both eager and nested) |
| `tengu_moth_copse` | Removes AutoMem and TeamMem from eager load (surfaced via relevant-memories prefetch instead) |
| `tengu_slim_subagent_claudemd` | Strips CLAUDE.md from subagent contexts (default: true) |
| `CLAUDE_CODE_DISABLE_CLAUDE_MDS` | Disables all CLAUDE.md loading entirely |
| `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD` | Enables loading CLAUDE.md from `--add-dir` directories |
| `--bare` mode | Disables auto-discovery (CWD walk), but honors explicit `--add-dir` directories |

---

## 11. Key Design Decisions and Tradeoffs

### User messages, not system prompt

CLAUDE.md content is injected as user messages (`isMeta: true`) rather than into the system prompt. This enables prompt caching: the stable CLAUDE.md prefix can be cached across turns, while the system prompt is cached separately. If CLAUDE.md changed on every turn, it would bust the entire conversation cache.

### Eager + lazy split

Files at CWD and above are loaded eagerly (once per session). Files in subdirectories and conditional rules are loaded lazily (when triggered). This reduces initial context size while ensuring relevant instructions appear when needed.

### Dedup via non-evicting Set

The `loadedNestedMemoryPaths` Set prevents re-injection of the same CLAUDE.md across turns. This is necessary because `readFileState` is an LRU that evicts entries, and without the Set, eviction would cause the same file to be re-injected repeatedly.

### Conditional rules via frontmatter

Using YAML frontmatter `paths:` for conditional rules keeps the rule file self-contained and allows gitignore-style glob matching. This avoids needing a separate configuration file to map rules to file patterns.

### Worktree dedup

In git worktrees, the upward walk from CWD can pass through both the worktree root and the main repo root, which both contain checked-in files. The code detects this and skips Project-type files from the main repo to avoid loading the same content twice.
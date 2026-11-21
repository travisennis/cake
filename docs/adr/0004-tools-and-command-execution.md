# 0004 --- Tool semantics, command execution, and file mutation

- **Status:** Accepted
- **Date:** 2026-11-20
- **Governs:** `src/clients/tools/` except `sandbox/`, plus `src/clients/tools/mod.rs` path validation

## Context

Tools are the model's only way to act. Their schemas and errors are model-visible text, and their side effects are user-visible. Most of the subtlety is in failure paths: truncation, cancellation, binary output, process groups, temporary files, line endings, and repairs. A tool that fails open or leaves a half-written file is worse than a tool that returns an error, so each fallback below is deliberately chosen and must stay deliberate.

## Decisions

### Command resolution and execution

- A bare command word resolves through `PATH` as an executable. Otherwise the word is treated as a path relative to the tool's working directory, so `./script` and `subdir/script` work without shell parsing.
- A `DangerFullAccess` policy applies no sandbox, so a grant check would be stricter than the policy in force; the check is skipped in that case.
- Reserve the command budget before the gate check. The exact normalized text the gate evaluated is the text that executes, so authorization and execution cannot diverge.
- Empty commands have nothing to evaluate; `bash -c ""` is harmless and skips the gate.
- Apply the sandbox before scrubbing Git environment variables. Sandbox application may replace the `Command` (macOS wraps it in `sandbox-exec`), so the scrub happens after application. `GIT_DIR`, `GIT_WORK_TREE`, and their relatives can redirect Git away from the Bash working directory and must not escape into the child under any sandbox policy.
- The transcript call id links a Bash call to its gate request and recorded events. Telemetry digests the identifier at construction, so the raw id never reaches telemetry.
- An allowlist override removes a block from authorization, so the gate composer reports no deciding verdict. The block was still observed and overridden, and the legacy event must keep that record.
- Script evidence is collected before the gate request is sent. A collection failure therefore reuses the fail-closed block prefix and keeps its own wording; no gate request is sent and the command does not execute.
- JSON-escape model-supplied paths before embedding them in generated shell text so a path cannot inject control characters.

### Bash output and truncation

- Binary detection counts null bytes and non-printable bytes. A few null bytes strongly indicate binary data; more than roughly 30 percent non-printable content is treated as binary. High bytes are allowed because they may be valid UTF-8, which the UTF-8 check catches separately.
- `.bash_output` overflow and binary artifacts go to the per-session secure temp directory. If the directory cannot be created, the tool fails closed rather than writing to an unvalidated shared path. If the full output cannot be written, the inline truncated result is the fallback.
- `hit_cap` is set only when a read cap exists; the absence of a cap is not a hit.

### Process groups and cancellation

- Capture the process-group id **before** waiting. `Child::id()` returns `None` once the child is reaped, so resolving the group after the cooperative window would silently skip the forceful phase whenever the direct child exits on `SIGTERM` and leaves a descendant behind.
- Kill in two phases: a bounded cooperative window for the child to exit on its own, then a forceful kill of the captured group so a descendant that ignored `SIGTERM` does not survive the direct child that did not. Reap under a bound and discard the status: a timed-out run reports the timeout, not an exit code.
- Non-Unix has no cooperative signal, so the direct child is force-killed and reaped under the same bound.
- Hook, toolbox, gate, and bash-session children are each placed in their own process group for the same reason. A direct child exiting does not mean the group is gone; descendants may hold a captured pipe or outlive the shell, so the group is always signalled and the guard stays armed after an output-cap kill.
- The safety justification beside each `kill(2)` call is a `// SAFETY:` block and stays in code because it is a proof obligation for the unsafe call.

### Bash sessions

- Observe the direct shell independently of pipe EOF. Descendants may inherit a captured pipe and keep it open after the shell exits.
- The guard retains the process-group id from before the direct child was reaped. Drop it before draining output so descendants holding a pipe are stopped first, even when the shell exited normally.

### File reads and writes

- Read and ReadImage reject directories, pipes, sockets, and other non-regular objects before the blocking open. A named pipe with no writer never returns from a blocking open.
- The per-line cap bounds memory: a line is held only up to the cap plus room for its trailing newline, and the rest is consumed and discarded. A line of exactly the cap is delivered without a marker, with or without a CRLF ending. `scan_line` reports EOF without distinguishing a final unterminated line, so only an empty buffer means nothing was read. The cap is the `read_max_line_bytes` `[limits]` key (compiled default 10,000; `"unlimited"` restores the whole-line behavior) and `read-description.txt` documents it, so the two change together.
- A read window stops once it is satisfied and a bounded number of lines past the last emitted line has been counted for the footer. An unlimited budget accumulates the whole file by design.
- Write resolves the validated normalized path before deciding create versus overwrite, so a path that only exists after normalization is reported as an overwrite. Parent directories are created as needed.
- Edit and Write publish a complete replacement through a staged sibling so a failed preparation leaves the original file untouched. Edit validates and canonicalizes the path first so a read-only file is rejected before any write, preserves a UTF-8 BOM, detects the line-ending style, and matches on normalized LF while retaining original byte ranges. Edits apply highest position first. The preflight reports at most the first five match positions so one error does not dump the file.
- Edit refuses binary files before treating bytes as editable UTF-8, and checks metadata before opening a known oversized file; the bounded reader is still required because the file can grow after that check.
- CRLF normalization with no CRLF endings is the identity mapping and produces an empty marker list.
- Unified diff output requires a meaningful overlap so the context does not become noise.

### File replacement and JSON repair

- `StageFile`'s `Drop` performs cleanup as a fallback for panics and any future early return that bypasses `failure_message`; explicit failure paths already report cleanup failures.
- JSON argument repair is intentionally narrow: it escapes raw control characters inside string literals and nothing else. Truncating trailing data, quote resynchronization, and any repair that guesses intent are out of scope. If the repaired payload still does not parse, the original payload is returned so the caller sees the original error.
- MCP tool arguments apply the same conservative repair pass as the other external-tool protocols before parsing.

### Path validation

- Path grants include the symlink spelling first, then the canonical path. On macOS `/tmp` resolves to `/private/tmp` and `/var/folders` to `/private/var/folders`; both forms are needed so ancestor literals and subpath rules cover a path regardless of which spelling a process uses.
- A new file is resolved root-to-leaf with symlink-aware semantics for existing components and lexical normalization for the nonexistent suffix, so `<cwd>/missing/../target` resolves to `<cwd>/target`. The full path is reconstructed before validation so a credential denial also covers a target that does not exist yet, such as a new file under `~/.ssh` or a new `~/.npmrc`.
- A new target's base is validated against the same grant table and precedence as an existing file. `path_within` canonicalizes each grant, so a symlinked cwd, temp, or settings directory is accepted for new files exactly as it is for existing ones.

### Toolbox executables

- A toolbox executor repairs arguments before parsing but is never read-safe: it runs as an unsandboxed external process and may mutate host state, so it is dropped under the read-only sandbox policy.
- Kill the whole process group on timeout, output cap, cancellation, or early error so descendants cannot outlive cake's reported result. Keep the guard armed after an output-cap kill so it also reaches descendants that survived the direct child's kill and reap.
- Bounded reads keep a broken tool that streams endless output from exhausting memory before the timeout fires; the timeout envelope covers reaping, and an expired child is killed on drop.
- A tool that exits on its own may leave processes running deliberately; that is its business.
- Tool diagnostics go to cake's log file, not the model, because the describe and execute protocol reserves stdout for tool output.
- A capped tool killed after partial output is an output-truncation compensation even though the result is an error.
- Automatic project-local discovery may duplicate an explicitly configured directory. Deduplicate before running an unsandboxed `describe` twice while retaining the first source's precedence. The lexical check catches an explicit `.cake/tools` that is itself a symlink; the canonical check catches a directory resolving into another root's `.cake/` tree. Executable discovery follows symlinks, so a symlink to an executable is accepted.

### Secure temporary directory

- Cache the validated directory for the session but revalidate on every use. A temp cleaner or attacker may have removed, replaced, or changed it; `exists()` alone is insufficient because a symlink replacement passes. Full validation uses non-following metadata, ownership, and permissions.
- A UUID supplies 64 bits of randomness for the directory name. An attacker in a shared sticky-bit temp directory cannot pre-create the path because the suffix is unpredictable.
- If two threads race to create the directory, either result is usable; the unused directory is left for normal cleanup. A directory that is a symlink is removed and recreated as a real directory, permissions that drift are repaired, and a vanished directory is recreated.

### Scheduling and search

- Mutations always serialize with themselves. A read joins a same-path group only when the turn also mutates that path, so a read observes the mutation rather than racing it.
- Resolve mutating targets first. A mutation may precede a same-path read in issue order, and a turn with no resolved mutation cannot serialize anything, so the observe resolvers are skipped and ungroupable calls keep no target.
- Rank the whole tool catalog before applying a result limit. An active tool may outrank a new one, and hiding the new match would be misleading.
- The tool registry's descriptions compose from the available tool set, but the BashSession description is essential even in a BashSession-only selection because it describes Bash output and IDs without recommending Bash itself.

## Consequences

These invariants are cheap to break with a plausible-looking simplification: dropping the captured pgid, replacing the staged write with an in-place one, accepting a symlink in the temp directory, or adding a new repair that guesses intent. Review changes to these modules against this record, not against the shape of the code.

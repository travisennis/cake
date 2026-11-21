# 0006 --- Configuration, sessions, hooks, gates, and trusted extensions

- **Status:** Accepted
- **Date:** 2026-11-20
- **Governs:** `src/config/`, `src/session_telemetry.rs`, `src/hooks.rs`, `src/gates/`, `src/types/session.rs`, `src/types/conversation.rs`

## Context

This layer decides what configuration is trusted, how a session is written and resumed, and how trusted extensions (hooks, gates, toolbox tools, MCP servers) are launched. Many invariants guard append-only persistence and process cleanup: once a session record is partially written or a child process escapes its group, no later retry can repair it. Settings precedence and the persisted record shape are compatibility contracts.

## Decisions

### Settings and model identity

- Settings files with unrecognized keys load normally because the typed loader silently drops them. Validation parses the raw document once to compare against the typed result, then surfaces unknown keys so a typo is visible. The sentinel `"unlimited"` is serialized back to its written form so the round trip keeps the key present and does not report a false unknown-key warning.
- The `[[models]]` validation order is deterministic: push the base entry first so name validation reports it before any generated variant, then clone it as the template for variants. Name `[[models]]` entries by their `name` field rather than their array index so warnings read like the settings file.
- A model name must match `[a-z0-9][a-z0-9-]{0,63}`. Duplicate names are rejected within one file and across the merged map.
- A configured endpoint is part of provider identity and is persisted in session metadata, so it must be a real URL with no embedded userinfo credential.
- `api_key_env` is persisted verbatim in the session provider identity, so it must name an environment variable; a pasted key or other non-variable text is rejected before it can reach a snapshot or a mismatch diagnostic.
- The tool allowlist is a union across precedence levels: every explicit entry in any settings file is honored, and an absent key contributes nothing, matching the other list-valued settings.
- Resolved grants use ordered sets: deduplication without losing stable ordering.
- Legacy judge tuning, including allowlists, is kept while its execution moves to the gate plugin. No credential or policy is copied into Cake. A profile-only TypeSafe overlay is still a legacy judge table for migration purposes even when no lower-precedence file has one.
- The profile merge calls `merge_typesafe_settings` unconditionally: it is a no-op on `None`, and the unconditional call avoids a branch that would not change behavior.

### Sessions and persistence

- A session writer that fails enters a terminal state. `into_parts` detaches the buffer instead of flushing it on drop, so a failed writer cannot append a partial record later. The locked file stays owned by the shared state until every clone drops, so no other writer can acquire the session lock meanwhile.
- Serialize while the mutex is held. A failure before serialization completes must fail the shared writer, not produce unconsumed bytes.
- A JSONL line not terminated by `\n` necessarily read the rest of the stream: it is the final line and a partial tail. Loading tolerates that partial final line; an interrupted write must not make the session unreadable.
- Session payload blobs are content-addressed and published before the record that references them. A trailing separator would make `symlink_metadata` follow a final symlink on Unix, so all blob operations use the normalized form. `O_NONBLOCK` closes the race where a regular target is replaced by a FIFO after the metadata check; the post-open shape check still rejects it.
- `lstat` follows a symlink when the path ends in a separator. `CAKE_DATA_DIR` validation therefore checks a lexically normalized path, or `/tmp/link/` would pass the symlink check and resolve through the link.
- A session JSONL file is append-only and versioned. Replay never writes to a session file, so `replay_error` has no persisted counterpart; it exists only in the replay path.
- Checkpoint externalization happens before measuring, because a checkpoint whose inline payloads would exceed the record ceiling is accepted when its references fit. The writer re-externalizes and re-verifies under its lock and repeats the validation.

### Session forking and resumption

- Seed once-per-session activation deduplication from persisted records so a resumed run does not re-emit "first observed" for known skills.
- Re-envelope the seed's checkpoint for the new session before restoring it, so the in-memory checkpoint and the copied record agree.
- A fork's session file starts with copies of the source session's `SkillActivated` records; seed the same names so re-reads do not emit duplicates.
- Copy only the latest checkpoint, re-enveloped for the fork at its original position among the copied records, so the positional boundary is reconstructed. A checkpoint-free format-4 fork copies exactly what it copied before.
- A fork copies the parent snapshot. `--allow-session-model-change` accepts the current resolution for this invocation without rebinding the fork to a new destination.
- A fork removes only a blob directory it created. A pre-existing directory may hold another writer's data and is left in place.
- Recovery is best-effort: a missing blob directory skips the scan, and legacy inline seed payloads are externalized before the child file exists. A failure after that point is reported rather than silently producing a fork without its blobs.
- A semantic doctor failure merges first and fails later, so no single file owns the blame; the diagnostic names the complete set.

### Hooks

- The discriminant comparison in the hook event `const fn` stands in for `==`: `PartialEq::eq` is not `const`, so `self == event` cannot be used. `HookEvent` declares no explicit discriminants, so each variant's discriminant is unique and identifies it.
- A hook child is placed in its own process group. A drop guard records the child PID and kills the whole tree on any non-successful exit (timeout, cancellation, early error), and is defused on the success path, including when the child has already exited and the group is empty.
- Feed stdin from a detached task so a hook that never reads stdin cannot block the call. Write errors are ignored because exit status is authoritative and the enclosing timeout bounds the writer.
- The timeout bounds the whole read and wait. `wait_for_hook_output` reads both streams with bounded buffers, so a hook that emits more than the output limit cannot exhaust memory; each stream buffers at most the limit and only counts the overflow.
- `kill_on_drop(true)` may already have killed the immediate shell when a read future is dropped; the guard sends `SIGKILL` to the process group to reach descendants.

### Gates

- A gate's `describe` action is a one-shot process whose stdout may have been closed by a descendant after the direct child exited. The group still needs forceful cleanup, and the later serve process is a different group.
- A restart's ready handshake does not reset the circuit breaker. A gate that crashes on every check would otherwise restart and handshake forever. A completed check resets the count.
- The direct child exiting does not mean the process group is gone: a gate may have spawned descendants. Always signal the group so descendants cannot outlive the run, even when the direct child exited successfully.
- Subscription credentials never cross the process boundary.
- Gate survivors are killed by the same group-first pattern as hooks, with the same `// SAFETY:` justification at the `kill(2)` call.

### Skills

- Strict YAML parsing is attempted first. A lenient fallback exists for real-world frontmatter that uses unquoted colons; it extracts only `name` and `description` and is deliberately bounded to those fields.
- `serde_saphyr` keeps the trailing newline of a block scalar even when the frontmatter has no final line break. Trim trailing whitespace so catalog and prompt rendering stay byte-identical to the previous parser's output.
- Skill discovery scans project paths first, then configured paths, then user paths. Project shadows configured; project and configured shadow user. A duplicate name within one scope keeps the first found and emits a warning, so a collision is never merged.
- Directory walking skips excluded directories and recognizes a `SKILL.md` file in a directory as a skill. Scanning still recurses into that directory's subdirectories, so a skill directory may contain nested skills.

### Worktrees

- Worktree creation is one transaction after `git worktree add`: copy `.worktreeinclude` files, and on any later failure roll back the registration and the directory so a reported setup failure does not leave a partial worktree.
- Cleanup dereferences the worktree and removes the directory, then deletes the branch this call created. An absent branch is treated as already gone; if the directory survives git's removal, remove it directly because creation verified it was absent and therefore owns it.
- A worktree is removable only when it has no uncommitted changes and no commits missing from the default remote branch. If no upstream exists, check whether the branch has any commits at all.
- `.worktreeinclude` matching uses the non-following file type. `Path::is_dir` and `Path::is_file` follow symlinks, which would let a symlink cycle recurse without bound and let symlinks escape the repository; symlinks are skipped entirely. Internal git and cake directories are skipped, and directories whose contents cannot match any pattern are pruned.
- The glob matcher treats `**` as crossing `/` and `*` as matching within one segment. Patterns without `**` are fixed depth and must have more segments than the directory to leave room for a filename. Patterns with `**` can expand to any depth, and a wildcard segment can match any single directory component.

### Telemetry

- A poisoned telemetry mutex held a writer that may have left a partial NDJSON line. Recover the state so the mutex stays the single availability authority, then fail stop: never write after a partial line. Exactly one caller observes `Failed`; later callers observe `Disabled`.

## Consequences

The persisted record shape, settings precedence, hook and gate protocols, and worktree rollback are compatibility surfaces. A change to any of them updates this record and `docs/integrations.md` together.

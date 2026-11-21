# 0002 --- CLI resolution, startup ordering, interrupts, and exit classes

- **Status:** Accepted
- **Date:** 2026-11-20
- **Governs:** `src/main.rs`, `src/cli/`, `src/exit_code.rs`, `src/logger.rs`, `src/prompts/`

## Context

The CLI is the compatibility surface: flags, `--help`, exit codes, stdout and stderr, completion JSON, and stream JSON are consumed by scripts and CI. Startup also decides which configuration sources are trusted and which paths are anchored where. The order of those steps is load-bearing: a failed input must not leave a registered worktree, a credential read must not mask a model mismatch, and an interrupt must still close the session with a matching record.

## Decisions

### CLI shape

Clap derives one `bool` per `no_*` flag. Grouping those switches into enums would change the CLI shape consumers depend on, so the flat flags stay. Doc comments on the CLI struct are the source of `--help` text and are therefore exempt from the comment ban.

### Startup ordering

- Check the stored session model entry **before** resolving credentials. Resolving a Codex-backend entry reads the local auth file; doing that first would either mask a model mismatch or make an auth file necessary just to report one.
- Validate stdin content, the prompt, and the output schema **before** creating a worktree. An input error must not leave a stale worktree registered.

### Path anchoring

- Canonicalize the working directory at startup into a stable absolute path. Later fixes may change the process cwd (`--worktree`), and every configured relative path stays anchored to the invocation directory.
- Project-local toolbox discovery follows the active worktree so `--worktree` selects that worktree's `.cake/tools`. Explicitly configured directories keep invocation-directory anchoring.
- `--no-project-config` must not be re-enabled by an explicit `--toolbox`, `CAKE_TOOLBOX`, or MCP config path pointing inside either checkout's `.cake/` tree. A wrapper cannot re-authorize project definitions the flag excluded.
- The CLI tool selection replaces the settings chain. Resolve it once, carry the effective list on `RunResources`, and let both the session builder and the unavailable-name warning read that value.

### Trusted extension discovery

- Reconcile MCP servers and toolbox tools after the working directory and config sources are final. Toolbox executables are user code: broken tools are skipped with a warning and never block startup, and under the read-only sandbox policy they are never executed at all, including `describe`, because even discovery runs outside the OS sandbox and could mutate the workspace. MCP servers are treated the same way.
- Under the read-only policy an explicit MCP config path is still validated, so a typo is not silently ignored.
- Warn early when a linked worktree's gitdir cannot be resolved. The resolution happens before any Bash call so the user sees the problem at session start rather than when git first fails; the detailed reason goes to the file-only tracing log, and a user-visible stderr line is emitted separately because tracing is not shown in the terminal.

### Task lifecycle and interrupts

- The agent turn begins after all setup. Everything before that boundary is startup and must be able to fail without a `TaskStart` that has no matching end.
- Install interrupt listeners **before** the turn starts. Tokio registers a signal's OS handler on the first poll of its listener, so creating the listener lazily inside `select!` leaves a window where an interrupt kills the process outright and no `TaskComplete` record is written.
- Race Ctrl-C and SIGTERM against the turn. A user interrupt is not an error the process hit itself, so the status protocol reports `idle`, the session gets a `TaskComplete` with `Interrupted`, telemetry records `success=false`, and the process exits `130`. Worktree cleanup runs through `WorktreeGuard`'s `Drop`.
- Catch a second interrupt during cleanup. Once a SIGTERM handler is installed the default termination is gone, so a second SIGTERM must be handled explicitly beside Ctrl-C; the listener is left detached as the hard escape hatch and the runtime tears it down at exit. Restore SIGTERM's default disposition once the turn is over, or a later SIGTERM during rendering or teardown would be swallowed.
- If the turn begins and fails before producing a result (a failing session-start hook, an unwritable completion record), report the failure instead of leaving the terminal on `working` until the process exits.
- A cut-off stays an `Err` so every render mode and the telemetry summary reports it as a failure, while `handle_agent_turn_result` has already emitted the distinct `CutOff` record. A cut-off cannot continue, so its follow-up hook sees a zero budget.
- A terminally failed session writer rejects further appends. Hook audit records are best-effort and must never reopen a failed session file, and the failure that ended the task remains the primary diagnostic: reporting the writer's rejection instead would hide the original cause, such as a checkpoint that could not be committed.
- Limit-exceeded errors are classified by `task_outcome_for_error`, which emits the distinct `limit_exceeded` outcome and runs the error hook.
- After `task_start`, before `send()`, repair and re-render the conversation so the first stream record opens the invocation and the provider receives the repaired history.
- Print the original answer before spending a follow-up turn; the user has already seen it. If the consumer stops reading, close the invocation without a follow-up turn.

### Session command behavior

- `CmdRunner` is asynchronous even when a command has no async work because the trait is shared; that is why the setup commands carry `async` with no `await`.
- `cake replay` mirrors the live stream vocabulary and physical record order, emits main-agent `turn_usage` records verbatim (including records written before optional context-budget fields existed), omits compaction-purpose usage as audit-only data, and strips blob references after hydration so consumers never see the storage shape. It does not pre-check `Path::exists`, because that returns `false` for permission-denied paths and would misclassify them as `session_not_found`; `File::open` distinguishes the cases. It reads through the shared framing codec, maps open errors locally, and tolerates a partial final line exactly as `Session::load` does.
- Session listing sorts newest first with the session id as the tiebreaker so two sessions written in the same second always list in the same order. It scans raw `serde_json::Value` records rather than full `SessionRecord` deserialization so a forward-compatible record cannot break listing, and reports the first line of the first user message as the prompt.
- A terminal cut-off in JSON output is an additive discriminator so consumers can tell it apart from other agent errors, matching stream-json's `task_complete` subtype. The error carries the concise detail; the partial result is surfaced in the result position and serializes as `null` when absent, like other errors.
- `cake doctor` resolves configured grants without reading a skill file because grants do not depend on the skill catalog.

### Exit classes

The exit code is part of the contract: `0` success, `1` agent or tool error, `2` API error including rate limit, auth, network, and request timeout, `3` input error, `130` interrupted. Classification walks the typed error chain first and only then string patterns, so status policy lives in one place:

- A no-data stall is a request timeout and shares the API-error class with client-side `reqwest` timeouts and provider `408`s.
- Typed output-schema errors are input errors when the schema file or arguments are unusable before the run, and agent errors when the run cannot satisfy the schema.
- Settings load and parse failures are input errors: the file the user supplied is the input cake rejected.
- A gate pipeline failure carries the protocol's failure class: provider failures are API errors, configuration failures are input errors, and everything else is an agent error. This preserves the classes callers saw before the in-process judge moved behind the gate.
- Structured commands (`cake replay`, `cake debug session`, `cake sessions checkpoint`) own their documented failure mappings.
- Status-classified failures defer to `classify_api_error_status`; string matching covers only network and connection failures that surface without a typed status, such as an error re-wrapped by `anyhow`.

### Prompt resolution and logging

- The system prompt resolves highest to lowest: `--system-prompt`, `<cwd>/.cake/system.md`, the settings `system_prompt` path, the user config `system.md` (normally `~/.config/cake/system.md`), then the embedded default. The first readable file wins and replaces the default entirely; an empty file is a valid intentional blank prompt, and an unreadable file is skipped with a warning.
- Omitting a skill from the prompt is allowed only when the run can search the complete catalog. Without `SkillSearch` the catalog stays complete and the over-budget warning names the missing search surface.
- The logger defaults cake to `INFO` and lets `RUST_LOG` override it, with daily rotation and seven-day retention.

## Consequences

A change to startup ordering, path anchoring, interrupt handling, or an exit mapping must update this record. Those behaviors are externally visible and cannot be inferred safely from the call graph alone.

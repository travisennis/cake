# Security and trust boundaries

This document defines Cake's security intent and limitations. It is the authority for permissions and trust; implementation details belong in the sandbox and tool code.

## Threat model

Cake treats model-generated tool calls and shell commands as untrusted. It aims to limit filesystem effects to paths allowed by the selected policy and to block a small set of known-destructive command patterns before execution.

Cake does not attempt to make an untrusted model safe to run with arbitrary credentials, network access, trusted hooks, or toolbox executables. The user remains responsible for the repository, configuration, secrets available to child processes, and authority granted through sandbox policy.

## Sandbox policies

`--sandbox` selects:

- `read-only`: the workspace, configured directories, and built-in toolchain paths are read-only. Cake-managed temporary paths remain writable so commands can produce intermediate output. Mutating built-in tools and toolbox tools are not offered to the model.
- `workspace-write`: the default. The working directory, linked-worktree Git directories, Cake-managed temporary paths, configured writable directories, and built-in toolchain and integration paths may be modified. Built-in grants cover common package managers, runtime managers, language caches, and CLI state such as `~/.cargo`, `~/.npm`, and `~/.config/gh`; the sandbox implementation is the exact list.
- `danger-full-access`: Cake does not apply its filesystem sandbox to Bash.

An explicit CLI policy takes precedence over `CAKE_SANDBOX`. For compatibility, `CAKE_SANDBOX=off` selects danger-full-access when no flag is present.

`--add-dir` adds a read-only path for one invocation. `directories` in settings adds persistent read-write paths. The `[sandbox]` section in settings adds persistent grants in two classes: `read_only` (read + execute, for files or directories) and `writable` (read + write + execute). Treat all of these as grants of authority.

The `[sandbox]` and `directories` path lists feed both the in-process Read/Edit/Write/Grep validation and the OS sandbox, so the two enforcement layers cannot diverge. A `read_only` entry naming a single executable grants exactly that file (plus read access to its ancestor directories), so sibling files in the same directory remain denied.

Project-level `.cake/settings.toml` is fully trusted by design, the same trust model as the rest of project `.cake/` configuration. There is no deny-list and no trust prompt: any path a project declares in `[sandbox]` becomes accessible to model-generated commands. Treat a cloned repository's `.cake/settings.toml` the way you treat its hooks.

## Enforcement layers

Read, Edit, and Write validate target paths in-process. Bash first applies best-effort deterministic command checks, then runs under an OS filesystem sandbox:

- macOS uses Seatbelt through `sandbox-exec`;
- Linux uses Landlock and requires a kernel capable of fully enforcing the configured ruleset.

Sandbox availability errors fail closed. On macOS, a process already inside Seatbelt may receive the recognized nested-profile `sandbox_apply: Operation not permitted` failure. In that one case Cake warns and relies on the inherited parent sandbox. The parent policy may be more or less restrictive than Cake's selected policy.

Command checks are workflow protection, not a shell parser or security boundary. They cannot recognize every equivalent spelling or prevent every remote side effect.

For operational diagnosis and platform-specific recovery, follow the [Debugging Sandbox Denials runbook](runbooks/debugging-sandbox.md).

## What the sandbox does not restrict

The Bash sandbox is a filesystem boundary. Network access remains available. Commands may reach remote services using credentials readable within their environment and allowed paths. Remote Git history, APIs, databases, and other network effects are outside the filesystem sandbox.

The sandbox also cannot promise confidentiality from a model or provider once content is included in a prompt, tool result, hook context, or API request.

## Trusted extensions

Hooks are user or project control-plane commands. Toolbox tools are user-provided executables exposed to the model. Both run with the Cake process's ambient host authority, outside the Bash sandbox.

Only enable hook files and toolbox directories you trust. A cloned repository does not automatically contribute toolbox executables, but explicitly adding a project directory is a trust decision. Read-only mode skips toolbox discovery; hooks remain trusted control-plane behavior.

## Sessions, logs, and telemetry

Session transcripts can contain prompts, model responses, tool calls, tool outputs, hook records, paths, and other sensitive project information. Protect `~/.local/share/cake/sessions/` or the configured data directory accordingly.

Telemetry sidecars intentionally omit prompt text, assistant text, and raw tool output bodies, but still contain operational metadata. Logs can contain errors, paths, hook diagnostics, and provider information.

## Security-sensitive changes

Changes to allowed paths, sandbox policy, fallback behavior, command checks, hook/toolbox execution, credential handling, transcript contents, or network policy require:

- explicit compatibility and security-impact analysis;
- focused allow/deny tests;
- macOS and Linux consideration;
- fail-closed behavior review;
- updated documentation when the guarantee or limitation changes.

The LLM judge (see ADR-018) is the command-safety gate above the OS sandbox; the corpus at `src/clients/tools/corpus/commands.jsonl` holds the regression cases the judge-driven runner (#174) evaluates.

Convenience is not sufficient justification for widening authority.

### Enumerate bypass classes first

Before editing a security boundary, enumerate the bypass classes the change must defend against. Record them with the change so review has something to check against.

A review-reported bypass class that was not enumerated is a signal that the design is wrong, not that another check is missing. Patching each reported bypass in turn converges slowly or not at all: a boundary that can only be held by enumerating evasions is not a boundary. Stop and revisit the approach instead.

Validating an untrusted command string by parsing it is the recurring instance of this. Shell quoting, expansion, chaining, symlinked targets, and a child process's own configuration flags each reopen the boundary independently, so a parser that rejects today's evasions does not constrain tomorrow's.

## Related decisions

- [ADR 014](adr/014-sandbox-policy-cli-flag.md), the sandbox policy flag.
- [ADR 015](adr/015-declarative-command-policy.md), declarative command policy.
- [ADR 016](adr/016-nested-seatbelt-sandbox-fallback.md), the recognized nested-Seatbelt fallback.
- [ADR 017](adr/017-trusted-executable-toolbox-tools.md), trusted toolbox executables.
- [ADR 019](adr/019-project-customizable-sandbox-paths.md), project-customizable sandbox paths.

# Security and trust boundaries

This document defines Cake's security intent and limitations. It is the authority for permissions and trust; implementation details belong in the sandbox and tool code.

## Threat model

Cake treats model-generated tool calls and shell commands as untrusted. It aims to limit filesystem effects to paths allowed by the selected policy and to judge every Bash command for destructive intent before execution.

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

`cake init` stays inside this trust model: its `.cake/settings.toml` is commented and behavior-preserving, with no sandbox grants, judge allowlist entries, or model selection; its `.cake/hooks.json.example` is inert because Cake never loads `.example` files.

## Enforcement layers

Read, Edit, and Write validate target paths in-process. Bash runs every command through the LLM judge before spawn, then runs under an OS filesystem sandbox:

- macOS uses Seatbelt through `sandbox-exec`;
- Linux uses Landlock and requires a kernel capable of fully enforcing the configured ruleset.

The LLM judge (ADR-018) is the command-safety gate above the OS sandbox. It replaced the compiled `bash_safety` guard: there is no deterministic rule floor. The judge is default-on and fail-closed --- a `block` verdict or any judge failure (missing context, unresolvable model, rubric read failure, timeout, transport error, or malformed verdict) prevents the command from running; a `warn` verdict runs it with guidance prepended. Judge failures remain fail-closed after at most one recovery within `timeout_secs + retry_budget_secs`. Because the gate is stochastic, its verdicts are model-dependent and must be measured, not trusted: every judge decision (verdict + code + latency, fail-closed class, or bypass) is recorded in the session telemetry sidecar, and the regression corpus at `src/clients/tools/corpus/commands.jsonl` is evaluated by the judge-driven runner (#174).

The judge's only override surface is an explicit allowlist of exact raw-command strings: an allowlisted command is still judged, and a `block` verdict is overridden but recorded with an `overridden` flag. An emergency bypass (`CAKE_JUDGE=off` or `tools.bash.judge.enabled = false`) disables the judge for every command, emits a bypass telemetry event per call, and is off by default. The accepted risks are non-determinism (the same command may be judged differently across calls, models, or days), latency and cost on the hottest tool, and correlated prompt-injection failure --- the judge is weakest exactly where it is needed most. The OS sandbox remains the filesystem boundary; effects the sandbox cannot bound (in-project destruction, remote Git effects) are the residual risk surface.

The judge is workflow protection above the OS sandbox, not a shell parser or a security boundary by itself: it cannot guarantee that every equivalent spelling is recognized, and it covers effects the sandbox cannot (such as remote git pushes).

The judge is stateless: each evaluation sees only the command, its working directory, the compact repository digest, and the model's optional reason. It has no conversation history, earlier command results, or tool outputs, so block and warn remediation recommends one self-contained command or guarded sequence whose safety the next request can evaluate alone. The judge cannot verify a reason's claims, and a reason never authorizes a remote destructive command: it may state intent as context, but a merge or branch delete is safe only with an in-command guard; a claim that a pull request is merged or authorized never makes `git push origin --delete <branch>` or `gh pr merge` safe. The judge also evaluates a guard as text: it cannot verify the remote state a guard reads or that the execution environment is unmodified, so a remote-effect guard is workflow guidance with a documented limitation rather than a hard guarantee.

Sandbox availability errors fail closed. On macOS, a process already inside Seatbelt may receive the recognized nested-profile `sandbox_apply: Operation not permitted` failure. In that one case Cake warns and relies on the inherited parent sandbox. The parent policy may be more or less restrictive than Cake's selected policy.

For operational diagnosis and platform-specific recovery, follow the [Debugging Sandbox Denials runbook](runbooks/debugging-sandbox.md).

## What the sandbox does not restrict

The Bash sandbox is a filesystem boundary. Network access remains available. Commands may reach remote services using credentials readable within their environment and allowed paths. Remote Git history, APIs, databases, and other network effects are outside the filesystem sandbox.

The sandbox also cannot promise confidentiality from a model or provider once content is included in a prompt, tool result, hook context, or API request.

## Trusted extensions

Hooks are user or project control-plane commands. Toolbox tools are user-provided executables exposed to the model. Both run with the Cake process's ambient host authority, outside the Bash sandbox.

`PreToolUse` hooks are a primary enforcement path for in-process mutations (Edit and Write) and for toolbox calls, which run unsandboxed. Hooks evaluate the same structured `tool_input` the tool will act on, so a payload whose raw JSON is malformed but repairable cannot bypass a hook that inspects the tool's structured arguments.

Only enable hook files and toolbox directories you trust. A cloned repository does not automatically contribute toolbox executables, but explicitly adding a project directory is a trust decision. Read-only mode skips toolbox discovery; hooks remain trusted control-plane behavior.

## Sessions, logs, and telemetry

Session transcripts can contain prompts, model responses, tool calls, tool outputs, hook records, paths, and other sensitive project information. Protect `~/.local/share/cake/sessions/` or the configured data directory accordingly.

Telemetry sidecars intentionally omit prompt text, assistant text, raw tool output bodies, judge command/reason/cwd values, provider response bodies, credentials, and authorization headers, but still contain operational metadata. Judge-attempt records include model controls, prompt byte counts, phase timing, attempt/retry ordinal, retry reason, backoff wait, effective deadline, status, request identity digests (never raw provider-controlled identifiers), termination, and token usage when available.

`cake bash check --diagnostic` is a separate, explicit raw inspection surface. Its stdout contains the effective judge prompts and transformed request JSON, so it exposes the inspected command, working directory, compact repository state, optional model-supplied reason, and any secrets already embedded in those values. It also renders parsed response metadata. Treat that output like a session transcript and avoid redirecting or sharing it unless the destination is trusted. Cake omits its resolved API key, authorization headers, configured provider headers, and unrelated environment variables; enabling the flag does not make normal Bash preflight telemetry raw.

## Security-sensitive changes

Changes to allowed paths, sandbox policy, fallback behavior, command checks, hook/toolbox execution, credential handling, transcript contents, or network policy require:

- explicit compatibility and security-impact analysis;
- focused allow/deny tests;
- macOS and Linux consideration;
- fail-closed behavior review;
- updated documentation when the guarantee or limitation changes.

Convenience is not sufficient justification for widening authority.

### Enumerate bypass classes first

Before editing a security boundary, enumerate the bypass classes the change must defend against and record them for review.

A review-reported bypass class that was not enumerated signals a wrong design, not a missing check. Patching each reported bypass converges slowly or not at all: a boundary held only by enumerating evasions is not a boundary. Stop and revisit the approach instead.

Validating an untrusted command string by parsing it is the recurring instance of this. Shell quoting, expansion, chaining, symlinked targets, and a child process's own configuration flags each reopen the boundary independently, so a parser that rejects today's evasions does not constrain tomorrow's.

## Related decisions

- [ADR 014](adr/014-sandbox-policy-cli-flag.md), the sandbox policy flag.
- [ADR 016](adr/016-nested-seatbelt-sandbox-fallback.md), the recognized nested-Seatbelt fallback.
- [ADR 017](adr/017-trusted-executable-toolbox-tools.md), trusted toolbox executables.
- [ADR 018](adr/018-llm-judge-command-gate.md), the LLM judge command gate.
- [ADR 019](adr/019-project-customizable-sandbox-paths.md), project-customizable sandbox paths.

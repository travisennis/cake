# Security and trust boundaries

This document defines Cake's security intent and limitations. It is the authority for permissions and trust; implementation details belong in the sandbox and tool code.

## Threat model

Cake treats model-generated tool calls and shell commands as untrusted. It aims to limit filesystem effects to paths allowed by the selected policy and to judge every Bash command for destructive intent before execution.

Cake does not attempt to make an untrusted model safe to run with arbitrary credentials, network access, trusted hooks, or toolbox executables. The user remains responsible for the repository, configuration, secrets available to child processes, and authority granted through sandbox policy.

## Sandbox policies

`--sandbox` selects:

- `read-only`: the workspace, configured directories, and built-in toolchain paths are read-only. Cake-managed temporary paths remain writable so commands can produce intermediate output. Mutating built-in tools and toolbox tools are not offered to the model.
- `workspace-write`: the default. The working directory, linked-worktree Git directories, Cake-managed temporary paths, configured writable directories, and built-in toolchain and integration paths may be modified. Built-in grants cover common package managers, runtime managers, language caches, and CLI state such as `~/.cargo`, `~/.npm`, and `~/.config/gh`; the supplementary `~/Library/Keychains` file grant remains read-only.
- `danger-full-access`: Cake does not apply its filesystem sandbox to Bash.

An explicit CLI policy takes precedence over `CAKE_SANDBOX`. For compatibility, `CAKE_SANDBOX=off` selects danger-full-access when no flag is present.

The Bash tool's optional per-call `cwd` selects an existing directory that the sandbox already grants: the invocation working directory, a read-write grant (a `[sandbox]` or `directories` entry, a Cake-managed temporary path, or a built-in toolchain cache), or a user read-only grant (`--add-dir`, a `[sandbox].read_only` entry, or a skill directory). Relative paths resolve from the invocation working directory, and canonicalization rejects a symlink whose target is outside every grant. The built-in system read-and-execute paths (`/etc`, `/usr`) stay readable by absolute path but are not selectable, and `danger-full-access` admits any existing directory because it applies no profile. Selecting a directory adds no grant, changes no profile, and persists to no later call: the sandbox still governs every access from wherever the command starts, so a `read-only` run denies a write inside a directory it selected.

`--add-dir` adds a read-only path for one invocation. `directories` in settings adds persistent read-write paths. The `[sandbox]` section in settings adds persistent grants in two classes: `read_only` (read + execute, for files or directories) and `writable` (read + write + execute). Treat all of these as grants of authority.

`tools.enabled` is a separate narrowing allowlist for the model-visible and executable tool registry. It cannot add authority: sandbox policy still removes tools that are unsafe under the selected policy, and an absent key preserves the default tool set.

The `[sandbox]` and `directories` path lists feed both the in-process Read/Edit/Write/Grep validation and the OS sandbox, so the two enforcement layers cannot diverge. A `read_only` entry naming a single executable grants exactly that file (plus read access to its ancestor directories), so sibling files in the same directory remain denied.

The OS sandbox represents ordinary filesystem grants with two effective path classes: read + write + execute, and read + execute without write. Built-in system and configuration paths, user read-only grants, skill paths, paths demoted by the `read-only` policy, and the supplementary `~/Library/Keychains` file grant share the second class. Platform-only capabilities such as macOS device, SSH agent, Keychain service access, Mach, process, and network rules stay separate because they are not ordinary filesystem path grants; Keychain service access remains mediated by Mach.

Project-level `.cake/settings.toml` is fully trusted by design, the same trust model as the rest of project `.cake/` configuration. There is no deny-list and no trust prompt: any path a project declares in `[sandbox]` becomes accessible to model-generated commands. Treat a cloned repository's `.cake/settings.toml` the way you treat its hooks.

`cake init` stays inside this trust model: its `.cake/settings.toml` is commented and behavior-preserving, with no sandbox grants, judge allowlist entries, or model selection; its `.cake/hooks.json.example` is inert because Cake never loads `.example` files.

## Enforcement layers

Read, Edit, and Write validate target paths in-process. Bash runs every command through the LLM judge before spawn, then runs under an OS filesystem sandbox:

- macOS uses Seatbelt through `sandbox-exec`;
- Linux uses Landlock and requires a kernel capable of fully enforcing the configured ruleset.

The LLM judge (ADR-018) is the command-safety gate above the OS sandbox. It replaced the compiled `bash_safety` guard: there is no deterministic rule floor. The judge is default-on and fail-closed --- a `block` verdict or any judge failure (missing context, unresolvable model, rubric read failure, timeout, transport error, or malformed verdict) prevents the command from running; a `warn` verdict runs it with guidance prepended. Judge failures remain fail-closed after at most one recovery within `timeout_secs + retry_budget_secs`. Because the gate is stochastic, its verdicts are model-dependent and must be measured, not trusted: every judge decision (verdict + code + latency, fail-closed class, or bypass) is recorded in the session telemetry sidecar, and the regression corpus at `src/clients/tools/corpus/commands.jsonl` is evaluated by the judge-driven runner (#174).

Judge verdicts must be complete, valid JSON objects. One Markdown code block with an empty or `json` language tag and surrounding whitespace is accepted, but explanatory text, trailing data, extra JSON values or blocks, malformed fences, and unescaped control characters in strings fail closed. The judge does not use tool-argument JSON repair: a valid `allow` prefix cannot authorize a command when the rest of the response is malformed. Providers must emit escaped control characters (for example, `\n`) inside JSON strings.

Before spawning Bash, Cake removes `config::git::AMBIENT_ENV_VARS` from the child environment after sandbox application. This keeps Git commands that inherit Cake's environment anchored to the Bash working directory, including when Cake itself was launched from a Git hook or another Git operation. The removal is unconditional across sandbox policies; a command that explicitly sets its own environment remains responsible for that choice.

The judge's only override surface is an explicit allowlist of exact raw-command strings: an allowlisted command is still judged, and a `block` verdict is overridden but recorded with an `overridden` flag. An emergency bypass (`CAKE_JUDGE=off` or `tools.bash.judge.enabled = false`) disables the judge for every command, emits a bypass telemetry event per call, and is off by default. The accepted risks are non-determinism (the same command may be judged differently across calls, models, or days), latency and cost on the hottest tool, and correlated prompt-injection failure --- the judge is weakest exactly where it is needed most. The OS sandbox remains the filesystem boundary; effects the sandbox cannot bound (in-project destruction, remote Git effects) are the residual risk surface.

The judge is workflow protection above the OS sandbox, not a shell parser or a security boundary by itself: it cannot guarantee that every equivalent spelling is recognized, and it covers effects the sandbox cannot (such as remote git pushes).

The judge's `credential-disclosure` class is its policy for secret disclosure. It blocks a command that prints a secret the request does not need, in two families: disclosure no path bound can stop --- environment dumps (`printenv`, `env`, `/proc/self/environ`), another process's environment, and container environment inspection (`docker inspect --format '{{json .Config.Env}}'`) --- and reading a credential store or secret-bearing file (`~/.npmrc`, `.env`, `~/.netrc`, `~/.aws/credentials`, `~/.git-credentials`, `~/.ssh/id_rsa`, shell history, keychain dumps). A path-bounded read is covered even though the sandbox would deny it, because that denial is opaque to the agent: a denied read teaches the model nothing, while a block names the safer alternative. A reason's claimed need or claimed user authorization never makes a credential one the request needs. Deliberately granted, non-credential reads (`cat ~/.gitconfig`), one named variable or status (`printenv PATH`, `gh auth status`), a
process listing without environments (`ps aux`), a variable-setting wrapper (`env VAR=value cmd`), and listing, counting, keys-only, or existence forms (`ls -l ~/.ssh`, `wc -l ~/.zsh_history`, `cut -d= -f1 .env`, `test -f ~/.ssh/id_ed25519`, `git config --list`) stay allowed, and a public key such as `~/.ssh/id_ed25519.pub` is not a secret. Because the gate is stochastic, the class is a policy statement with a measured regression corpus rather than a guarantee: a credential a child process can read stays reachable by a command the judge approves, and a process's own environment has no path bound on either platform. The platform split matters for the rest: `/proc/<pid>/environ` is Linux-only, while the macOS credential store is reached through the Keychain service (`security find-generic-password -w`). The legacy corpus (`src/clients/tools/corpus/commands.jsonl`) carries the class's blocked and allowed cases.

The judge is stateless: each evaluation sees only the command, its working directory, the compact repository digest, the model's optional reason, and any explicitly collected script observation. It has no conversation history, earlier command results, or tool outputs, so block and warn remediation recommends one self-contained command or guarded sequence whose safety the next request can evaluate alone. The judge cannot verify a reason's claims, and a reason never authorizes a remote destructive command: it may state intent as context, but a merge or branch delete is safe only with an in-command guard; a claim that a pull request is merged or authorized never makes `git push origin --delete <branch>` or `gh pr merge` safe. The judge also evaluates a guard as text: it cannot verify the remote state a guard reads or that the execution environment is unmodified, so a remote-effect guard is workflow guidance with a documented limitation rather than a hard guarantee.

Bash preflight additionally observes one directly referenced script for literal `bash`, `sh`, `zsh`, `dash`, `ksh`, `ksh93`, `ash`, `mksh`, and `pdksh` invocations (including `/bin/` and `/usr/bin/` spellings, quoted paths, and optional `--`). It resolves the path relative to the tool cwd using the in-process Read grants, then reads at most 32 KiB from a regular UTF-8 file. Missing, denied, oversized, binary, and unreadable recognized files or non-UTF-8 canonical paths fail closed before judgment; exact-command allowlisting cannot override these collection errors. Emergency bypass skips collection. Script text is untrusted evidence, never user authorization, and is sent to the configured judge provider. Successful tool results identify the observed path without echoing contents.

Collection is deliberately incomplete: interpreter flags (including `+` options), expansions, compound commands, other interpreters, wrappers, and recursive dependencies are not inspected. Interpreter identity, startup files, and environment-driven behavior are not verified. Requests explicitly state whether one file or no files were observed; neither state proves all dependencies safe. `cake bash check` and corpus evaluations without a tool context do not collect host files and explicitly report no script evidence in the judge request. Their script verdicts can therefore differ from Bash preflight. Observation is not an execution snapshot: directory-handle traversal refuses symlink replacement during opening, but renames of already-open directories and changes after reading remain possible. The OS sandbox still confines executed commands. See [ADR-035](adr/035-judge-script-observations.md).

Sandbox availability errors fail closed, including on platforms without a supported OS sandbox. Under the default `workspace-write` or explicit `read-only` policy, Cake reports the unavailable sandbox before Bash spawns; users must explicitly select `danger-full-access` (or set `CAKE_SANDBOX=off`) to run without an OS filesystem sandbox. On macOS, a process already inside Seatbelt may receive the recognized nested-profile `sandbox_apply: Operation not permitted` failure. In that one case Cake warns and relies on the inherited parent sandbox. The parent policy may be more or less restrictive than Cake's selected policy.

For operational diagnosis and platform-specific recovery, follow the [Debugging Sandbox Denials runbook](runbooks/debugging-sandbox.md).

## What the sandbox does not restrict

The Bash sandbox is a filesystem boundary. Network access remains available, and environment variables are not path-bounded, so a command can still reach a credential the sandbox cannot deny; the judge's `credential-disclosure` class is the only gate over that disclosure, and it is stochastic. Remote Git history, APIs, databases, and other network effects are outside the filesystem sandbox.

The sandbox also cannot promise confidentiality from a model or provider once content is included in a prompt, tool result, hook context, or API request.

## Trusted extensions

Hooks are user or project control-plane commands. Toolbox tools are user-provided executables exposed to the model. Both run with the Cake process's ambient host authority, outside the Bash sandbox.

`PreToolUse` hooks are a primary enforcement path for in-process mutations (Edit and Write) and for toolbox calls, which run unsandboxed. Hooks evaluate the same structured `tool_input` the tool will act on, so a payload whose raw JSON is malformed but repairable cannot bypass a hook that inspects the tool's structured arguments.

Only enable hook files and toolbox directories you trust. The project-local `.cake/tools` directory is discovered automatically, so a cloned repository can contribute executables when run with a writable sandbox; inspect or remove those files before running untrusted code. Read-only mode skips toolbox discovery; hooks remain trusted control-plane behavior.

## Sessions, logs, and telemetry

Session transcripts can contain prompts, model responses, tool calls, tool outputs, hook records, paths, and other sensitive project information. Protect `~/.local/share/cake/sessions/` or the configured data directory accordingly.

A command that starts under the filesystem sandbox but receives a sandbox denial keeps its ordinary Bash tool result and appends a `[Sandbox restriction]` marker. The task completion record also includes a `sandbox` permission-denial label with the inferred `read`, `write`, or `execute` operation and attempted path. This label is audit metadata; the operating-system sandbox remains the enforcement boundary, and the operation inference is intentionally best effort.

Telemetry sidecars intentionally omit prompt text, assistant text, raw tool output bodies, judge command/reason/cwd values, provider response bodies, credentials, and authorization headers, but still contain operational metadata. Judge-attempt records include model controls, prompt byte counts, phase timing, attempt/retry ordinal, retry reason, backoff wait, effective deadline, status, request identity digests (never raw provider-controlled identifiers), termination, and token usage when available. Judge compensation events carry the same one-way digest of the originating tool-call identifier, so a verdict can be paired with its transcript call without persisting the raw identifier.

`cake bash check --diagnostic` is a separate, explicit raw inspection surface. Its stdout contains the effective judge prompts and transformed request JSON, so it exposes the inspected command, working directory, compact repository state, optional model-supplied reason, and any secrets already embedded in those values. It also renders parsed response metadata and the bounded `TypeSafe` observation (outcome, probability or failure class, and elapsed time), but not the raw `TypeSafe` request or response: the request carries the same command, working directory, repository state, reason, and rubric the judge prompts already expose, and the response is provider-supplied text. Treat that output like a session transcript and avoid redirecting or sharing it unless the destination is trusted. Cake omits its resolved API key, authorization headers, configured provider headers, and unrelated environment variables; enabling the flag does not make normal Bash preflight telemetry raw.

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

TypeSafe evaluation is an explicit opt-in data transmission. When enabled (`mode = "shadow"` or `mode = "cascade"`), Cake sends the command and bounded judge context to `api.typesafe.ai`: the command, working directory, repository digest, untrusted reason, and effective rubric, in the request text the ADR 034 evaluation measured. In shadow mode the answer has no execution authority and is metadata only. In cascade mode the answer can authorize: a clean observation at or above the compiled `0.85` cutoff approves a command without a generative-judge call ([ADR 034](adr/034-typesafe-judge-cascade-cutoff.md)). That authorizes a command; it does not widen the boundary. The existing judge, allowlist, and emergency bypass keep their command-policy behavior, a fast approval still runs under the OS sandbox, and every failure class --- a timeout, a transport or protocol failure, a missing credential, or a probability below the cutoff --- falls back to the judge, so a TypeSafe failure can neither
approve nor block. Hooks run independently and retain their own warning or blocking behavior. Local telemetry stores only elapsed time, bounded model and usage metadata, the typed probability, a failure class, and an optional one-way tool-call digest.

The three `[tools.bash.judge.typesafe]` keys merge from global, project, and profile settings, so a repository's `.cake/settings.toml` can arm the cascade, or point the observation at a different model, for commands run in it under the user's ambient `TYPESAFE_AI_API_KEY`. That is the authority the rest of `[tools.bash.judge]` already grants a project, where `enabled = false` removes the judge entirely, and the cutoff's `0.85` evidence is specific to the pinned `jev-1.13.0` model: a different model sends the same request text to a classifier with no measured bound. Treat project settings as trusted input, as the sections above already require.

## Related decisions

- [ADR 014](adr/014-sandbox-policy-cli-flag.md), the sandbox policy flag.
- [ADR 016](adr/016-nested-seatbelt-sandbox-fallback.md), the recognized nested-Seatbelt fallback.
- [ADR 017](adr/017-trusted-executable-toolbox-tools.md), trusted toolbox executables.
- [ADR 018](adr/018-llm-judge-command-gate.md), the LLM judge command gate.
- [ADR 019](adr/019-project-customizable-sandbox-paths.md), project-customizable sandbox paths.

# Security and trust boundaries

This document defines Cake's security intent and limitations. It is the authority for permissions and trust; implementation details belong in the sandbox and tool code.

## Threat model

Cake treats model-generated tool calls and shell commands as untrusted. It limits filesystem effects to paths allowed by the selected policy and sends every Bash command to the configured external policy gate before execution.

Cake does not attempt to make an untrusted model safe to run with arbitrary credentials, network access, trusted hooks, or toolbox executables. The user remains responsible for the repository, configuration, secrets available to child processes, and authority granted through sandbox policy.

## Sandbox policies

`--sandbox` selects:

- `read-only`: the workspace, configured directories, and built-in toolchain paths are read-only. Cake-managed temporary paths remain writable so commands can produce intermediate output. Mutating built-in tools and toolbox tools are not offered to the model.
- `workspace-write`: the default. The working directory, linked-worktree Git directories, Cake-managed temporary paths, configured writable directories, and built-in toolchain and integration paths may be modified. Built-in grants cover common package managers, runtime managers, language caches, and CLI state such as `~/.cargo`, `~/.npm`, and `~/.config/gh`; the supplementary `~/Library/Keychains` file grant remains read-only.
- `workspace-write-interactive`: `workspace-write` plus the two macOS capabilities that hand work to another application --- `lsopen` (LaunchServices) and `appleevent-send` (Apple Events) --- so `open` and `osascript` can launch and drive other applications. Those primitives are capabilities rather than filesystem path grants, and they are the only difference: the path grants, tool set, and gate behavior are identical to `workspace-write`. An application launched this way runs outside Cake's sandbox, so selecting the policy deliberately widens the boundary beyond the filesystem for that run. On Linux, Landlock has no equivalent permission and does not restrict launching another process, so the policy behaves as `workspace-write`.
- `danger-full-access`: Cake does not apply its filesystem sandbox to Bash.

An explicit CLI policy takes precedence over `CAKE_SANDBOX`, which recognizes only the off aliases (`off`, `0`, `false`, `no`) and never selects a policy name, so `read-only` and `workspace-write-interactive` are available only through `--sandbox`. For compatibility, `CAKE_SANDBOX=off` selects danger-full-access when no flag is present.

The Bash tool's optional per-call `cwd` selects an existing directory that the sandbox already grants: the invocation working directory, a read-write grant (a `[sandbox]` or `directories` entry, a Cake-managed temporary path, or a built-in toolchain cache), or a read-only grant (`--add-dir`, a `[sandbox].read_only` entry, a skill directory, or Cake's own state directories). Relative paths resolve from the invocation working directory, and canonicalization rejects a symlink whose target is outside every grant. The built-in system read-and-execute paths (`/etc`, `/usr`) stay readable by absolute path but are not selectable, and `danger-full-access` admits any existing directory because it applies no profile. Selecting a directory adds no grant, changes no profile, and persists to no later call: the sandbox still governs every access from wherever the command starts, so a `read-only` run denies a write inside a directory it selected.

`--add-dir` adds a read-only path for one invocation. `directories` in settings adds persistent read-write paths. The `[sandbox]` section in settings adds persistent grants in two classes: `read_only` (read + execute, for files or directories) and `writable` (read + write + execute). Treat all of these as grants of authority.

`tools.enabled` is a separate narrowing allowlist for the model-visible and executable tool registry. Selecting Bash includes its BashSession controller so yielded commands remain manageable. It cannot add authority beyond the selected sandbox policy: that policy still removes tools that are unsafe under it, and an absent key preserves the default tool set.

The `[sandbox]` and `directories` path lists feed both the in-process Read/Edit/Write/Grep validation and the OS sandbox, so the two enforcement layers cannot diverge. A `read_only` entry naming a single executable grants exactly that file (plus read access to its ancestor directories), so sibling files in the same directory remain denied.

The OS sandbox represents ordinary filesystem grants with two effective path classes: read + write + execute, and read + execute without write. Built-in system and configuration paths, user read-only grants, skill paths, Cake's own state roots (the data/cache and sessions directories, granted read-only so session analysis and debugging skills can read their inputs), paths demoted by the `read-only` policy, and the supplementary `~/Library/Keychains` file grant share the second class. Platform-only capabilities such as macOS device, SSH agent, Keychain service access, Mach, process, and network rules stay separate because they are not ordinary filesystem path grants; Keychain service access remains mediated by Mach, and the app-launching pair (`lsopen` and `appleevent-send`) is emitted only under `workspace-write-interactive`.

macOS SSH agent access is a socket grant, not a directory grant. The profile grants read and write to the socket file `SSH_AUTH_SOCK` names and read-only access to its parent directory for listing and traversal, so an agent socket stored in `~/.ssh` leaves `config`, `known_hosts`, and private keys unwritable. The `/tmp/ssh-*` and `/private/tmp/ssh-*` agent-socket patterns are end-anchored to an `agent.<pid>` file, so a second agent at one of those standard paths is also allowed; a socket outside the conventional temporary layouts stays denied unless it is the session's `SSH_AUTH_SOCK`. A relative, missing, or non-socket `SSH_AUTH_SOCK` value emits no literal grant, and a socket that does not exist when the profile is generated receives no literal grant.

Project-level `.cake/settings.toml` is fully trusted by design, the same trust model as the rest of project `.cake/` configuration. There is no deny-list and no trust prompt: any path a project declares in `[sandbox]` becomes accessible to model-generated commands. Treat a cloned repository's `.cake/settings.toml` the way you treat its hooks.

During writable agent-run initialization, trusted `directories` and `[sandbox].writable` settings also authorize Cake to create absent directory targets and their missing parents before the tool sandbox is applied. Only the configured target is granted writable access; parents and siblings gain no grant. Read-only runs and settings inspection never provision configured targets. Creation is logged, errors stop initialization, and partial parent creation may remain after a failure. A typo can therefore create a stray directory under Cake's ambient authority. On Linux, read-only grants restrict content writes but do not restrict timestamp updates to existing files, a [Landlock limitation](https://docs.kernel.org/userspace-api/landlock.html). See [ADR 040](adr/040-bootstrap-writable-sandbox-grants.md).

`cake init` stays inside this trust model: its `.cake/settings.toml` is commented and behavior-preserving, with no sandbox grants, gate selection, or model selection; its `.cake/hooks.json.example` is inert because Cake never loads `.example` files.

## Enforcement layers

Read, ReadImage, Edit, and Write validate target paths in-process. Bash runs every command through the configured policy gate before spawn, then runs under an OS filesystem sandbox:

- macOS uses Seatbelt through `sandbox-exec`;
- Linux uses Landlock and requires a kernel capable of fully enforcing the configured ruleset.

The configured external command-policy gate is the command-safety gate above the OS sandbox. Cake contains no built-in judge, rubric, provider client, or TypeSafe client: an external executable owns policy, and Cake assembles the request and enforces the decision. A `block` or a fail-closed gate failure prevents the command from running; a `warn` runs it with the gate's guidance prepended. Per-check failures follow the configured `fail` mode, and `describe`/`init` startup failures always remove Bash for the run. Because a gate may call a model, its decisions are stochastic and must be measured rather than trusted; the generic `gate_startup`, `gate_decision`, and `gate_evidence` records in the session telemetry sidecar carry the gate identity, decision, failure class, latency, and one-way call digest. The gate owns its regression corpus and quality evidence outside Cake.

Before spawning Bash, Cake removes `config::git::AMBIENT_ENV_VARS` from the child environment after sandbox application. This keeps Git commands that inherit Cake's environment anchored to the Bash working directory, including when Cake itself was launched from a Git hook or another Git operation. The removal is unconditional across sandbox policies; a command that explicitly sets its own environment remains responsible for that choice.

The Bash preflight approves the command once, before spawn. A command that outlives its `timeout` yield window keeps running in an in-memory session under the same OS sandbox until it exits, is killed, reaches the hard wall clock, or Cake exits. `BashSession` only reads, lists, or terminates an already approved process and does not run the gate again. Cake retains the sandbox guard and process-group kill guard for the session's lifetime; normal exit, Ctrl-C, and SIGTERM tear down live groups. When the shell exits, Cake stops remaining members of its process group, including background children that redirected the captured pipes. A resumed transcript's old session IDs are invalid. Both Bash and BashSession are unavailable under the `read-only` policy because a yielded command can continue changing state after the original tool call. Removing a simple trailing `&` before preflight keeps the evaluated and executed text identical and prevents that shell operator from immediately orphaning
a child; process-group cleanup covers descendants independently of shell spelling.

The `sandboxed_bash_session_yields_and_completes_under_platform_policy` test exercises yield, poll, kill, and redirected background-child cleanup through the platform sandbox. It passed under macOS Seatbelt during issue #639 implementation; Linux Landlock runs the same test in Linux CI.

The macOS SSH agent profile tests need no live agent: they assert that the named socket is the only writable path in its directory, that the parent directory stays read-only, that sibling files and prefix-sharing temporary paths do not match, and that the read-only `~/.ssh` configuration grants survive.

Only Cake applies an exact-command allowlist override: the gate still checks the command, and a `block` decision is overridden but recorded with an `overridden` flag. `CAKE_GATE=off`, `--no-gate`, and legacy `CAKE_JUDGE=off` are explicit recorded bypasses; `[tools.bash.judge] enabled = false` remains a migration bypass when no explicit `gates` list is selected. The OS sandbox remains the filesystem boundary. Effects the sandbox cannot bound --- in-project destruction, remote Git effects, and credential or data egress --- are the residual risk surface covered only by the external gate's policy, if it has one, and that policy is stochastic when it uses a model.

The gate is workflow protection above the OS sandbox, not a shell parser or a security boundary by itself: it cannot guarantee that every equivalent spelling is recognized, and it covers effects the sandbox cannot, such as remote Git pushes. A gate that cannot evaluate must fail closed; abstention is a deliberate no-opinion decision, not a failure.

Cake sends a gate only the current request: the command, its working directory, the compact repository digest, the model's optional untrusted `reason`, collected script evidence, and the declared model descriptors. It sends no conversation history, earlier command results, or tool outputs, so a gate cannot verify a reason's claims and a reason never authorizes a remote destructive command. A merge or branch delete is safe only with an in-command guard; a claimed prior authorization does not make `git push origin --delete <branch>` or `gh pr merge` safe. A guard is evaluated as text: neither Cake nor the gate can verify the remote state a guard reads or that the execution environment is unmodified, so a remote-effect guard is workflow guidance with a documented limitation rather than a hard guarantee.

Bash preflight observes one directly referenced script for literal `bash`, `sh`, `zsh`, `dash`, `ksh`, `ksh93`, `ash`, `mksh`, and `pdksh` invocations (including `/bin/` and `/usr/bin/` spellings, quoted paths, and optional `--`). It resolves the path relative to the tool cwd using the in-process Read grants, then reads at most 32 KiB from a regular UTF-8 file. Missing, denied, oversized, binary, and unreadable recognized files or non-UTF-8 canonical paths fail closed before a gate request is sent. Exact-command allowlisting cannot override these collection errors, and emergency bypass skips collection. Script text is untrusted evidence, never user authorization, and is passed to the configured gate. The model-visible result identifies the collected path without echoing contents and does not claim the gate inspected them.

Collection is deliberately incomplete: interpreter flags (including `+` options), expansions, compound commands, other interpreters, wrappers, and recursive dependencies are not inspected. Interpreter identity, startup files, and environment-driven behavior are not verified. Requests state whether one file or no files were observed; neither state proves all dependencies safe. `cake bash check` does not collect host files and reports no script evidence. Gate verdicts can therefore differ between an inspection command and Bash preflight. Observation is not an execution snapshot: directory-handle traversal refuses symlink replacement during opening, but renames of already-open directories and changes after reading remain possible. The OS sandbox still confines executed commands. [ADR-035](adr/035-judge-script-observations.md) records the collection contract.

Sandbox availability errors fail closed, including on platforms without a supported OS sandbox. Under the default `workspace-write`, explicit `read-only`, or explicit `workspace-write-interactive` policy, Cake reports the unavailable sandbox before Bash spawns; users must explicitly select `danger-full-access` (or set `CAKE_SANDBOX=off`) to run without an OS filesystem sandbox. On macOS, a process already inside Seatbelt may receive the recognized nested-profile `sandbox_apply: Operation not permitted` failure. In that one case Cake warns and relies on the inherited parent sandbox. The parent policy may be more or less restrictive than Cake's selected policy.

For operational diagnosis and platform-specific recovery, use the `debugging-cake-sandbox` skill.

## External policy gates

[ADR 049](adr/049-external-policy-gates-with-the-cake-gate-1-protocol.md) defines the `cake-gate/1` boundary; [ADR 051](adr/051-external-judge-extraction-with-offline-acceptance.md) removed the parity, mandatory-shadow, and one-release-built-in prerequisites. The external gate is the only command-policy implementation in Cake.

A gate is a trusted executable, like a hook or a toolbox tool. Cake runs it outside the Bash sandbox with Cake's ambient host authority. Only enable gate executables and gate configuration you trust. A project `.cake/settings.toml` can name a different gate command, add a permissive gate, or remove one; the gate is only as trustworthy as the checkout and the user configuration, which is the trust this project already grants to hooks and toolbox executables. A gate must not read the workspace to assemble action evidence or resolve sandbox paths; Cake supplies the command, working directory, repository digest, and script evidence so sandbox semantics have one implementation. A gate may read its own rubric and configuration files named in its config.

No secret value crosses the gate boundary. Cake passes resolved model descriptors and the *names* of credential environment variables, never the values, and it never passes `Authorization` headers, credential file contents, or a file-backed subscription credential path. A descriptor is emitted only when its `api_key_env` is the effective credential; a file-backed run model yields no descriptor rather than a variable name that points at something else. It starts the gate with a minimal base environment plus the gate's declared `env` allowlist and the `api_key_env` names of its resolved models. The gate reads its own credentials from those variables. This is least disclosure, not a sandbox: a trusted gate can read anything Cake can, so the environment allowlist exists to prevent accidental leakage, not to contain a malicious executable. Gate configuration is not a credential store; credentials belong in environment variables.

The no-gate rule is part of the security contract: if no `pre_tool` `bash.exec` gate is available in enforce mode, Bash and `BashSession` are removed from the tool registry and a startup diagnostic explains how to install or configure a gate (`cake gates doctor`). Cake never falls back to running commands without a gate and contains no in-process fallback. An explicitly configured gate that cannot load is a settings error unless the invocation is explicitly bypassed; the bypass skips only post-parse structural validation, so TOML and unknown-key errors still fail. A selected gate that fails `describe` or `init` leaves no usable enforce gate and removes Bash for the run, and a broken gate is never silently dropped to leave a weaker pipeline. The full condition-to-outcome mapping is in [Configuration](configuration.md#gates) (the "Gate failure outcomes" table). `CAKE_GATE=off` and `--no-gate` are explicit, recorded bypasses; `CAKE_JUDGE=off` remains an alias for one release. With no
explicit `gates` list, Cake selects `cake-safety-judge` as the implicit `safety` gate; the executable resolves through `PATH`, and a missing executable is a startup failure, not a quiet allow. Treat `PATH` as trusted configuration.

Enforcement remains fail-closed. A per-check gate failure in enforce mode blocks the command unless the gate sets `fail = "open"`; a `describe` or `init` startup failure is always pipeline-fatal. Per-gate `fail = "open"` is an explicit experiment setting, is recorded as failed-open, and is surfaced by `cake gates doctor`; it is never the default. The circuit breaker honors the gate's `fail` mode on later checks. A shadow gate has no authority: its decision, annotations, allowlist, and failures are recorded and never change authorization. Only Cake applies an allowlist override or a bypass. A gate approval is not a sandbox bypass: the approved command still runs under the OS sandbox, and a gate can only annotate a command, never change its text, working directory, or arguments.

Cake treats every gate frame as untrusted input even though the executable is trusted. It validates the protocol and frame shape, bounds frames, annotations, messages, warnings, and evidence, strips terminal control characters from model-visible text, kills and restarts a timed-out or crashed process, and disables a gate after two consecutive process-level failures so a broken gate cannot add a startup timeout to every command; later checks follow the gate's `fail` mode. Gate-supplied text is never executed.

Bypass and failure classes this boundary must defend against, for security review of any change:

- no gate configured, found, or usable: Bash is disabled, never ungated; there is no in-process fallback;
- an enforce gate that fails to start: a `gate_startup` failure is recorded, Bash is removed for the run, and the gate is never silently dropped;
- an enforce chain in which every gate abstains: accepted as a configured no-opinion pipeline, so the command runs with `data.gate: null`; a gate that cannot evaluate must send an error frame instead of abstaining;
- a project settings file replacing the safety gate with a permissive command: accepted as trusted project configuration, and documented as such;
- a secret value crossing the JSON boundary or the child environment: descriptors carry names only, and the environment is a fixed base plus the explicit allowlist;
- a gate forging or rewriting a decision to widen authorization: Cake owns composition, allowlist application, bypass, and fail-closed handling; a gate cannot widen another gate's decision;
- gate text injecting terminal control sequences or oversized output into the model-visible result: text is sanitized and bounded;
- a gate process timing out, crashing, or looping on restart: the process group is killed, the check fails unless the gate is `fail = "open"`, and repeated process failures disable the gate for the run;
- a gate approval skipping the sandbox, the exact-command allowlist rule, or the bypass record: no path exists; a gate approval is only a policy decision;
- a shadow gate acquiring authority: shadow decisions are recorded only.

## What the sandbox does not restrict

The Bash sandbox is a filesystem boundary. Network access remains available, and environment variables are not path-bounded, so a command can still reach a credential the sandbox cannot deny. Remote Git history, APIs, databases, and other network effects are outside the filesystem sandbox. The configured external gate may implement credential-disclosure, data-egress, or destructive-effect policy, but Cake does not provide one, and a model-backed gate's decision is stochastic.

The sandbox also cannot promise confidentiality from a model or provider once content is included in a prompt, tool result, hook context, or API request.

## Trusted extensions

Hooks are user or project control-plane commands. Toolbox tools are user-provided executables exposed to the model. Gate executables enforce command policy. All three run with the Cake process's ambient host authority, outside the Bash sandbox. Only enable hooks, toolbox directories, and gate executables you trust.

`PreToolUse` hooks are a primary enforcement path for in-process mutations (Edit and Write) and for toolbox calls, which run unsandboxed. Hooks evaluate the same structured `tool_input` the tool will act on, so a payload whose raw JSON is malformed but repairable cannot bypass a hook that inspects the tool's structured arguments.

Only enable hook files and toolbox directories you trust. The project-local `.cake/tools` directory is discovered automatically, so a cloned repository can contribute executables when run with a writable sandbox; inspect or remove those files before running untrusted code. Read-only mode skips toolbox discovery; hooks remain trusted control-plane behavior.

## Sessions, logs, and telemetry

Session transcripts can contain prompts, model responses, tool calls, tool outputs, hook records, paths, and other sensitive project information. Protect `~/.local/share/cake/sessions/` or the configured data directory accordingly.

A command that starts under the filesystem sandbox but receives a sandbox denial keeps its ordinary Bash tool result and appends a `[Sandbox restriction]` marker. The task completion record also includes a `sandbox` permission-denial label with the inferred `read`, `write`, or `execute` operation and attempted path. This label is audit metadata; the operating-system sandbox remains the enforcement boundary, and the operation inference is intentionally best effort.

Telemetry sidecars intentionally omit prompt text, assistant text, raw tool output bodies, command/reason/cwd values, provider response bodies, credentials, authorization headers, and absolute working directories, but still contain operational metadata. The omission is enforced at the serialization boundary: every provider-controlled identifier a record retains (a tool-call id, a provider request id) is stored only as a one-way SHA-256 digest, an absent or empty identifier writes no value, every provider-supplied text field (`api_attempt.error`, the `responses_failed` strings, retry and compensation details, and the session summary error) is bounded to 1,024 bytes with a truncation marker, and the configured endpoint is reduced to its parsed, credential-free form. One carve-out is deliberate: the termination record's provider-reported `provider_status` and `provider_reason` are retained as sent and are not bounded here. Generic gate records carry gate name, version, mode, decision,
code, latency, override, bypass, disabled, and failure-class metadata plus the one-way call digest. `gate_evidence` is the only path for gate-controlled text, and Cake bounds every payload. Historical `judge_attempt` and `type_safe_shadow` records remain readable in old sidecars but no current path writes them. Current gate decisions also emit the compatibility `judge_verdict`, `judge_fail_closed`, and `judge_bypass` compensation events so existing denial attribution continues to work.

`cake bash check --diagnostic` is no longer available. The removed flag produced raw prompts and provider responses; use the standalone gate's own diagnostic command (for example `cake-safety-judge check --diagnostic`) when you need that inspection surface, and treat its output like a session transcript. `cake bash check --json` remains a metadata-only gate-decision document.

## Security-sensitive changes

Changes to allowed paths, sandbox policy, fallback behavior, command checks, gate executables or gate configuration, hook/toolbox execution, credential handling, transcript contents, or network policy require:

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

TypeSafe is no longer a Cake evaluation stage. A legacy `[tools.bash.judge.typesafe]` table is translated into the implicit `safety` gate's `config`, and `TYPESAFE_AI_API_KEY` is added to that gate's environment when the legacy table is present. Cake sends no request to TypeSafe and stores no TypeSafe probability, usage, or failure metadata. The external gate owns that disclosure surface and any shadow/cascade behavior; review the gate's own documentation before enabling it. A project `.cake/settings.toml` can still select a gate command and pass it configuration, so treat project settings as trusted input.

## Related decisions

- [ADR 014](adr/014-sandbox-policy-cli-flag.md), the sandbox policy flag.
- [ADR 016](adr/016-nested-seatbelt-sandbox-fallback.md), the recognized nested-Seatbelt fallback.
- [ADR 017](adr/017-trusted-executable-toolbox-tools.md), trusted toolbox executables.
- [ADR 018](adr/018-llm-judge-command-gate.md), the historical LLM judge command gate.
- [ADR 019](adr/019-project-customizable-sandbox-paths.md), project-customizable sandbox paths.
- [ADR 049](adr/049-external-policy-gates-with-the-cake-gate-1-protocol.md), external policy gates and the `cake-gate/1` protocol.
- [ADR 051](adr/051-external-judge-extraction-with-offline-acceptance.md), extraction without parity or mandatory-shadow prerequisites.

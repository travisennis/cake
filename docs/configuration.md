# Configuration

This is the user authority for configuring Cake. `cake --help` remains the authority for CLI flags and defaults.

## Locations and precedence

Let `<config>` mean the non-empty value of `XDG_CONFIG_HOME`, or `~/.config` when that variable is unset or empty. Cake loads global configuration from `<config>/cake/` and project configuration from `.cake/` in the working directory.

For model and behavior settings, project values overlay global values. When a profile is selected, precedence is:

1. CLI flags
2. project profile
3. global profile
4. project top-level settings
5. global top-level settings

Keys in a settings file that Cake does not recognize are ignored and reported as a warning on stderr, so a misspelled key (for example `temparature` instead of `temperature`) is surfaced instead of silently dropped. The warning names the file and the section, for example `unknown key 'temparature' in [[models]] entry 'zen'; ignored`.

Model definitions with the same name are replaced as a unit by the project definition. Additional `directories` are merged. Profiles may select a model and overlay behavior but may not define model providers.

## Project scaffolding with `cake init`

`cake init` creates explicit, reviewable project scaffolding: `.cake/` and a commented, behavior-preserving `.cake/settings.toml` referencing the keys in this document without changing any behavior. `cake init --hooks` also creates an inert `.cake/hooks.json.example`. If a planned target already exists, the command reports the conflict and exits nonzero without writing anything; re-running after success is a safe refusal. Generated files are references only; the full contracts are here and in `docs/security.md`.

## Models

Settings files are `<config>/cake/settings.toml` and `.cake/settings.toml`:

```toml
default_model = "openrouter"
directories = ["../shared"]
system_prompt = "prompts/coding.md"

[[models]]
name = "openrouter"
model = "openai/gpt-5"
base_url = "https://openrouter.ai/api/v1/"
api_key_env = "OPENROUTER_API_KEY"
api_type = "chat_completions"
max_output_tokens = 8000
context_window = 200000
reasoning_effort = "high"

[profiles.review]
default_model = "openrouter"
directories = ["../standards"]

[profiles.review.skills]
only = ["review"]
```

Every model requires:

- `name`: lowercase letters, numbers, and hyphens;
- `model`: provider model identifier;
- `base_url`: OpenAI-compatible endpoint;
- `api_key_env`: environment variable containing the credential.

### Codex ChatGPT subscription prototype

For a fast local prototype, Cake can reuse a file-backed login created by the Codex CLI:

```toml
default_model = "chatgpt"

[[models]]
name = "chatgpt"
model = "gpt-5"
base_url = "https://chatgpt.com/backend-api/codex"
api_key_env = "UNUSED_FOR_CHATGPT_AUTH"
api_type = "responses"
```

Run `codex login` first. For this exact `base_url`, Cake reads the access token and ChatGPT account ID from `CODEX_HOME/auth.json`, or `~/.codex/auth.json` when `CODEX_HOME` is unset. `api_key_env` is still required by the settings schema, but is ignored for this backend. The prototype does not implement its own browser login, token refresh, logout, or keyring-backed Codex auth; if the file-backed login expires, log in again with Codex.

This uses an internal Codex backend and is intended for experimentation rather than a stable provider integration. The backend may require a currently supported Codex model and Responses API request shape.

Optional model fields are `api_type` (`chat_completions` or `responses`), `provider`, `provider_headers`, `temperature`, `top_p`, `top_k`, `max_output_tokens`, `context_window`, `reasoning_effort`, `reasoning_summary`, `reasoning_max_tokens`, `providers`, and `supports_images`. `top_k` is an integer top-k sampling cutoff, omitted when unset.

### Provider-specific behavior

`provider` selects provider-specific request behavior: `"openrouter"` sends configurable `provider_headers` attribution (`http_referer` as `HTTP-Referer`, `x_title` as `X-Title`), while `"opencode"` targets OpenCode Zen and sends `x-opencode-session` with the run's session UUID on every provider POST (both Chat Completions and Responses APIs). When `provider` is omitted, Cake infers OpenRouter from an `openrouter.ai` base URL and OpenCode from an `opencode.ai` base URL (for example `https://opencode.ai/zen`). The session ID is not secret and is never redacted.

`context_window` is the model's input-token budget in tokens. When set, Cake logs the remaining budget each turn: window minus the last request's input tokens (the full request: system prompt, tools, history). The next request adds output and client-added tool outputs, which Cake does not tokenize; reserve a buffer. A known window also classifies each response: complete reported input above it is context pressure, so the response is discarded instead of persisted or continued, whatever ended the turn. A length stop at or above the intended `max_output_tokens`, a refusal, a content filter, a provider-reported failure, and missing or partial usage are never context pressure. Absent means the window is unknown and Cake keeps current behavior (recovering from provider context-limit errors by parsing their message text).

`supports_images` (default `false`) declares that the model accepts image input. It gates the `ReadImage` tool: a model without it never sees `ReadImage` in its prompt tool list or request tools array, so Cake cannot send pixels to a text-only model. Set `supports_images = true` on a vision-capable entry to enable the tool. Because the flag belongs to the `[[models]]` entry, a resumed or forked session uses the entry it was pinned to, and `--tools ReadImage` narrows the tool set but cannot re-add a tool the model's capability gate removed.

Set the selected model explicitly with `--model`, through a selected `--profile`, or with `default_model`. Reasoning and output-token CLI flags override the resolved model for one invocation.

All model references --- `default_model`, `--model`, profiles, and `[tools.bash.judge] model` --- use a `[[models]]` entry's `name` as the index into that entry's full configuration (provider, base URL, API key, temperature, reasoning, and other fields). The `model` field inside a `[[models]]` entry is the raw provider model identifier and is not used to reference a model elsewhere.

Resumed and forked sessions (`--resume`, `--fork`) are pinned to the `[[models]]` entry name they were created with, which is persisted in the session alongside the provider model ID. A bare resume reuses that entry deterministically. Passing `--model` for a different entry --- even one sharing the same provider model ID with a different backend or reasoning effort --- fails with a session-model mismatch instead of silently switching. Sessions written before the entry name was persisted carry only the provider ID: a bare resume still works when the ID matches exactly one entry, and otherwise fails listing the matching entry names so the run can be retried with an explicit `--model <name>`.

Relative `system_prompt`, `skills.path`, `directories`, and `[sandbox]` values resolve from the invocation working directory, including the created worktree when `--worktree` is active. Use absolute paths for global settings that must work from every project. Invalid files and unknown selected models fail before the provider request.

## Limits

The optional `[limits]` section bounds the agent loop and the tool output budgets. A limit is a positive integer, or the string `"unlimited"` to mean no cap:

```toml
[limits]
max_turns = "unlimited"  # explicit opt-out; overrides a global cap
```

`0`, negative values, and any other string are rejected at load time. Project `[limits]` values override global values per key, including back to `"unlimited"`. A selected profile may also define `[profiles.NAME.limits]`; profile values override the merged top-level values per key, with project profile values taking precedence over global profile values.

```toml
[profiles.review.limits]
max_turns = 10
read_max_output_bytes = "unlimited"
```

An absent key inherits the lower-precedence value. An explicit `"unlimited"` removes that value's cap.

### Agent loop limits

The agent-loop keys are off by default: an uncapped loop is deliberate, and no limit fires unless you configure one. Turns and tool calls are independent resource boundaries.

```toml
[limits]
max_turns = 10        # stop after 10 agent-loop turns
max_tool_calls = 50   # stop after 50 executed tool calls
```

- `max_turns`: maximum agent-loop turns. When the loop reaches the cap and would otherwise continue, Cake stops with a `limit_exceeded` outcome and surfaces the last assistant message, if any, as the partial result.
- `max_tool_calls`: maximum tool calls executed. A turn whose batch would exceed the cap is stopped before any call in the batch runs.

The limits combine; whichever fires first stops the loop. The stop is reported as `limit_exceeded` in the `task_complete` record and completion JSON, and as a distinct error in text mode.

### Tool output budgets

Tool limits have built-in defaults. Output budgets accept `"unlimited"`; Bash session safety limits require positive integers.

```toml
[limits]
bash_output_max_bytes = 50000   # Bash inline output cap (bytes; default 50000)
bash_read_cap = 100000          # Legacy key; parsed but no longer limits Bash sessions
bash_session_output_max_bytes = 1048576 # Unread output kept per session
bash_session_max = 16                    # Concurrent running commands
bash_session_max_seconds = 3600          # Hard wall clock per command
bash_session_exited_ttl_seconds = 600    # Retention after exit
read_default_end_line = 200     # Read default window (lines; default 200)
read_max_output_bytes = 100000  # Read output and Edit input cap (bytes; default 100000)
read_max_line_bytes = 10000     # Read per-line cap (bytes; default 10000)
read_image_max_bytes = 5242880  # ReadImage image cap (bytes; default 5 MiB)
hook_output_limit = 65536       # Hook stdout/stderr cap per hook (bytes; default 65536)
```

- `bash_output_max_bytes`: maximum bytes of Bash or BashSession output returned inline. Output exceeding the cap is written to a secure temp file; completed Bash results include a head+tail preview, while yielded Bash and BashSession results include a head preview. `"unlimited"` disables the spill.
- `bash_read_cap`: retained for settings compatibility. Session-managed Bash commands no longer use it or kill a command for producing too much output.
- `bash_session_output_max_bytes`: maximum unread bytes held for one Bash session. Overflow drops the oldest bytes at a UTF-8 boundary and reports the gap on the next read.
- `bash_session_max`: maximum concurrently running Bash sessions. Completed sessions are retained up to four times this count.
- `bash_session_max_seconds`: hard wall clock in seconds; reaching it terminates the process group.
- `bash_session_exited_ttl_seconds`: seconds a completed session remains available for late or repeated reads, subject to the completed-session count cap.
- `read_default_end_line`: default Read window in lines when the model omits `end_line`. `"unlimited"` reads to the end of the file.
- `read_max_output_bytes`: maximum bytes of Read output before truncation at a UTF-8 boundary and maximum bytes of an input file Edit will read. `"unlimited"` disables both caps.
- `read_max_line_bytes`: maximum bytes delivered for a single Read line before it is truncated with a marker at a UTF-8 boundary. The cap bounds memory for newline-free giant lines. `"unlimited"` re-enables reading a whole line into memory, which can starve memory for such a file.
- `read_image_max_bytes`: maximum bytes of an image the ReadImage tool will read and send to the provider. A larger file is rejected with a model-visible error, and the read itself stops at the cap. `"unlimited"` disables the cap.
- `hook_output_limit`: maximum bytes of hook stdout and stderr captured per hook invocation. `"unlimited"` disables truncation.

When a project overrides a global budget back to no cap, `"unlimited"` is the explicit value that does so.

## Tool selection

The optional `[tools]` section can limit the exact tools exposed to the model and accepted by the agent. When `enabled` is absent, Cake keeps the normal built-in tools and any discovered toolbox tools. An explicit empty list exposes no tools.

```toml
[tools]
enabled = ["Read", "Edit"]

[profiles.review.tools]
enabled = ["Read"]
```

Names are case-sensitive registered names: `Bash`, `BashSession`, `Read`, `Edit`, `Write`, or a toolbox name such as `tb__run_tests`. Selecting `Bash` also exposes `BashSession` so commands that yield can be read or killed. Selecting `BashSession` alone remains valid, though it cannot start a command. The list replaces lower-precedence global or project values, so a selected profile can narrow the top-level selection. A profile that omits `enabled` inherits the lower-precedence list; there is no `all` value that restores every tool. Unknown or unavailable names are warned about and are never registered. The selection is applied after sandbox filtering; for example, `--sandbox read-only` still removes `Bash`, `BashSession`, `Edit`, `Write`, and toolbox tools. `tools.enabled = []` also causes provider requests to omit tool definitions.

`--tools name1,name2` selects a one-run allowlist and `--no-tools` exposes no tools for the run, the CLI spelling of `enabled = []`. Both mirror `--skills`/`--no-skills`. Because CLI flags are the highest precedence, either flag replaces the profile and top-level `[tools].enabled` chain instead of unioning with it, and `--no-tools` wins when both flags are passed. Values split on `,`, trim surrounding whitespace, and drop empty entries, so `--tools ""` or `--tools ","` selects no tools, matching `--skills ""`. As with settings, unknown or unavailable names warn and are dropped without failing the run; `--tools` is a usability filter, not a security boundary, and the sandbox and configured policy gates remain the enforcement.

## Bash tool settings

`[tools.bash]` configures the Bash tool. Its `gates` list selects the ordered external policy pipeline that evaluates every non-empty command before spawn. Cake assembles the command, working directory, repository digest, optional model-supplied `reason`, and any collected script evidence, then enforces the gate's decision. If no usable enforce gate covers `pre_tool`/`bash.exec`, Cake removes Bash and BashSession rather than running commands ungated. The gate configuration is in [Gates](#gates); [Security](security.md) owns the trust boundary.

```toml
[tools.bash]
gates = ["safety"]
```

The `reason` argument is the model's untrusted self-report of intent. It is supplied to the gate and appears in the session transcript, never in telemetry. It never authorizes a remote destructive command on its own, so a merge or branch delete still needs an in-command guard.

For a direct literal `bash`, `sh`, `zsh`, `dash`, `ksh`, `ksh93`, `ash`, `mksh`, or `pdksh` script invocation (including `/bin/` and `/usr/bin/` spellings, quoted paths, and optional `--`), Cake reads one regular UTF-8 file of at most 32 KiB under the configured Read grants and sends its contents to the selected gate as untrusted script evidence. Missing, denied, oversized, binary, or unreadable recognized files fail closed before a gate request is sent. This fixed limit has no settings key. Cake cannot know whether a gate used the evidence, so the model-visible note says only that the script was collected. Other shell forms, interpreter flags, expansions, wrappers, and nested dependencies are not inspected. Emergency bypass skips collection.

### Legacy judge settings

`[tools.bash.judge]` is accepted for migration only. Cake no longer contains an in-process judge, provider client, rubric, or TypeSafe client. When no explicit `[tools.bash].gates` list is present, Cake selects an external gate named `safety` whose command is `cake-safety-judge` and translates any configured legacy table into that gate's `config`:

- `model` becomes `config.model` and the gate's model descriptor name. `default` means the run's resolved model.
- `timeout_secs` plus `retry_budget_secs` becomes the gate's per-check `timeout_secs`.
- `rubric_file` becomes `config.rubric_file`; the external gate reads it.
- `allowlist` becomes the gate's exact-command allowlist.
- `[tools.bash.judge.typesafe]` becomes `config.typesafe`, and `TYPESAFE_AI_API_KEY` is added to the gate environment. Cake itself makes no TypeSafe request.

`enabled = false` and `CAKE_JUDGE=off` remain explicit bypasses and never enable a gate. When an explicit `gates` list is present, the legacy table is ignored, including its `enabled` flag; use `CAKE_GATE=off` or `--no-gate` for a bypass. An invalid legacy model is ignored only when an explicit gate selection is present or the legacy judge is bypassed. A missing `cake-safety-judge` executable is a startup failure, not a quiet fallback: Bash and BashSession are removed for the run.

The external gate owns its rubric, prompt, provider calls, retries, verdict vocabulary, and TypeSafe behavior. See the [cake-safety-judge](https://github.com/travisennis/cake-safety-judge) project for that policy and its evaluation commands.

## Gates

Status: this section is the `[gates]` configuration contract from [ADR 049](adr/049-external-policy-gates-with-the-cake-gate-1-protocol.md), as revised by [ADR 051](adr/051-external-judge-extraction-with-offline-acceptance.md). The keys are active; Cake contains no in-process judge. An unrecognized or invalid gate key is an input error (exit `3`), never a silent fallback.

Gates are external policy executables composed in order for an action. The only action in this release is the Bash pre-tool check, `bash.exec`. The [Gate protocol](integrations.md#gate-protocol) is the wire contract; this section owns configuration, defaults, and precedence.

```toml
[tools.bash]
gates = ["safety"]

[gates.safety]
command = "cake-safety-judge"
transport = "stdio"
mode = "enforce"
timeout_secs = 45
fail = "closed"
allowlist = ["git status --short"]
models = ["judge"]
config = { model = "judge", rubric_file = ".cake/judge-rubric.md" }
```

`[tools.bash].gates` is the ordered composition for `bash.exec`. Each name indexes a `[gates.<name>]` table. The table key is the local instance name; it may differ from the executable's `describe.name`, so one binary can be configured twice with different `config` (for example, two rubrics). A listed name with no table, an unknown transport or mode, or a missing `command` is a settings error at load time. A command that cannot be resolved or started is a startup failure: Bash is removed for the run. A table that is not listed is ignored, and `cake gates list` shows the resolved selection so a forgotten list entry is visible. An explicit empty list (`gates = []`) means no gates: Bash is disabled under the no-gate rule. A profile overlay replaces the list; `env`, `models`, and `allowlist` overlays also replace rather than union. The legacy `[tools.bash.judge].allowlist` keeps its union-across-precedence behavior; only the new `[gates.<name>].allowlist` replaces.

The TypeSafe cascade remains inside `cake-safety-judge`. A later composition can put a fast-approval gate in front, for example `gates = ["typesafe", "safety"]` with `[gates.typesafe] mode = "enforce"` and `short_circuit_on_allow = true`; `cake-typesafe-gate` does not exist yet.

Each `[gates.<name>]` table supports:

- `command` (required): the executable to run. A value containing a path separator resolves from the invocation working directory; a bare name resolves on `PATH`. Cake runs it directly, never through a shell.
- `args` (optional, default `[]`): arguments inserted before the `describe` or `serve` subcommand. No shell expansion is performed.
- `transport` (default `"stdio"`): `stdio` for one long-lived process per run, or `oneshot` for one process per check. The gate's `describe` must list the selected transport.
- `mode` (default `"enforce"`): `enforce`, `shadow`, or `off`. A shadow gate records its would-be decision and never changes authorization. `off` removes the gate from the run without deleting its configuration. A shadow gate must declare the `shadow` capability.
- `timeout_secs` (default `30`, minimum `1`): the per-check deadline. A timeout kills the gate process group, restarts the process for the next check, and fails that check closed unless `fail = "open"`.
- `startup_timeout_secs` (default `10`, minimum `1`): the deadline for `describe` and for `init`/`ready`, applied separately.
- `fail` (default `"closed"`): applies to per-check failures. `closed` blocks the command; `open` records the failure as failed-open and continues. A `describe` or `init` startup failure is always pipeline-fatal and is not affected by `fail`. `open` is an explicit experiment setting and is called out by `cake gates doctor`.
- `short_circuit_on_allow` (default `false`): when true, an `allow` decision that also sets `short_circuit` stops the enforce chain. A `warn` or `abstain` never short-circuits.
- `allowlist` (default `[]`): exact `input.command` strings whose `block` decision from this gate is overridden to allow. The command is still checked by every gate, the override is recorded, and only a deliberate `block` is overridden; failures, protocol errors, and the no-gate rule are never overridden. A profile overlay replaces the list.
- `env` (default `[]`): additional environment variable names to pass to the gate process. The credential variable names of the gate's resolved models are passed automatically. Values are never written to the protocol or telemetry.
- `models` (default `[]`): `[[models]]` entry names whose resolved descriptors the gate receives. The reserved name `default` selects the run's resolved model; the auto-detected safety gate receives it automatically when it declares `resolved-models`. A gate that does not declare `resolved-models` must leave this empty; a non-empty list without that capability is a settings error. A gate model must be environment-backed: Cake emits a descriptor only when its `api_key_env` is the credential the run actually uses, and a file-backed subscription credential is never passed. A file-backed run model is not emitted as `default`; a gate that needs a model then replies with `config` at `init`, Bash is removed for the run, and `doctor` names the fix. An unknown model name, or an explicit entry that resolves to a file-backed credential, is a settings error.
- `config` (default `{}`): the gate's own JSON-compatible configuration table, passed in `init` and limited to 64 KiB. Keys are gate-defined; Cake does not interpret them. Do not put credentials here.

`builtin-safety` was the reserved name of the deleted in-process adapter. It is rejected at load time with a message naming an external gate such as `cake-safety-judge`; no table or fallback can restore it.

The default pipeline is chosen in this order. An explicit `[tools.bash].gates` list wins, including an explicitly empty list. Otherwise Cake selects an enforce gate named `safety` that runs `cake-safety-judge`; a configured `[tools.bash.judge]` table supplies that gate's translation, and no table leaves its defaults in place. `enabled = false` or `CAKE_JUDGE=off` is the explicit global bypass. If the executable is missing or fails `describe`/`init`, the no-gate rule applies: Bash and BashSession are removed for the run. Nothing falls back to an in-process judge. An explicit `gates` list ignores the legacy judge table, including its `enabled` flag; use `CAKE_GATE=off` or `--no-gate` for a bypass. If `[gates.safety]` exists but is not listed, the table is ignored; the implicit `safety` selection still resolves `cake-safety-judge` from `PATH`.

`CAKE_GATE=off` or `--no-gate` bypasses the whole pipeline for the invocation. Every Bash call while bypassed records one bypass `gate_decision` with `bypassed: true`, and no gate process starts. `CAKE_JUDGE=off` is an alias for `CAKE_GATE=off` for one release. `--no-gate` wins over the environment variable, and `CAKE_GATE` wins over the deprecated `CAKE_JUDGE`. Only the exact value `off` disables the pipeline; other values are ignored, matching the existing `CAKE_JUDGE` behavior. A bypass is unsafe and is never the default.

When both `[tools.bash].gates` and `[tools.bash.judge]` are present, `gates` wins and the legacy table is ignored, including its `enabled` flag; use `CAKE_GATE=off` or `--no-gate` for a bypass. With no explicit `gates` list, the translated legacy table configures the implicit `safety` gate: `enabled = false` or `CAKE_JUDGE=off` is the global bypass, `allowlist` becomes that gate's allowlist, `timeout_secs` and `retry_budget_secs` are summed into its per-check timeout, and `rubric_file`, `model`, and `[tools.bash.judge.typesafe]` become its `config` table. The legacy `[tools.bash.judge].allowlist` keeps its union-across-precedence behavior. The legacy table is not a source of external-gate configuration when `[gates.safety]` is explicitly selected: that table's `config` and `env` own the rubric, TypeSafe settings, and `TYPESAFE_AI_API_KEY`. A project settings file may set any of these, so treat project settings as trusted input.

Profile overlays follow the ordinary settings rules. `[profiles.<name>.tools.bash] gates` replaces the list; `[profiles.<name>.gates.<name>]` merges field by field, and its `config` table merges key by key rather than replacing the whole table. A profile may set `mode = "off"` to drop one gate without editing the global list; if that leaves no enforce gate for `bash.exec`, the no-gate rule applies.

Gate failure outcomes, in the order the pipeline encounters them:

  | Condition                                                                                                                                    | Outcome                                                                                                                                                                                                                                                                                                                                                                                           |
  | -------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | A listed gate has no `[gates.<name>]` table, an unknown transport or mode, a missing `command`, or an unresolvable command                   | Settings error at load (exit `3`); no command runs.                                                                                                                                                                                                                                                                                                                                               |
  | No enforce gate is selected after precedence resolution                                                                                      | No-gate rule: `Bash` and `BashSession` are removed from the tool registry; one startup diagnostic; the session continues.                                                                                                                                                                                                                                                                         |
  | An enforce gate fails `describe` or `init` (nonzero exit, timeout, invalid object, or protocol or capability mismatch), regardless of `fail` | The pipeline is unavailable: a `gate_startup` failure is recorded, `Bash` and `BashSession` are removed for the run, and the diagnostic names the gate and the fix.                                                                                                                                                                                                                               |
  | A shadow gate fails `describe` or `init`                                                                                                     | A `gate_startup` failure is recorded; the shadow gate is dropped for the run; authorization is unaffected.                                                                                                                                                                                                                                                                                        |
  | A check times out, crashes, or violates framing                                                                                              | That check fails closed unless `fail = "open"`; the process group is killed and the next check starts a fresh process. After two consecutive process-level failures the gate is disabled for the run: later checks follow the gate's `fail` mode (an open gate records `failed_open` and continues, a closed gate fails immediately with the recorded class), and a shadow gate is not run again. |
  | A failed check has `fail = "open"`                                                                                                           | Recorded as failed-open; the next gate runs, or the command runs when it was the last enforce gate.                                                                                                                                                                                                                                                                                               |
  | A gate is `mode = "off"`, or an explicit `gates = []` list is set                                                                            | The gate is absent; if no enforce gate remains for `bash.exec`, the no-gate rule applies. An explicit empty list has no fallback: the no-gate rule applies.                                                                                                                                                                                                                                       |
  | `CAKE_GATE=off`, `--no-gate`, or legacy `CAKE_JUDGE=off`                                                                                     | No gate process starts; every Bash call records a bypass and runs under the OS sandbox.                                                                                                                                                                                                                                                                                                           |

Inspect the resolved pipeline without running a command:

```bash
cake gates list
cake gates doctor
cake gates doctor -- 'git push --force'
```

`list` shows the configured and auto-detected gates, their resolved commands and paths, transports, modes, timeouts, and declared phase/action coverage; it is informational and exits `0` when the settings load, even when a gate is broken. `doctor` runs `describe` and `init` for every gate, reports missing commands, unlisted gate tables, protocol or capability mismatches, unresolved models, file-backed models with no environment-backed descriptor, missing environment variable names, and the no-gate condition; it exits `0` when the pipeline is healthy, `1` on a configuration or coverage problem, `2` on a gate process failure, and `3` on a settings load error. With `-- <command>`, `doctor` also runs one check through the full chain and prints each stage's decision; that run may call a gate's provider and spend tokens. `doctor` never executes the inspected command.

## Filesystem access

Top-level and profile `directories` grant persistent read-write access to the listed directories under `workspace-write`. Global and project entries are merged, deduplicated, and sorted, so the resolved list is stable across runs. The `read-only` policy demotes these paths to read-only access.

The `[sandbox]` section grants the Bash sandbox and the Read/Edit/Write/Grep path checks extra filesystem access on top of the built-in toolchain paths, `--add-dir`, and `directories`:

```toml
[sandbox]
read_only = ["~/.local/bin/claude"]   # read + execute
writable = ["~/.claude", "~/.cache/claude"]  # read + write + execute
```

`read_only` entries may be files or directories and grant read plus execute access (enough to run a single binary such as `~/.local/bin/claude` without opening its whole directory). `writable` entries grant read, write, and execute access to directories. Both keys accept absolute paths, relative paths, and `~` expansion, and merge as a union across global settings, project settings, and the selected profile; the merged list is deduplicated and sorted, so resolved grants are stable across runs.

During writable agent-run initialization (including `danger-full-access`), Cake creates missing `directories` and `[sandbox].writable` targets and any missing parents before tools start. Automatic creation produces a warning in the log file; failures stop startup with the setting, target, and filesystem error. Existing nondirectory writable targets are ignored with a log warning. Only the configured target receives a writable grant, not its parents or siblings. Settings inspection and help do not create grant directories. Under `--sandbox read-only`, existing writable targets are demoted to read-only and missing ones are ignored with a log warning, without creating them. Missing `read_only` entries are also ignored with a log warning.

On top of those grants, Cake always grants read-only access to its own state: the data/cache directory (daily logs and session telemetry) and the sessions directory (or their `CAKE_DATA_DIR` equivalents). Session analysis and debugging skills read these inputs, so they work without a manual grant. The config directory is deliberately not granted; it holds credentials and trusted hooks and tools.

`directories = ["~/shared"]` also expands `~` (historically the path was ignored).

`--add-dir <PATH>` grants additional read-only access for one invocation and may be repeated. `--sandbox` selects `read-only`, `workspace-write`, `workspace-write-interactive`, or `danger-full-access`. `workspace-write-interactive` is `workspace-write` plus the macOS `lsopen` and `appleevent-send` capabilities, so model-generated commands can launch and automate other applications; on Linux it behaves as `workspace-write`.

These are security decisions. See [Security](security.md) before expanding access.

## Skills

Skills are directories containing `SKILL.md` with YAML `name` and `description` frontmatter. Cake discovers project and user skills plus configured roots, lists their metadata to the model, and lets the model read the full instructions on demand.

Default roots are `<working-directory>/.agents/skills/` for project skills and `~/.agents/skills/` for user skills. Configured `skills.path` roots are scanned between them. When names collide, project skills win, then configured roots in path order, then user skills.

```toml
[skills]
disabled = false
only = ["debugging-cake", "review"]
path = "~/my-skills:/shared/team-skills"
```

`path` uses the platform path separator. An empty `only` list permits all discovered skills. `--no-skills` disables them for one run; `--skills name1,name2` selects a one-run allowlist.

A run receives the catalog only when it can read a `SKILL.md`: the `Read` tool when it is selected, otherwise a shell-capable tool such as `Bash`, whose instructions load a skill with `cat <location>`. A run with neither, such as `--no-tools`, receives no catalog. `SkillActivated` session records remain `Read`-only, so a skill loaded through `Bash` is not recorded.

Profiles may overlay `disabled`, `only`, and `path` under `[profiles.<name>.skills]`.

Inspect the selected catalog without calling a model:

```bash
cake debug skills
cake --profile review debug skills --skills debugging-cake,review
cake debug skills --no-skills
cake debug skills --catalog-budget 8000 --description-budget 400
```

The report counts the rendered XML, including escaping and locations, in UTF-8 bytes and Unicode characters. Its token estimate is characters divided by four, rounded up; it is not a provider token count. It lists descriptions largest first and warns above the advisory defaults of 8,000 catalog characters and 400 description characters. These are configurable review thresholds, not model limits or measurements of instruction quality. Warnings exit successfully and never truncate descriptions or remove skills. Select fewer skills with `skills.only`, a profile, or `--no-skills` when appropriate.

The report measures the selected catalog, not the complete prompt; an agent run includes the catalog only when the run can read a `SKILL.md` (see above). Discovery precedence and profile filtering match an agent run. Place `--skills` and `--no-skills` after `debug skills` for this report.

## Instructions and system prompts

Cake reads optional agent instructions from:

1. `~/.cake/AGENTS.md`
2. `<config>/AGENTS.md`
3. `./AGENTS.md`

Non-empty files are included as mutable developer context. They complement one another rather than replacing earlier files.

The system prompt uses the first readable source in this order:

1. `--system-prompt <PATH>`
2. `.cake/system.md`
3. `system_prompt` from resolved settings/profile
4. `<config>/cake/system.md`
5. the prompt embedded in the binary

An override replaces the built-in prompt; it is not appended. The selected system prompt is stored when a session is created and reused on resume. Mutable AGENTS.md, skill, and environment context is rebuilt for each invocation.

`--no-runtime-context` suppresses only the mutable process-environment Developer message that reports the working directory, date, platform, architecture, shell, and terminal. The system prompt, AGENTS.md context, skill catalog, tool list, output schema, and session persistence are unchanged. It is intended for tool-less or transcript-only subcalls, not as a sandbox or security control: commands still inherit their normal environment.

## Hooks

Hooks are trusted commands configured in:

1. `<config>/cake/hooks.json`
2. `.cake/hooks.json`
3. `.cake/hooks.local.json`

Files are appended in that order and must declare `"version": 1`. Missing files are ignored; malformed files stop startup. Project-local hooks should normally be committed only when every project user is expected to trust them.

```json
{
  "version": 1,
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash|Write",
        "hooks": [
          {
            "type": "command",
            "command": "./.cake/hooks/check-tool.sh",
            "timeout": 5,
            "fail_closed": true
          }
        ]
      }
    ]
  }
}
```

Supported events are `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop`, `ErrorOccurred`, and `SessionEnd`. Matchers are accepted only for `SessionStart`, `PreToolUse`, `PostToolUse`, and `PostToolUseFailure`; omit the field for the other events. For matcher-capable events, a missing matcher or `"*"` matches all supported sources, and `|` separates exact matches.

`SessionEnd` is the final best-effort lifecycle event for a root agent invocation. It runs at most once with `reason` set to `success`, `error`, or `interrupted`; a failing or non-JSON `SessionEnd` hook never changes the invocation outcome.

Hook commands run outside the model tool sandbox with the project root as their working directory. Their input and decision protocol is documented in [Integrations](integrations.md).

## Toolbox tools

Toolbox directories are scanned in this order: entries from `CAKE_TOOLBOX`, the repeated `--toolbox` arguments, and the active project's `.cake/tools` directory. When `CAKE_TOOLBOX` is unset, Cake scans `<config>/cake/tools` before the explicit and project-local directories; a non-empty value replaces that global default, while an explicitly empty value suppresses only the global/default scan. Earlier directories win name conflicts. Relative `CAKE_TOOLBOX` and `--toolbox` entries resolve from the invocation directory. With `--worktree`, project-local discovery uses the active worktree's `.cake/tools` directory.

Each executable implements a describe and execute protocol. Valid tools are registered for the model with a `tb__` prefix. Project-local `.cake/tools` executables are trusted project configuration and are discovered automatically; inspect or remove them before running a cloned repository with a writable sandbox. Broken tools are skipped with a warning rather than blocking startup.

Toolbox executables are trusted and unsandboxed. Under `read-only`, Cake skips toolbox discovery entirely so even the describe action cannot mutate the workspace. See [Integrations](integrations.md) for the protocol and [Security](security.md) for the trust boundary.

## Data and diagnostics

`CAKE_DATA_DIR` overrides Cake's cache and session data roots. Otherwise logs and telemetry use `~/.cache/cake/`, while resumable sessions use `~/.local/share/cake/sessions/`.

Set `RUST_LOG=cake=debug` or `RUST_LOG=cake=trace` for verbose file logging. Normal command output is unchanged.

## Related decisions

- [ADR 002](adr/002-agent-skills.md), the skills system.
- [ADR 003](adr/003-settings-profiles.md), settings profiles.
- [ADR 049](adr/049-external-policy-gates-with-the-cake-gate-1-protocol.md), external policy gates and the `cake-gate/1` protocol.

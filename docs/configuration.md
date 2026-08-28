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

Optional model fields are `api_type` (`chat_completions` or `responses`), `provider`, `provider_headers`, `temperature`, `top_p`, `max_output_tokens`, `context_window`, `reasoning_effort`, `reasoning_summary`, `reasoning_max_tokens`, and `providers`.

`context_window` is the model's input-token budget in tokens. When set, Cake logs the remaining budget each turn: window minus the last request's input tokens (the full request: system prompt, tools, history). The next request adds output and client-added tool outputs, which Cake does not tokenize; reserve a buffer. Absent means the window is unknown and Cake keeps current behavior (recovering from provider context-limit errors by parsing their message text).

Set the selected model explicitly with `--model`, through a selected `--profile`, or with `default_model`. Reasoning and output-token CLI flags override the resolved model for one invocation.

All model references --- `default_model`, `--model`, profiles, and `[tools.bash.judge] model` --- use a `[[models]]` entry's `name` as the index into that entry's full configuration (provider, base URL, API key, temperature, reasoning, and other fields). The `model` field inside a `[[models]]` entry is the raw provider model identifier and is not used to reference a model elsewhere.

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

The output-budget keys have built-in compiled defaults that match the hard-coded constants they replaced, so out-of-the-box behavior is unchanged. Overriding a key changes tool behavior without a release; `"unlimited"` disables the cap.

```toml
[limits]
bash_output_max_bytes = 50000   # Bash inline output cap (bytes; default 50000)
bash_read_cap = 100000          # Bash read cap before kill (bytes; default 100000)
read_default_end_line = 200     # Read default window (lines; default 200)
read_max_output_bytes = 100000  # Read output cap (bytes; default 100000)
hook_output_limit = 65536       # Hook stdout/stderr cap per hook (bytes; default 65536)
```

- `bash_output_max_bytes`: maximum bytes of Bash tool output returned inline. Output exceeding the cap is written to a secure temp file and the agent receives a summary with the path plus a head+tail preview. `"unlimited"` disables the spill.
- `bash_read_cap`: maximum bytes of Bash output read before the process is killed and the capture ends. The default is 2× the inline cap, so a spill has enough data for a useful preview. `"unlimited"` reads until the process exits.
- `read_default_end_line`: default Read window in lines when the model omits `end_line`. `"unlimited"` reads to the end of the file.
- `read_max_output_bytes`: maximum bytes of Read output before truncation at a UTF-8 boundary. `"unlimited"` disables truncation.
- `hook_output_limit`: maximum bytes of hook stdout and stderr captured per hook invocation. `"unlimited"` disables truncation.

When a project overrides a global budget back to no cap, `"unlimited"` is the explicit value that does so.

## Bash tool settings

`[tools.bash]` configures the Bash tool. The `[tools.bash.judge]` table holds settings for the LLM command-safety judge, which evaluates every Bash command before it runs and is the command-safety gate above the OS sandbox. The judge is default-on and fail-closed: an unavailable judge (unresolvable model, rubric read failure, timeout, transport error, or malformed verdict) blocks the command rather than running it ungated. A `block` verdict prevents the command from running; a `warn` verdict runs it with the judge's guidance prepended to the output. `enabled = false` or `CAKE_JUDGE=off` disables the judge entirely for every command.

```toml
[tools.bash.judge]
model = "zen"       # optional [[models]] name
timeout_secs = 30   # default 30; never below 1
retry_budget_secs = 15  # default 15; 0 disables recovery
rubric_file = ".cake/judge-rubric.md"  # optional user rubric guidance
enabled = true      # default true; false is the emergency bypass
allowlist = ["git status"]  # exact raw commands whose blocks are overridden
```

- `model`: optional `[[models]]` name for the judge's model, in the same vocabulary as `default_model` and `--model`. The name indexes a fully configured `[[models]]` entry, so the judge uses that entry's provider, base URL, API key, temperature, reasoning, and other fields. When unset, the judge uses the agent's configured model. An unknown name fails at load time, unless the judge is bypassed (`enabled = false` or `CAKE_JUDGE=off`), in which case the unused model config is inert. Every Bash command's text is transmitted to the judge's provider for evaluation; with the default it is the same provider the conversation already uses, and setting a different `model` sends command text to a provider the conversation never touches. The judge's request carries no conversation history, earlier command results, or tool outputs: each evaluation is stateless, seeing only the command, working directory, repository digest, and the model's optional `reason`.
- `timeout_secs`: bounded judge call timeout in seconds. Defaults to 30; values below 1 are raised to 1.
- `retry_budget_secs`: extra seconds one recovery attempt may consume beyond `timeout_secs`. Defaults to 15; `0` disables recovery entirely. A timeout, a retryable transport/HTTP failure, or an undecodable 2xx response body (for example, an empty or non-JSON body) triggers at most one recovery attempt within a total operation deadline of `timeout_secs + retry_budget_secs` (45 seconds with defaults), never two full timeout periods. The recovery re-sends the same request on a fresh connection, honoring `Retry-After` up to a 5-second cap. Valid verdicts, refusals, malformed verdicts, and semantic backend parse failures are never retried; exhausted recovery still blocks the command (fail-closed).
- `rubric_file`: optional path to a user rubric file whose text is appended to the embedded default rubric as additional judge guidance (extra always-block classes, relaxations phrased as guidance). Relative paths resolve from the invocation working directory. Relaxations are advisory to the judge, not hard overrides; the allowlist is the only hard override.
- `enabled`: emergency bypass. `false` disables the judge for every command, equivalent to the `CAKE_JUDGE=off` environment variable; the environment variable wins when both are set. Off by default means the judge is enabled, and there are no allowlist entries in shipped defaults.
- `allowlist`: list of exact raw-command strings whose `block` verdicts are overridden to allow. An allowlisted command is still judged, and the verdict plus an `overridden` flag are recorded; only a `block` is overridden, so the command still cannot hide a judge failure. Matching is exact raw-command equality (no patterns, aliases, or normalization). Entries from global and project settings are merged.

## Filesystem access

Top-level and profile `directories` grant persistent read-write access to the listed directories under `workspace-write`. Global and project entries are merged. The `read-only` policy demotes these paths to read-only access.

The `[sandbox]` section grants the Bash sandbox and the Read/Edit/Write/Grep path checks extra filesystem access on top of the built-in toolchain paths, `--add-dir`, and `directories`:

```toml
[sandbox]
read_only = ["~/.local/bin/claude"]   # read + execute
writable = ["~/.claude", "~/.cache/claude"]  # read + write + execute
```

`read_only` entries may be files or directories and grant read plus execute access (enough to run a single binary such as `~/.local/bin/claude` without opening its whole directory). `writable` entries grant read, write, and execute access. Both keys accept absolute paths, relative paths, and `~` expansion, and merge as a union across global settings, project settings, and the selected profile. Entries that do not exist are ignored with a warning in the log file. Under `--sandbox read-only`, `writable` entries are demoted to read-only, matching `directories`.

`directories = ["~/shared"]` also expands `~` (historically the path was ignored).

`--add-dir <PATH>` grants additional read-only access for one invocation and may be repeated. `--sandbox` selects `read-only`, `workspace-write`, or `danger-full-access`.

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

Profiles may overlay `disabled`, `only`, and `path` under `[profiles.<name>.skills]`.

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

An override replaces the built-in prompt; it is not appended. The selected system prompt is stored when a session is created and reused on continue or resume. Mutable AGENTS.md, skill, and environment context is rebuilt for each invocation.

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

Supported events are `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `PostToolUseFailure`, `Stop`, and `ErrorOccurred`. Matchers are accepted only for `SessionStart`, `PreToolUse`, `PostToolUse`, and `PostToolUseFailure`; omit the field for the other events. For matcher-capable events, a missing matcher or `"*"` matches all supported sources, and `|` separates exact matches.

Hook commands run outside the model tool sandbox with the project root as their working directory. Their input and decision protocol is documented in [Integrations](integrations.md).

## Toolbox tools

Toolbox directories come from `CAKE_TOOLBOX`, then repeated `--toolbox` arguments. When `CAKE_TOOLBOX` is unset, Cake scans `<config>/cake/tools`; an explicitly empty value disables that default. Earlier directories win name conflicts.

Each executable implements a describe and execute protocol. Valid tools are registered for the model with a `tb__` prefix. Broken tools are skipped with a warning rather than blocking startup.

Toolbox executables are trusted and unsandboxed. Under `read-only`, Cake skips toolbox discovery entirely so even the describe action cannot mutate the workspace. See [Integrations](integrations.md) for the protocol and [Security](security.md) for the trust boundary.

## Data and diagnostics

`CAKE_DATA_DIR` overrides Cake's cache and session data roots. Otherwise logs and telemetry use `~/.cache/cake/`, while resumable sessions use `~/.local/share/cake/sessions/`.

Set `RUST_LOG=cake=debug` or `RUST_LOG=cake=trace` for verbose file logging. Normal command output is unchanged.

## Related decisions

- [ADR 002](adr/002-agent-skills.md), the skills system.
- [ADR 003](adr/003-settings-profiles.md), settings profiles.

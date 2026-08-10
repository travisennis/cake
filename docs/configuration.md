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

Model definitions with the same name are replaced as a unit by the project definition. Additional `directories` are merged. Profiles may select a model and overlay behavior but may not define model providers.

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

Optional model fields are `api_type` (`chat_completions` or `responses`), `provider`, `provider_headers`, `temperature`, `top_p`, `max_output_tokens`, `reasoning_effort`, `reasoning_summary`, `reasoning_max_tokens`, and `providers`.

Set the selected model explicitly with `--model`, through a selected `--profile`, or with `default_model`. Reasoning and output-token CLI flags override the resolved model for one invocation.

All model references --- `default_model`, `--model`, profiles, and `[tools.bash.judge] model` --- use a `[[models]]` entry's `name` as the index into that entry's full configuration (provider, base URL, API key, temperature, reasoning, and other fields). The `model` field inside a `[[models]]` entry is the raw provider model identifier and is not used to reference a model elsewhere.

Relative `system_prompt`, `skills.path`, `directories`, and `[sandbox]` values resolve from the invocation working directory, including the created worktree when `--worktree` is active. Use absolute paths for global settings that must work from every project. Invalid files and unknown selected models fail before the provider request.

## Bash tool settings

`[tools.bash]` configures the Bash tool. The `[tools.bash.judge]` table holds settings for the LLM command-safety judge, which evaluates every Bash command before it runs and is the command-safety gate above the OS sandbox. The judge is default-on and fail-closed: an unavailable judge (unresolvable model, rubric read failure, timeout, transport error, or malformed verdict) blocks the command rather than running it ungated. A `block` verdict prevents the command from running; a `warn` verdict runs it with the judge's guidance prepended to the output. `enabled = false` or `CAKE_JUDGE=off` disables the judge entirely for every command.

```toml
[tools.bash.judge]
model = "zen"       # optional [[models]] name
timeout_secs = 30   # default 30; never below 1
rubric_file = ".cake/judge-rubric.md"  # optional user rubric guidance
enabled = true      # default true; false is the emergency bypass
allowlist = ["git status"]  # exact raw commands whose blocks are overridden
```

- `model`: optional `[[models]]` name for the judge's model, in the same vocabulary as `default_model` and `--model`. The name indexes a fully configured `[[models]]` entry, so the judge uses that entry's provider, base URL, API key, temperature, reasoning, and other fields. When unset, the judge uses the agent's configured model. An unknown name fails at load time, unless the judge is bypassed (`enabled = false` or `CAKE_JUDGE=off`), in which case the unused model config is inert. Every Bash command's text is transmitted to the judge's provider for evaluation; with the default it is the same provider the conversation already uses, and setting a different `model` sends command text to a provider the conversation never touches. The judge's request carries no conversation history.
- `timeout_secs`: bounded judge call timeout in seconds. Defaults to 30; values below 1 are raised to 1.
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

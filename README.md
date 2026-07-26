# cake

cake is a headless AI coding assistant for the terminal. It behaves like a Unix filter: provide a task, let the model inspect and modify the working tree, receive the result, and exit.

Cake supports OpenAI-compatible Chat Completions and Responses APIs, sandboxed tool execution, persisted conversations, machine-readable output, project instructions, hooks, skills, and user-defined tools.

## Install

Cake requires a current Rust toolchain:

```bash
git clone https://github.com/travisennis/cake.git
cd cake
cargo build --release
```

The binary is written to `target/release/cake`. Contributors should follow [CONTRIBUTING.md](CONTRIBUTING.md) instead.

## Configure a model

Create `$XDG_CONFIG_HOME/cake/settings.toml`, or `~/.config/cake/settings.toml` when `XDG_CONFIG_HOME` is unset:

```toml
default_model = "openrouter"

[[models]]
name = "openrouter"
model = "openai/gpt-5"
base_url = "https://openrouter.ai/api/v1/"
api_key_env = "OPENROUTER_API_KEY"
api_type = "chat_completions"
```

Then export the API key named by `api_key_env`:

```bash
export OPENROUTER_API_KEY="..."
```

Project settings may be placed in `.cake/settings.toml`. See [Configuration](docs/configuration.md) for model fields, precedence, profiles, skills, prompts, hooks, and toolbox tools.

## Use cake

```bash
# Prompt argument
cake "Explain the architecture"

# Pipe context while keeping the task separate
git diff --staged | cake "Review this change"

# Read the complete prompt from stdin
cake - < prompt.md

# Select a configured model
cake --model openrouter "Fix the failing test"
```

Run `cake --help` for the authoritative flag list.

### Sessions

Runs are persisted by default:

```bash
cake "Start the refactor"
cake --continue "Finish it"
cake --resume <UUID> "Try another approach"
cake --fork <UUID> "Explore this without changing the original session"
```

Use `--no-session` for an ephemeral run. Session and output compatibility contracts are documented in [Integrations](docs/integrations.md).

### Worktrees

`--worktree [NAME]` runs the task in an isolated Git worktree. Files matching patterns in a repository-root `.worktreeinclude` are copied into a newly created worktree.

```bash
cake --worktree experiment "Implement the change"
```

Cake removes an unchanged worktree when the task finishes and retains one with uncommitted changes or new commits.

### Machine-readable output

```bash
cake --output-format json "Summarize this repository" | jq '.result'
cake --output-format stream-json "Run the tests" | jq -c '.type'
cake --output-schema result.schema.json "Return structured findings"
```

`json` emits one completion object. `stream-json` emits newline-delimited task events as they happen. `--output-schema` constrains only the final model response.

### Sandboxing

Model-generated Bash commands use `workspace-write` sandboxing by default. Select `read-only`, `workspace-write`, or `danger-full-access` with `--sandbox`.

The sandbox is a filesystem boundary, not a network boundary. Hooks and toolbox executables are trusted extensions and run outside the Bash sandbox. Read [Security](docs/security.md) before changing permissions or enabling extensions.

## Project instructions and extensions

- `AGENTS.md` supplies project-specific agent instructions.
- `SKILL.md` files provide instructions loaded on demand.
- `hooks.json` files run local commands at lifecycle events.
- Toolbox executables register additional `tb__*` tools.
- `system.md` can replace the built-in system prompt.

Configuration and precedence are in [Configuration](docs/configuration.md); extension protocols are in [Integrations](docs/integrations.md).

## Documentation

  | Need                                                       | Authority                              |
  | ---------------------------------------------------------- | -------------------------------------- |
  | Configure and operate cake                                 | [Configuration](docs/configuration.md) |
  | Consume sessions, JSON output, hooks, or toolbox protocols | [Integrations](docs/integrations.md)   |
  | Understand permissions and trust boundaries                | [Security](docs/security.md)           |
  | Understand durable system boundaries                       | [Architecture](ARCHITECTURE.md)        |
  | Contribute changes                                         | [Contributing](CONTRIBUTING.md)        |
  | Understand past decisions                                  | [ADR archive](docs/adr/README.md)      |

## Platform support

Cake is developed and primarily validated on macOS. Linux builds include Landlock support, but CI provides less runtime coverage than on macOS.

## License

Cake is licensed under the [MIT License](LICENSE).

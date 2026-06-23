# Contributing to Cake

Thank you for your interest in contributing to Cake! This document provides contributor onboarding, development setup, common command invocations, and pull request workflow guidance for humans and agents.

Agent-specific operating rules live in [AGENTS.md](AGENTS.md). Follow AGENTS.md first when working as an agent; use this file for the shared development workflow.

## Development Setup

### Prerequisites

- [mise](https://mise.jdx.dev/) (or install Rust and just manually)
- Git

### Quick Setup

```bash
mise install    # installs pinned Rust toolchain and just
just setup      # installs cargo subcommands
prek install --hook-type pre-commit --hook-type commit-msg
```

This installs all required tools: clippy, rustfmt, cargo-edit, cargo-deny, cargo-insta, cargo-llvm-cov, cargo-crap, panache, prek, cocogitto. The `prek install` step configures git hooks.

Git hooks will automatically run: - **pre-commit**: `cargo fmt -- --check` (formatting verification) - **pre-commit**: `cargo clippy --all-targets -- -D warnings` (linting) - **commit-msg**: `cog verify --file` (conventional commit validation)

## Contributor Guides

### Code Style

- Use `thiserror` for custom errors and `anyhow` for application errors.
- Prefer Tokio `async fn` and `?` for error propagation.
- Default to deleting dead code. Use `#[cfg(test)]` only for test-only items.
- Use `#[expect(dead_code, reason = "...")]` only for serde fields that must exist for deserialization but are not read by application logic. The reason must say: `field required for serde deserialization; not read by application code`.
- Never use `#[allow(dead_code)]`.

### Adding a New Tool

Tools are defined in `src/clients/tools/`.

1. **Create the tool definition** in `tools.rs`:
   ```rust
   fn my_tool() -> Tool {
       Tool {
           type_: "function".to_string(),
           name: "MyTool".to_string(),
           description: "Description of what the tool does".to_string(),
           parameters: serde_json::json!({
               "type": "object",
               "properties": {
                   "param1": { "type": "string", "description": "..." }
               },
               "required": ["param1"]
           }),
       }
   }
   ```

2. **Add execution logic** in `execute_tool()`:
   ```rust
   "MyTool" => execute_my_tool(arguments).await,
   ```

3. **Implement the execution function**:
   ```rust
   async fn execute_my_tool(arguments: &str) -> Result<ToolResult, String> {
       // Parse arguments, validate paths, execute, return result
   }
   ```

4. **Register in tools list** (if applicable)

See [docs/design-docs/tools.md](docs/design-docs/tools.md) for tool framework details.

### Adding a New Conversation Type

Conversation items are defined in `src/clients/types.rs`.

1. **Extend the `ConversationItem` enum**:
   ```rust
   pub enum ConversationItem {
       // ... existing variants
       MyNewItem { field1: String, field2: i32 },
   }
   ```

2. **Update serialization** (`#[serde]` attributes)

3. **Update API translation** (`to_api_input()`, `build_messages()`, `to_streaming_json()`)

4. **Add pattern matches** in all `match` arms across the codebase

See [docs/design-docs/conversation-types.md](docs/design-docs/conversation-types.md) for data model details.

### Testing Changes

```bash
# Run all tests
just test

# Run tests for a specific module
cargo test module_name

# Run tests with coverage
just coverage

# Run snapshot tests
just snapshots

# Review and accept snapshot updates
cargo insta review

# Open HTML coverage report
just coverage-open

# Run the broad local validation suite
just check-full
```

Tests live alongside source files: - `src/module/mod.rs` → `tests/module_tests.rs` - Inline `#[cfg(test)]` modules are also used

Snapshot tests use `insta`. Run `just snapshots` after changing serialized output, prompts, API request construction, or other snapshot-backed behavior. If `.snap.new` files are created, inspect and accept or reject them with `cargo insta review`; do not leave `.snap.new` files in the worktree.

See [docs/design-docs/tools.md](docs/design-docs/tools.md) for testing patterns (tools use `tempfile` for isolation).

### Verification Expectations

For Rust changes:

1. Run the narrowest useful check first, such as `cargo check --tests`, `cargo test <module_or_test_name>`, or `cargo test`.
2. Run `cargo fmt` after code edits.
3. Run `just check-coverage` when adding or removing meaningful Rust code, changing tests or fixtures, changing coverage configuration or baselines under `ci/`, or changing dependency features in a way that affects compiled code.
4. Run `just ci` before final handoff for code, test, config, fixture, or dependency changes.

If `just ci` cannot be run, state the exact reason and list the narrower checks that were run instead.

For cfg-sensitive or platform-specific Rust changes, run the narrowest feasible target check for installed non-host targets affected by the change. On macOS, prefer `just clippy-linux` for Linux-sensitive changes when the target and cross compiler are available; otherwise use the closest feasible `cargo check --target ...` command. State any platform verification gap in the handoff.

For dependency changes, also run `just check-deps`. It is not part of `just ci`.

For documentation-only changes, run the narrowest useful Markdown or link checks instead of `just ci` when no code, tests, config, fixtures, generated indexes, dependency files, or build metadata changed. Explain the skip in the handoff.

This crate has no library target. Do not run `cargo test --lib`; use `cargo test <module_or_test_name>` for targeted tests or `cargo test` for the full test suite.

## Build/Lint/Test Commands

```bash
# Build release binary
cargo build --release

# Build and install to ~/bin
just install

# Run tests
cargo test

# Run tests for a specific module
cargo test <module_name>

# Run tests with coverage
just coverage

# Run snapshot tests
just snapshots

# Run coverage and open HTML report
just coverage-open

# Formatting
cargo fmt

# Linting
just clippy-strict

# Update dependencies
just update-dependencies

# Full CI check
just ci
```

## Running the App

```bash
# Set API key
export OPENROUTER_API_KEY=your_key_here

# Run binary directly
./target/release/cake "Your prompt here"

# Or with cargo
cargo run --release -- "Your prompt here"

# To get help
./target/release/cake --help
```

## Updating Rust Version

The project Rust toolchain is pinned in `rust-toolchain.toml`. When changing it: - Update `rust-toolchain.toml`. - Update matching project-toolchain pins in `.github/workflows/ci.yml`, `.github/workflows/release.yml`, and non-MSRV Rust jobs in `.github/workflows/scheduled.yml`. - Leave the scheduled `MSRV Compatibility` job pinned to the supported minimum Rust version unless intentionally changing MSRV. - Run `just rust-version-check` to verify pins are synchronized. - Run `just ci` before finishing the change.

## Git Workflow

- **Never commit directly to the master branch** --- verify current branch with `git branch` before committing
- Merge via feature branch + PR. Naming: `feat/xxx`, `fix/xxx`, `refactor/xxx`, `test/xxx`

## Commit Conventions

This project uses [Conventional Commits](https://www.conventionalcommits.org/). Commit messages are validated by a `commit-msg` hook.

**Format:** `<type>[(scope)]: <description>`

**Types:** `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`

**Recommended Scopes** (aligned with architecture):

  | Scope       | Description                                            |
  | ----------- | ------------------------------------------------------ |
  | `cli`       | Command-line interface and argument parsing            |
  | `agent`     | Agent orchestration, conversation loop, tool execution |
  | `responses` | Responses API backend                                  |
  | `chat`      | Chat Completions API backend                           |
  | `tools`     | Tool definitions (Bash, Read, Edit, Write, etc.)       |
  | `sandbox`   | Sandbox implementations (Seatbelt, Landlock)           |
  | `config`    | Configuration, sessions, data directory                |
  | `session`   | Session persistence and management                     |
  | `model`     | Model configuration and API types                      |
  | `prompts`   | System prompt construction, AGENTS.md integration      |
  | `logger`    | Logging configuration                                  |
  | `docs`      | Documentation changes                                  |
  | `tests`     | Test files and test infrastructure                     |

**Examples:** ```
feat(cli): add --verbose flag
fix(agent): handle timeout correctly
docs: update ARCHITECTURE.md with new module
refactor(tools): extract path validation into shared function
```

## Pull Request Process

1. Fork the repository
2. Create a new branch for your feature or bug fix (see Git Workflow naming conventions)
3. Make your changes and commit them following the commit conventions above
4. Write tests for new functionality
5. Ensure all CI checks pass (build, formatting, linting, tests)
6. Update affected documentation if needed
7. Submit a pull request

# Install required development tools and git hooks.
# Pinned tools are verified after installation; every missing or mismatched item
# is reported with its recovery, and setup exits nonzero.
setup:
    @echo "Checking Rust and Python installations..."
    @which rustc > /dev/null || { echo "ERROR: Rust not installed. Install from https://rustup.rs"; exit 1; }
    @command -v python3 > /dev/null || { echo "ERROR: Python 3 not installed. Install Python 3; the repository scripts use only the standard library."; exit 1; }
    @scripts/dev-tools.sh --install
    @echo "Installing git hooks declared in prek.toml..."
    prek install --hook-type pre-commit --hook-type pre-push --hook-type commit-msg
    @python3 scripts/doctor.py --scope setup --quiet
    @echo "Setup complete! Run 'just --list' to see available commands."

# Report whether this checkout can run its documented gates and hooks. Read-only:
# pass --json for a machine-readable report. Exit 1 names every required failure.
doctor flag="":
    @if [ -z {{ quote(flag) }} ]; then python3 scripts/doctor.py; else python3 scripts/doctor.py {{ quote(flag) }}; fi

# Run the primary Samply profile; pass --profiler instruments for the optional macOS path
profile *args:
    cargo build --profile profiling
    python3 scripts/profile-agent-loop.py {{ args }}

# Reject branch names outside the <type>/<slug> convention.
# just interpolates a recipe argument into shell source, so an otherwise legal
# Git ref such as `feat/x$(...)` would execute before Git ever saw it. The
# recipes below also shell-quote every interpolation; this check keeps the
# accepted character set narrow enough that the quoting has nothing to defend.
_check-branch-name name:
    @name={{ quote(name) }}; \
    case "$name" in \
        -*|/*|*/|*..*|*[!A-Za-z0-9._/-]*) \
            echo "ERROR: branch name may use only letters, digits, dot, underscore, hyphen, and /, and may not start with '-' or '/', end with '/', or contain '..'" >&2; \
            exit 1 ;; \
    esac; \
    case "$name" in \
        */*) ;; \
        *) echo "ERROR: branch name must start with a commit type, for example feat/turn-limits" >&2; exit 1 ;; \
    esac

# Start work on a feature branch cut from an up-to-date master
branch name: (_check-branch-name name)
    @# --no-track keeps the upstream unset, so a later `git push` cannot target master
    git fetch origin
    git switch --create {{ quote(name) }} --no-track origin/master

# Start work in a linked worktree on a new branch cut from an up-to-date master
worktree name: (_check-branch-name name)
    @test ! -e {{ quote(".cake/worktrees/" + name) }} || { printf 'ERROR: worktree %s already exists\n' {{ quote(".cake/worktrees/" + name) }} >&2; exit 1; }
    git fetch origin
    git worktree add {{ quote(".cake/worktrees/" + name) }} -b {{ quote(name) }} --no-track origin/master
    @# Untracked local files a checkout does not carry; keep in sync with .worktreeinclude
    @for f in .local.justfile .claude/settings.local.json; do \
        if [ -f "$f" ]; then \
            dest={{ quote(".cake/worktrees/" + name) }}/"$f"; \
            mkdir -p "$(dirname "$dest")" && cp "$f" "$dest"; \
        fi; \
    done
    @printf 'Worktree ready: %s (branch %s)\n' {{ quote(".cake/worktrees/" + name) }} {{ quote(name) }}

# Remove a finished worktree and its branch
worktree-rm name: (_check-branch-name name)
    git worktree remove {{ quote(".cake/worktrees/" + name) }}
    git branch --delete {{ quote(name) }}

# List active worktrees and the branch each one holds
worktrees:
    @git worktree list

# Open a pull request for the current branch (branch must be pushed).
# Pass up to four key=value options, in any order:
#   just pr labels="type:feature,area:cli" body=path/to/body.md
#   labels  comma-separated labels, checked against .github/labels.yml
#   body    pull request description file (default: fill title/body from commits)
#   title   pull request title (default: HEAD commit subject; wins over --fill)
#   base    target branch (default: master); pass the branch below for a stacked pull request
pr option1="" option2="" option3="" option4="":
    #!/usr/bin/env bash
    scripts/just-pr.sh {{ quote(option1) }} {{ quote(option2) }} {{ quote(option3) }} {{ quote(option4) }}

# Check code formatting (use in CI)
fmt-check:
    cargo fmt -- --check

# Auto-fix formatting
fmt:
    cargo fmt

# Run clippy with workspace lints (configured in Cargo.toml)
clippy:
    cargo clippy

# Ultra-strict clippy for the local gate (all targets and features)
_clippy-strict:
    cargo clippy --all-targets --all-features -- -D warnings

# Verify Rust toolchain pins stay synchronized
rust-version-check:
    sh scripts/check-rust-toolchain.sh

# Report dependency and tooling surfaces (pass e.g. --rust-channel PATH --as-of YYYY-MM-DD)
[no-exit-message]
dependency-sweep *args:
    @python3 scripts/dependency-sweep.py {{ args }}

# Report session metrics from transcripts + telemetry (pass e.g. --days 7, --model X)
session-metrics *args:
    @python3 scripts/session-metrics/report.py {{ args }}

# Synchronize repository labels with the committed vocabulary in .github/labels.yml
labels:
    @python3 scripts/sync-labels.py

# Verify repository labels match .github/labels.yml (exit 1 on drift; requires gh)
labels-check:
    @python3 scripts/sync-labels.py --check

# Validate .github/labels.yml structure without touching the network
labels-check-file:
    @python3 scripts/sync-labels.py --check-file

# Delete repository labels not present in .github/labels.yml
labels-prune:
    @python3 scripts/sync-labels.py --prune

# Clippy against the Linux target so local macOS checks cover CI-only cfg paths
clippy-linux:
    @rustup target list --installed | grep -qx 'x86_64-unknown-linux-gnu' || { echo "ERROR: missing Rust target x86_64-unknown-linux-gnu. Run: rustup target add x86_64-unknown-linux-gnu"; exit 1; }
    @which x86_64-linux-gnu-gcc > /dev/null || { echo "ERROR: missing x86_64-linux-gnu-gcc cross compiler required by aws-lc-sys"; exit 1; }
    cargo clippy --target x86_64-unknown-linux-gnu --all-targets --all-features -- -D warnings

# Lint for use of super::/self:: in production code
lint-imports:
    @grep -rn 'use super::' src/ --include='*.rs' | grep -v 'use super::\*;' | { if grep -q .; then echo "ERROR: Use crate:: paths, not super:: in production code. Found:"; grep -rn 'use super::' src/ --include='*.rs' | grep -v 'use super::\*;'; exit 1; fi; }
    @! grep -rn 'use self::' src/ --include='*.rs' | grep -q . || true
    @echo "Import lint passed!"

# Lint for dependency direction violations (nothing below cli imports cli, types is foundational)
lint-deps:
    @grep -rn 'use crate::cli[;:]' src/ --include='*.rs' | grep -v '^src/cli/' | grep -v '^src/main.rs:' | { if grep -q .; then echo "ERROR: Module imports crate::cli from outside the CLI layer. Violations:"; grep -rn 'use crate::cli[;:]' src/ --include='*.rs' | grep -v '^src/cli/' | grep -v '^src/main.rs:'; exit 1; fi; }
    @grep -rn 'use crate::' src/types/ --include='*.rs' | grep -v 'use crate::types' | { if grep -q .; then echo "ERROR: src/types/ imports from a non-types crate module. Violations:"; grep -rn 'use crate::' src/types/ --include='*.rs' | grep -v 'use crate::types'; exit 1; fi; }
    @echo "Dependency lint passed!"

# Check module sizes against thresholds (informational, always passes)
lint-module-size:
    python3 scripts/lint-module-size.py

# Check that docs/domain-glossary.md still matches the code it describes
lint-domain-glossary:
    python3 scripts/lint-domain-glossary.py

# Run the local gate for code changes: formatting, complexity, strict Clippy,
# and the import, dependency-direction, module-size, and glossary lints.
check: rust-version-check fmt-check cc-check _clippy-strict lint-imports lint-deps lint-module-size lint-domain-glossary
    echo "Local checks passed!"

# Run the pre-push gate: the local code gate plus the documentation checks.
pre-push:
    just check
    just docs-check
    git diff --check

# Check markdown formatting and lint over the repository's Markdown: the files git
# tracks, so untracked scratch notes are neither checked nor rewritten (requires
# panache; installed by `just setup`)
docs-check:
    @scripts/docs-corpus.sh --check

# Auto-format the repository's Markdown (tracked files; format an untracked draft
# explicitly with `panache format <path>`)
docs-fmt:
    @scripts/docs-corpus.sh --fmt

# Check for denied/advisory dependencies and disallowed licenses (requires cargo-deny)
check-deps:
    cargo deny check advisories
    cargo deny check licenses

# Build documentation with warnings denied
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items

# Check per-function cyclomatic complexity against the baseline.
# New functions must stay at or below the CC target; existing functions may not exceed max(target, baseline CC).
cc-check:
    scripts/check-cc.sh

build:
    cargo build --release

# Build the native release binary and record its exact byte size in the committed baseline.
# This baseline is target-specific; do not compare binaries built for different release targets.
binary-size-baseline: build
    python3 scripts/binary-size-baseline.py

# Print release notes from conventional commits; scope with a range, e.g. `just changelog v0.1.0..HEAD` (release-time step)
changelog range="":
    @cog changelog {{ range }} 2>/dev/null

update-dependencies:
    cargo upgrade -i allow && cargo update

install:
    cargo build --release
    mkdir -p ~/bin
    tmp="$HOME/bin/.cake-install-$$" && cp target/release/cake "$tmp" && chmod 755 "$tmp" && mv -f "$tmp" "$HOME/bin/cake"

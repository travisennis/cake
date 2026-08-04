# Install required development tools
setup:
    @echo "Checking Rust installation..."
    @which rustc > /dev/null || { echo "ERROR: Rust not installed. Install from https://rustup.rs"; exit 1; }
    @echo "Installing required cargo tools..."
    cargo install cargo-edit --quiet 2>/dev/null || true
    cargo install cargo-deny --quiet 2>/dev/null || true
    cargo install cargo-insta --quiet 2>/dev/null || true
    cargo install cargo-llvm-cov --quiet 2>/dev/null || true
    cargo install cargo-crap --version 0.2.2 --locked --quiet 2>/dev/null || true
    cargo install panache --version 3.0.0 --locked --quiet 2>/dev/null || true
    cargo install prek --quiet 2>/dev/null || true
    cargo install --locked cocogitto --quiet 2>/dev/null || true
    @echo "Setup complete! Run 'just --list' to see available commands."

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

# Open a pull request for the current branch (branch must be pushed)
pr:
    gh pr create --base master --fill

# Check code formatting (use in CI)
fmt-check:
    cargo fmt -- --check

# Auto-fix formatting
fmt:
    cargo fmt

# Run clippy with workspace lints (configured in Cargo.toml)
clippy:
    cargo clippy

# Ultra-strict clippy for CI (deny all warnings, lint all targets)
clippy-strict:
    cargo clippy --all-targets --all-features -- -D warnings

# Ultra-strict clippy without default features, matching the CI matrix
clippy-no-default-features:
    cargo clippy --all-targets --no-default-features -- -D warnings

# Verify Rust toolchain pins stay synchronized
rust-version-check:
    sh scripts/check-rust-toolchain.sh

# Report session metrics from transcripts + telemetry (pass e.g. --days 7, --model X)
session-metrics *args:
    @python3 scripts/session-metrics/report.py {{args}}

# Print the session briefing: open issues by status, active ExecPlans, recent research
brief:
    @python3 scripts/session-brief.py

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

# Run tests
test:
    cargo test --quiet

# Run tests with all features enabled, matching CI
test-all-features:
    cargo test --all-features --quiet

# Run insta snapshot tests (requires cargo-insta; installed by `just setup`)
snapshots:
    cargo insta test

# Lint for use of super::/self:: in production code (test modules use super::* is allowed)
lint-imports:
    @grep -rn 'use super::' src/ --include='*.rs' | grep -v 'use super::\*;' | { if grep -q .; then echo "ERROR: Use crate:: paths, not super:: in production code. Found:"; grep -rn 'use super::' src/ --include='*.rs' | grep -v 'use super::\*;'; exit 1; fi; }
    @! grep -rn 'use self::' src/ --include='*.rs' | grep -q . || true
    @echo "Import lint passed!"

# Lint for dependency direction violations (nothing below cli imports cli, types is foundational)
lint-deps:
    @grep -rn 'use crate::cli[;:]' src/ --include='*.rs' | grep -v '^src/cli/' | grep -v '^src/main.rs:' | { if grep -q .; then echo "ERROR: Module imports crate::cli from outside the CLI layer. Violations:"; grep -rn 'use crate::cli[;:]' src/ --include='*.rs' | grep -v '^src/cli/' | grep -v '^src/main.rs:'; exit 1; fi; }
    @grep -rn 'use crate::' src/types/ --include='*.rs' | grep -v 'use crate::types' | grep -v '_tests\.rs:' | { if grep -q .; then echo "ERROR: src/types/ imports from a non-types crate module. Violations:"; grep -rn 'use crate::' src/types/ --include='*.rs' | grep -v 'use crate::types' | grep -v '_tests\.rs:'; exit 1; fi; }
    @echo "Dependency lint passed!"

# Run the primary local checks, including the always-on CI command set
ci: rust-version-check check-linux fmt-check clippy-strict clippy-no-default-features test-all-features check-coverage lint-imports lint-deps lint-module-size
    echo "All checks passed!"

# Run the required pre-push gate for code, config, CI, fixture, and dependency changes
pre-push: ci

# Run the macOS correctness path used by GitHub Actions
ci-macos: rust-version-check fmt-check clippy-strict clippy-no-default-features test-all-features
    echo "macOS CI checks passed!"

# Run the Linux compatibility gate command used by GitHub Actions
check-linux:
    cargo check --all-features

# Run the broad local validation suite
check-full: ci check-deps doc build
    echo "Full check suite passed!"

# Check module sizes against thresholds (informational, always passes)
lint-module-size:
    python3 scripts/lint-module-size.py

# Check for denied/advisory dependencies (requires cargo-deny)
check-deps:
    cargo deny check advisories

# Build documentation with warnings denied
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items

# Run tests with coverage (requires cargo-llvm-cov)
coverage:
    cargo llvm-cov --html

# Print coverage summary (requires cargo-llvm-cov)
coverage-summary:
    cargo llvm-cov --summary-only

# Check coverage threshold and untested-complexity regression.
# CRAP_REGRESSION_EPSILON defaults to 0.5 in scripts/cargo-crap.sh to absorb rounding-level coverage noise.
check-coverage:
    scripts/check-coverage.sh

# Run coverage and open report
coverage-open:
    cargo llvm-cov --html --open

# Generate coverage in lcov format for CI
coverage-lcov:
    cargo llvm-cov --lcov --output-path lcov.info

# Regenerate the macOS cargo-crap baseline from current coverage.
# Run this after intentional code or test changes alter coverage/complexity, then commit ci/cargo-crap-baseline.json with the change.
change-risk-baseline:
    mkdir -p ci
    cargo llvm-cov --lcov --output-path lcov.info
    scripts/cargo-crap.sh --lcov lcov.info --format json --output ci/cargo-crap-baseline.json

# Print a reviewer-friendly macOS cargo-crap regression report
change-risk-report:
    cargo llvm-cov --lcov --output-path lcov.info
    scripts/cargo-crap.sh --lcov lcov.info --baseline ci/cargo-crap-baseline.json --format markdown

update-dependencies:
    cargo upgrade -i allow && cargo update    

# Check markdown formatting and lint (requires panache; installed by `just setup`)
docs-check:
	panache format --check . --quiet
	-panache lint . --quiet

# Auto-format all markdown files
docs-fmt:
	panache format .

build:
    cargo build --release

# Print release notes from conventional commits; scope with a range, e.g. `just changelog v0.1.0..HEAD` (release-time step)
changelog range="":
    @cog changelog {{ range }} 2>/dev/null

install:
    cargo build --release
    mkdir -p ~/bin
    tmp="$HOME/bin/.cake-install-$$" && cp target/release/cake "$tmp" && chmod 755 "$tmp" && mv -f "$tmp" "$HOME/bin/cake"

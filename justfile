cargo_crap_excludes := "--exclude 'tests/**' --exclude 'src/clients/agent/agent_tests.rs' --exclude 'src/clients/chat_completions_tests.rs' --exclude 'src/config/settings_tests.rs' --exclude 'src/clients/tools/sandbox/linux.rs'"

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
    cargo install panache --quiet 2>/dev/null || true
    cargo install prek --quiet 2>/dev/null || true
    cargo install --locked cocogitto --quiet 2>/dev/null || true
    @echo "Setup complete! Run 'just --list' to see available commands."

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

# Regenerate task indexes from task file metadata
task-index:
    ahm index

# Verify generated task indexes are current (no ahm dependency)
task-index-check:
    @python3 scripts/check-indexes.py

# Mark a task as Completed, move active/<id>.md to completed/, and refresh indexes
task-complete id:
    ahm task complete {{id}}

# Mark a task as Cancelled, move active/<id>.md to cancelled/, and refresh indexes
task-cancel id:
    ahm task cancel {{id}}

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

# Run the primary local checks, including the always-on CI command set
ci: task-index-check rust-version-check check-linux fmt-check clippy-strict clippy-no-default-features test-all-features lint-imports lint-module-size
    echo "All checks passed!"

# Run the integration test suite only (fast binary-level smoke tests)
smoke:
    cargo test --test exit_codes --test debug_models --test stdin_handling --test session_telemetry

# Run the macOS correctness path used by GitHub Actions
ci-macos: rust-version-check fmt-check clippy-strict clippy-no-default-features test-all-features
    echo "macOS CI checks passed!"

# Run the Linux compatibility gate command used by GitHub Actions
check-linux:
    cargo check --all-features

# Run the broad local validation suite
check-full: ci check-coverage check-deps doc build
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

# Check coverage threshold and untested-complexity regression
check-coverage:
    @OUTPUT="$(cargo llvm-cov --summary-only)"; \
    printf '%s\n' "$OUTPUT"; \
    COVERAGE="$(printf '%s\n' "$OUTPUT" | grep "^TOTAL" | grep -oE '[0-9]+\.[0-9]+%' | tail -1 | tr -d '%')"; \
    echo "Coverage: ${COVERAGE}%"; \
    if [ "$(echo "$COVERAGE < 90" | bc -l)" = "1" ]; then \
        echo "Coverage below 90%"; \
        exit 1; \
    fi; \
    cargo llvm-cov --lcov --output-path lcov.info; \
    cargo crap --lcov lcov.info --baseline ci/cargo-crap-baseline.json --fail-regression --summary {{cargo_crap_excludes}}

# Run coverage and open report
coverage-open:
    cargo llvm-cov --html --open

# Generate coverage in lcov format for CI
coverage-lcov:
    cargo llvm-cov --lcov --output-path lcov.info

# Regenerate the macOS cargo-crap baseline from current coverage
change-risk-baseline:
    mkdir -p ci
    cargo llvm-cov --lcov --output-path lcov.info
    cargo crap --lcov lcov.info --format json --output ci/cargo-crap-baseline.json {{cargo_crap_excludes}}

# Print a reviewer-friendly macOS cargo-crap regression report
change-risk-report:
    cargo llvm-cov --lcov --output-path lcov.info
    cargo crap --lcov lcov.info --baseline ci/cargo-crap-baseline.json --format markdown {{cargo_crap_excludes}}

update-dependencies:
    cargo upgrade -i allow && cargo update    

build:
    cargo build --release

install:
    cargo build --release
    mkdir -p ~/bin
    cp target/release/cake ~/bin/cake

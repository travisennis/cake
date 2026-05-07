# Agent Readiness Report: cake

**Date**: 2026-04-09
**Overall Score**: 56/93 (60% pass rate)
**Maturity Level**: L2 — Managed
**Interpretation**: cake has strong foundations for agent-assisted development with excellent L1/L2 coverage, fast CI (~1 minute), and thorough documentation. Its strongest signal is the tight feedback loop: `just ci` runs format, lint, test, and import checks in ~12 seconds locally. The main limiting factor for L3 is the gap in code quality enforcement (no complexity limits, no file/function length limits, no dead code or duplication detection) and missing task discovery infrastructure (no issue/PR templates, no labels).
**Language Context**: Rust's static type system and strict compiler provide strong safety guarantees that compensate for some L3 gaps. The compiler catches type errors, unused variables, and many classes of bugs at compile time. However, Rust's verbose compiler output increases token consumption per agent iteration, making CI output conciseness more impactful than in dynamic languages.

---

## Platform Summary

| Aspect | Detected |
|--------|----------|
| Code Hosting | GitHub |
| CI System | GitHub Actions |
| Task Management | GitHub Issues |
| Platform CLI | `gh` v2.89.0 |

---

## Feedback Loop Profile

| Metric | Value |
|--------|-------|
| Time to validate (lint + typecheck + test) | ~12 seconds (`just ci`) |
| Approximate output tokens per validation run | ~2,000-3,000 |
| Targeted execution available | yes (`cargo test <module>`) |
| Fail-fast configured | yes (CI quick-check gate + pre-commit hooks) |

---

## Verification Bottleneck

The single biggest factor making agent-produced changes expensive to verify is the lack of targeted linting and code size limits. Every change triggers a full-repo clippy scan, and several source files exceed 500 lines (agent.rs: 1,343; responses.rs: 1,021; chat_completions.rs: 910). Agents must read and reason about these large files in their entirety, increasing both token cost and error rate on modifications.

---

## Level Progress

| Level | Name | Criteria Passed | Pass Rate | Threshold | Unlocked |
|-------|------|----------------|-----------|-----------|----------|
| L1 | Initial | 10/10 | 100% | 80% | yes |
| L2 | Managed | 24/25 | 96% | 80% | yes |
| L3 | Standardized | 19/34 | 56% | 90% | no |
| L4 | Measured | 3/18 | 17% | 90% | no |
| L5 | Optimized | 0/6 | 0% | 90% | no |

---

## Top Recommendations

Ordered by priority: L1 failures first, then L2, then L3+. Within each level, highest-impact failures first.

1. **`large_file_detection`** (L2 — Style & Validation) — Add a pre-commit hook or CI check for large files (e.g., `prek` hook or git LFS) to prevent repository bloat
2. **`issue_labeling_system`** (L2 — Task Discovery) — Define and apply consistent labels (bug, feature, good-first-issue, etc.) to issues
3. **`cyclomatic_complexity`** (L3 — Style & Validation) — Add a complexity analysis tool (e.g., `cargo clippy -- -W clippy::cognitive_complexity` or a custom lint) to catch overly complex functions
4. **`max_file_length`** (L3 — Style & Validation) — Refactor files exceeding 500 lines (agent.rs: 1,343; responses.rs: 1,021; chat_completions.rs: 910; bash_safety.rs: 808; bash.rs: 788; main.rs: 793; edit.rs: 835) into smaller modules
5. **`max_function_length`** (L3 — Style & Validation) — Add a clippy lint or CI check for function length limits (e.g., `too_many_lines` threshold)
6. **`dead_code_detection`** (L3 — Style & Validation) — Add `#![warn(dead_code)]` or a CI step that flags dead code; several `#[allow(dead_code)]` and `#[expect(dead_code)]` annotations suggest awareness of the issue
7. **`duplicate_code_detection`** (L3 — Style & Validation) — Add a duplicate code detection tool to CI (e.g., `jscpd` for Rust or manual review of similar patterns)
8. **`targeted_linting`** (L3 — Style & Validation) — Configure prek or CI to run clippy only on changed files rather than the entire codebase
9. **`automated_pr_review`** (L3 — Build System) — Add a review bot (e.g., GitHub Actions that comment on PRs with lint results, coverage changes, or size concerns)
10. **`devcontainer`** (L3 — Dev Environment) — Add `.devcontainer/devcontainer.json` for one-click reproducible development environments
11. **`local_services_setup`** (L3 — Dev Environment) — While cake is a CLI with minimal external dependencies, consider adding a docker-compose.yml or justfile recipe for mocking the API endpoint during development
12. **`error_tracking_contextualized`** (L3 — Debugging & Observability) — Integrate an error tracking service (e.g., Sentry) to capture runtime errors with context
13. **`distributed_tracing`** (L3 — Debugging & Observability) — Add OpenTelemetry instrumentation or trace IDs for request correlation across API calls
14. **`log_scrubbing`** (L3 — Security) — Add log sanitization to prevent API keys and PII from appearing in log output
15. **`pii_handling`** (L3 — Security) — Implement PII redaction in logging and session storage
16. **`backlog_health`** (L3 — Task Discovery) — Grow and label the issue backlog with descriptive titles and consistent tags
17. **`machine_readable_output`** (L3 — Agent Efficiency) — Configure linters and test runners to emit JSON or SARIF output for structured parsing

---

## Criteria Detail

### 1. Style & Validation — 7/10 (70%)

| Status | Criterion | Finding |
|--------|-----------|---------|
| ✓ | `formatter` | rustfmt configured with rustfmt.toml (edition 2024, max_width 100) |
| ✓ | `lint_config` | Clippy with pedantic + nursery lints, deny unwrap/expect in Cargo.toml |
| ✓ | `type_check` | Rust provides static type checking by default; cargo check runs in CI |
| ✓ | `strict_typing` | Clippy pedantic+nursery enabled, deny unwrap_used/expect_used, deny missing_errors_doc |
| ✓ | `pre_commit_hooks` | prek.toml configures cargo fmt, cargo clippy, and conventional commit checks |
| ✓ | `naming_consistency` | Rust naming conventions enforced by clippy; consistent module naming across codebase |
| ✗ | `large_file_detection` | No git LFS or pre-commit hook to detect large files |
| ✓ | `code_modularization` | Clear module boundaries (cli, clients, config, models, prompts) with thin layers and no catch-all directories |
| ✗ | `cyclomatic_complexity` | No complexity analysis tooling configured |
| ✗ | `max_file_length` | 6 files exceed 500 lines (agent.rs: 1,343; responses.rs: 1,021; chat_completions.rs: 910; edit.rs: 835; bash_safety.rs: 808; bash.rs: 788) |
| ✗ | `max_function_length` | No function length limits enforced |
| ✗ | `dead_code_detection` | No dead code detection tooling; several `#[allow(dead_code)]` and `#[expect(dead_code)]` annotations present |
| ✗ | `duplicate_code_detection` | No duplicate code detection tooling |
| ✗ | `targeted_linting` | Pre-commit hooks and CI run clippy on entire codebase, not scoped to changed files |
| ✓ | `incremental_type_checking` | Rust's incremental compilation is default; cargo check is incremental |
| ✗ | `tech_debt_tracking` | No TODO scanner or tech debt tracking in CI |
| — | `n_plus_one_detection` | Skipped: no database |
| ✗ | `max_file_length_strict` | Multiple files exceed 300 lines |
| ✗ | `max_function_length_strict` | No function length limits enforced |

### 2. Build System — 10/11 (91%)

| Status | Criterion | Finding |
|--------|-----------|---------|
| ✓ | `build_cmd_doc` | `cargo build --release` documented in README, CONTRIBUTING.md, and AGENTS.md; command executes successfully |
| ✓ | `deps_pinned` | Cargo.lock exists and is committed |
| ✓ | `vcs_cli_tools` | `gh` v2.89.0 installed and authenticated |
| ✓ | `fast_ci_feedback` | CI completes in ~1 minute (quick check 12s, clippy 14s, test 31s, format 9s) |
| ✓ | `single_command_setup` | `cargo build --release` + `prek install` documented in CONTRIBUTING.md |
| ✓ | `release_automation` | release.yml workflow for tags with multi-platform builds (Linux, macOS x86_64, macOS ARM) |
| ✓ | `deployment_frequency` | 94 commits in the last month; release workflow ready for tags |
| ✓ | `release_notes_automation` | git-cliff (cliff.toml) configured for changelog generation |
| ✓ | `agentic_development` | Project is an AI agent tool; commit history shows AI-assisted development patterns |
| ✗ | `automated_pr_review` | No Danger.js, review bot, or automated review comments |
| ✓ | `feature_flag_infrastructure` | Cargo features exist (landlock feature flag for Linux sandboxing) |
| ✗ | `build_performance_tracking` | No build timing metrics or caching dashboards |
| ✗ | `heavy_dependency_detection` | No dependency size analysis tooling |
| ✓ | `unused_dependencies_detection` | cargo-udeps runs in scheduled workflow |
| — | `dead_feature_flag_detection` | Skipped: no feature flags beyond landlock |
| — | `monorepo_tooling` | Skipped: not a monorepo |
| — | `version_drift_detection` | Skipped: not a monorepo |
| ✓ | `fast_ci_feedback_optimized` | CI completes in ~1 minute, well under 5-minute threshold |
| ✗ | `progressive_rollout` | No canary deployment or gradual rollout mechanism |
| ✗ | `rollback_automation` | No one-click rollback capability |

### 3. Testing — 8/11 (73%)

| Status | Criterion | Finding |
|--------|-----------|---------|
| ✓ | `unit_tests_exist` | 19 test modules across source files + 1 integration test file; 310 total tests covering actual project logic |
| ✓ | `unit_tests_runnable` | `cargo test --all-features` completes successfully: 305 unit tests + 5 integration tests pass |
| ✓ | `test_naming_conventions` | Consistent `mod tests` pattern across all source files |
| ✓ | `test_isolation` | Multiple independent test suites (19 modules + integration tests); cargo test runs them in parallel |
| ✓ | `integration_tests_exist` | tests/stdin_handling.rs with 5 integration tests for CLI behavior |
| ✓ | `test_coverage_thresholds` | Coverage threshold of 30% enforced in CI with cargo llvm-cov |
| ✓ | `targeted_test_execution` | cargo test supports `--package`, `--lib`, module filtering; project has test breadth across modules |
| ✓ | `test_output_minimal` | `cargo test --quiet` configured in justfile and CI workflow |
| ✗ | `flaky_test_detection` | No test retry, quarantine, or flaky test tracking |
| ✗ | `test_performance_tracking` | No test timing metrics or slow-test detection |
| ✗ | `test_coverage_strict` | Coverage threshold is 30%, far below 95% |

### 4. Documentation — 7/9 (78%)

| Status | Criterion | Finding |
|--------|-----------|---------|
| ✓ | `readme` | Comprehensive README.md with installation, usage, configuration, architecture, and contributing sections |
| ✓ | `agents_md` | AGENTS.md with actionable repo-specific guidance: build/test commands, git workflow, code style, debugging skill reference |
| ✓ | `documentation_freshness` | All key docs updated within 180 days (AGENTS.md: Mar 29, README: Apr 6, ARCHITECTURE.md: Apr 4, CONTRIBUTING.md: Mar 28) |
| ✓ | `api_schema_docs` | docs/references/ contains responses-api.md and chat-completions-api.md; design docs cover conversation types and tools |
| ✓ | `automated_doc_generation` | `just doc` runs `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items`; CI scheduled workflow also runs docs |
| ✓ | `service_flow_documented` | ARCHITECTURE.md has detailed codemap, system boundaries, cross-cutting concerns, and reading list |
| ✓ | `documentation_coherence` | AGENTS.md, README, ARCHITECTURE.md, CONTRIBUTING.md align well, delegate clearly, and internal references resolve correctly |
| ✓ | `skills` | .agents/skills/ directory with debugging-cake and evaluating-cake skills |
| ✗ | `agents_md_validation` | No CI step validates that AGENTS.md commands (e.g., `just ci`) still work |
| ✗ | `decision_records` | No ADR directory or formal decision records; design-docs exist but don't capture ADR-style rationale/trade-offs |

### 5. Dev Environment — 3/5 (60%)

| Status | Criterion | Finding |
|--------|-----------|---------|
| ✓ | `env_template` | .cake/settings.toml.example with documented env vars (OPENCODE_ZEN_API_TOKEN, OPENROUTER_API_KEY, CAKE_SANDBOX) |
| ✓ | `configuration_complexity` | ~4 env vars, 7 config files in root (Cargo.toml, rustfmt.toml, bacon.toml, cliff.toml, cog.toml, deny.toml, prek.toml); under thresholds |
| ✗ | `devcontainer` | No .devcontainer directory |
| — | `devcontainer_runnable` | Skipped: no devcontainer |
| — | `database_schema` | Skipped: no database |
| ✗ | `local_services_setup` | No docker-compose.yml or mock API setup for development |
| — | `migration_safety` | Skipped: no database |

### 6. Debugging & Observability — 2/8 (25%)

| Status | Criterion | Finding |
|--------|-----------|---------|
| ✓ | `structured_logging` | tracing library with structured output, file appender with daily rotation (7-day retention) |
| ✓ | `code_quality_metrics` | Coverage reporting in CI with codecov integration; threshold enforcement |
| ✗ | `error_tracking_contextualized` | No Sentry, Bugsnag, or similar error tracking service |
| ✗ | `distributed_tracing` | No OpenTelemetry or trace ID instrumentation |
| ✗ | `metrics_collection` | No Prometheus, Datadog, or custom metrics collection |
| — | `health_checks` | Skipped: CLI tool |
| ✗ | `profiling_instrumentation` | No CPU/memory profiling tools configured |
| ✗ | `alerting_configured` | No PagerDuty, OpsGenie, or alert rules |
| ✗ | `deployment_observability` | No deploy tracking or dashboards |
| ✗ | `runbooks_documented` | No runbooks directory or linked incident response docs |
| ✗ | `circuit_breakers` | No circuit breaker patterns (retry logic exists but is not a circuit breaker) |

### 7. Security — 4/7 (57%)

| Status | Criterion | Finding |
|--------|-----------|---------|
| ✓ | `gitignore_comprehensive` | .gitignore excludes target/, .DS_Store, coverage output, worktrees |
| ✓ | `secrets_management` | API keys via env vars; GitHub Secrets used for CI (CODECOV_TOKEN) |
| ✓ | `codeowners` | CODEOWNERS file with per-module ownership aligned to architecture layers |
| — | `branch_protection` | Skipped: no admin access to verify |
| ✓ | `dependency_update_automation` | .github/dependabot.yml configured for weekly Cargo and GitHub Actions updates |
| ✗ | `log_scrubbing` | No log sanitization for PII or API keys |
| ✗ | `pii_handling` | No PII redaction mechanisms |
| ✗ | `automated_security_review` | No CodeQL, Snyk, or SonarQube in CI (cargo-deny runs in scheduled workflow for advisories only) |
| — | `secret_scanning` | Skipped: no admin access to verify |
| — | `dast_scanning` | Skipped: CLI tool |
| ✗ | `privacy_compliance` | No GDPR/privacy tooling |

### 8. Task Discovery — 4/6 (67%)

| Status | Criterion | Finding |
|--------|-----------|---------|
| ✓ | `issue_templates` | .github/ISSUE_TEMPLATE/ with bug report and feature request YAML templates |
| ✗ | `issue_labeling_system` | No consistent labels on issues (3 open issues, no labels visible) |
| ✓ | `pr_templates` | .github/pull_request_template.md with summary, motivation, changes, and checklist |
| ✓ | `git_workflow_documented` | CONTRIBUTING.md documents branching strategy (feat/fix/refactor prefixes), PR process, and commit conventions |
| ✗ | `backlog_health` | Only 3 open issues with no consistent labeling or descriptive categorization |
| ✓ | `repo_hygiene` | 7 remote branches all recent (within ~2 weeks), 0 stale PRs, all 7 PRs merged |

### 9. Product & Analytics — 0/2 (0%)

| Status | Criterion | Finding |
|--------|-----------|---------|
| ✗ | `error_to_insight_pipeline` | No error tracking auto-creating issues in task tracker |
| ✗ | `product_analytics_instrumentation` | No product analytics instrumentation (Mixpanel, Amplitude, etc.) |

### 10. Agent Efficiency — 5/6 (83%)

| Status | Criterion | Finding |
|--------|-----------|---------|
| ✓ | `single_command_validation` | `just ci` runs format+lint+test+import checks and completes successfully |
| ✓ | `fail_fast_configuration` | CI has quick-check gate that fails early; pre-commit hooks stop on first failure |
| ✓ | `deterministic_builds` | Cargo.lock pins dependencies; dtolnay/rust-toolchain pins Rust version in CI |
| ✓ | `ci_output_concise` | CI uses `--message-format=short` for clippy and `--quiet` for cargo test |
| ✗ | `machine_readable_output` | No JSON/SARIF/JUnit XML output from linters or test runners in CI |
| ✓ | `actionable_error_messages` | Rust compiler and clippy provide file path, line number, and clear descriptions by default |
| ✓ | `agent_sandbox_safety` | Project implements OS-level sandboxing (macOS Seatbelt, Linux Landlock); agent commands run in sandboxed env |
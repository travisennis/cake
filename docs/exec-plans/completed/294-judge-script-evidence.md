## Observe referenced scripts in Bash preflight

This living ExecPlan follows docs/workflow/exec-plans.md.

## Purpose / Big Picture

For issue #294, make `bash path/to/script.sh` reviewable using the actual file contents while preserving path grants, bounded reads, and fail-closed judgment.

## Progress

- [x] (2026-09-14) Confirmed #314 closed, moved #294 to Ready and claimed it.
- [x] Recorded the observation boundary in ADR-029 before implementation.
- [x] Implement evidence collection and request serialization.
- [x] Add focused tests and update security and tool documentation.
- [x] Run focused tests, just check, just cc-check, and three-pass preflight.
- [x] Complete implementation and review records for archival.
- [x] Implementation committed for PR handoff; the issue record owns push and review status.

## Surprises & Discoveries

The parent's entry-point prose and #294's Blocked status were stale. Local mock servers initially needed sandbox escalation. Strict Clippy found error-context and integer-conversion issues, which were fixed. The shared judge pipeline takes an owned request and does not own a sandbox tool context.

## Decision Log

Use the deliberately narrow observation boundary in ADR-029. Do not add a shell interpreter, recursive reads, or new dependencies. Unsupported commands remain model judged with explicitly uncollected evidence. Inspection-only callers do not acquire implicit host filesystem access.

## Outcomes & Retrospective

Literal script references now carry bounded untrusted contents to the judge. Eleven focused tests cover collection, encoding, denial, bypass, and actual request/execution behavior. The final `just check` gate passed with all eleven new tests, as did `just snapshots` and `just cc-check`. CC checks pass. Both affected provider request snapshots were reviewed with cargo insta review. No provider credentials or live model calls were used.

Preflight completed three passes: rules/documentation conformance, correctness, and simplification. Kept findings led to directory-handle traversal to prevent ancestor symlink redirects, honest block-result observation notes, and smaller functions to satisfy complexity ratchets. Recursive parsing, a new dependency, and generalized action-packet work were rejected as outside #294. Root AGENTS.md, task #294, this plan, ARCHITECTURE.md, docs/security.md, CONTRIBUTING.md, complexity guardrails, and ADR-018/029 informed review; no nested AGENTS.md exists. Linux runtime validation remains with CI because only the macOS Rust target is installed. just check-full and live evaluation were not required or run.

This change does not bind execution to observed bytes. Unsupported shell forms and nested dependencies remain explicitly unobserved, and inspection-only callers without tool context do not read host files.

## Context and Orientation

src/clients/tools/bash.rs runs preflight before spawn. src/clients/judge.rs owns JudgeRequest and JSON prompt construction. src/clients/tools/mod.rs owns shared in-process path grants. The observed pipeline applies bypass and raw-command allowlisting; evidence must not alter either comparison or execution text.

## Plan of Work

First add an isolated script-observation module under src/clients/tools, called after bypass and before the provider in Bash preflight. Add typed optional script evidence to JudgeRequest and serialize it as untrusted JSON. Identify observed paths in tool output and report collection failures without file contents.

Next test literal recognition, actual bytes, quoting, denied and symlinked paths, size boundaries, invalid encoding, and special files. Test prompt serialization and fail-closed preflight without provider access. Update docs/security.md and the Bash tool description with coverage and race limitations.

Finally run validation and preflight, record results in #294, complete this plan, commit the narrow diff, push, and open a labeled PR without merging it.

## Concrete Steps

From the repository root run `cargo test script_evidence`, focused judge tests, `just check`, and `just cc-check`. Tests should pass without provider credentials. Use targeted Panache checks for changed Markdown and inspect relative links.

## Validation and Acceptance

A literal script invocation includes its current bytes in the judge request. Denied paths, oversized files, and unreadable files fail before execution. Unsupported syntax never falsely claims full evidence. Bypass remains usable. macOS checks run locally; Linux OS sandbox behavior is verified by CI unless the cross-toolchain is available locally. No live provider spend is required.

## Idempotence and Recovery

Tests use temporary fixtures and can be repeated. Preserve unrelated changes. If verification fails, fix the scoped implementation and rerun affected checks.

## Artifacts and Notes

Issue #314 supplies independent evaluation cases; this issue adds deterministic observation tests without changing their independent expected labels.

## Interfaces and Dependencies

An optional typed observation on JudgeRequest carries resolved path and contents. Collection takes ToolContext and the raw command. Use existing libc and standard filesystem APIs on macOS/Linux to refuse symlinks at open and avoid FIFO blocking.

Revision note (2026-09-14): completed implementation and preflight; replaced path-based open with directory-handle traversal after reviewing the enumerated symlink race class.

Review revision (2026-09-14, PR #553): corrected the shipped rubric to consume script evidence without accepting embedded instructions or authorization claims; updated configuration, integration, and architecture disclosures. Collection failures now explain that the judge was not called and give recovery guidance, with script names and assertions that failed collection never claims inspection. Existing fail-closed telemetry classes and recognition scope remain unchanged. Follow-up #554 tracks direct execution and residual field cases. ADR-029 remains proposed pending review, consistent with the issue record. The rubric snapshot was regenerated and its diff reviewed; focused rubric and script-evidence tests, just check, just cc-check, targeted Markdown checks, and git diff --check passed. Three-pass review found no need for broader parsing or new configuration.

# 0001 --- Code comments are banned; rationale lives in ADRs

- **Status:** Accepted
- **Date:** 2026-11-20
- **Supersedes:** none

## Context

The Rust sources carried about 1,000 lines of ordinary comments. An audit found three failure modes. Some comments restated the next line (`// Validate model name format`, section banners, `// mise` beside a path). Some described code that no longer existed or compared behavior to a removed implementation. Some excused a shortcut (`// this function's change-risk budget allows no new branch`, `// Dummy value — the flag check below short-circuits`). The remainder held real decisions, but they were invisible to anyone reading the architecture docs and could not be reviewed as a set.

A comment also has no owner. When the surrounding function is rewritten, nothing fails if the comment becomes false, so a plausible sentence can outlive the invariant it described. That is worse than silence: a wrong comment is read as evidence.

## Decision

Ordinary comments are banned in Rust production sources under `src/`. Rationale belongs in `docs/adr/`, organised by subsystem. The enforcement is `scripts/lint-comments.py`, run by `just check`:

- A file may hold no more ordinary comments than its entry in `ci/comment-baseline.json`.
- A file absent from the baseline may hold none.
- Entries may only fall. `--write-baseline` rewrites the file after a cleanup and refuses a count above the existing entry; an exemption belongs in the lint, not in a raised allowance.

Two comment forms are exempt:

- **Doc comments** (`///`, `//!`). Clap derives `--help` from them, so they are part of the CLI compatibility surface, not commentary.
- **`// SAFETY:` blocks**. `clippy::undocumented_unsafe_blocks` requires the justification at the unsafe operation, and deleting it would weaken review of every `unsafe` block.

The ratchet exists because a cleanup need not finish in one change. A partial pass records its progress, and later passes lower the number until the only permitted comments are the two exemptions above. The current baseline is empty: the first pass removed every ordinary comment, and each decision moved to records 0002--0006.

## Consequences

- A new comment fails `just check` unless the baseline is deliberately raised, which forces the author to ask whether the sentence is a decision that belongs in an ADR or a restatement that belongs nowhere.
- Review reads records 0002--0006 with the subsystem they govern. The records name modules and symbols, never line numbers, so a rename does not invalidate them.
- The audit that produced this record classified every ordinary comment as a tautology, a stale claim, an excuse, or a decision. Tautologies and stale claims were deleted; excuses were deleted and filed as issues; decisions were moved here. The classification is preserved in the commit history rather than as a second artifact that would itself go stale.
- The records are prose, not generated output. `just docs-check` verifies formatting and links; it cannot verify that a record still matches the code. An editor who reverses a decision must update the record in the same change.

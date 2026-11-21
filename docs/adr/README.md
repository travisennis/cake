# Architecture decision records

This directory holds the decisions that used to live as comments in `src/`. It is the only sanctioned home for rationale: ordinary comments in Rust production code are banned by `scripts/lint-comments.py`, and `just check` fails on a new one.

## Why decisions live here

A comment is an unversioned, unindexed, unvalidated claim attached to one line. It rots when the code moves, it duplicates what the code already says, and it gives an implementation permission to stay hacky. A decision record instead states the context, the choice, and the consequence, so a later change can tell whether the decision still holds. The code keeps the behavior; this directory keeps the reasoning.

## What stays in code

Two comment forms are exempt because a tool consumes them:

- **Doc comments** (`///`, `//!`). Clap derives `--help` text from them, so they are a user-facing compatibility surface; rustdoc renders the rest. They are documentation, not commentary, and deleting them changes the CLI.
- **`// SAFETY:` blocks**. Clippy's `undocumented_unsafe_blocks` requires a justification immediately at the unsafe operation. The justification is part of the proof obligation, so it must stay beside the `unsafe` block.

## Format

Each record has a title, a status, a context, the decisions, and the consequences. A record may hold a set of related decisions for one subsystem, because the invariants in this codebase are coupled: a record per line would be a second copy of the code. Every decision names the module or symbol it governs so an editor can find it without line numbers.

Statuses: `Accepted`, `Superseded by NNNN`, or `Deprecated`. Change a status; do not rewrite history. A new decision that reverses an old one gets a new record and marks the old one superseded.

## Index

- [0001](0001-comments-are-banned.md) --- Code comments are banned; rationale lives in this directory
- [0002](0002-cli-and-task-lifecycle.md) --- CLI resolution, startup ordering, interrupts, and exit classes
- [0003](0003-agent-loop-and-providers.md) --- Agent turn lifecycle, streaming recovery, and provider shaping
- [0004](0004-tools-and-command-execution.md) --- Tool semantics, command execution, and file mutation
- [0005](0005-sandbox-and-filesystem-grants.md) --- Sandbox policy, path grants, and platform confinement
- [0006](0006-configuration-sessions-and-extensions.md) --- Settings, sessions, hooks, gates, and trusted extensions

## Adding a record

1. Search this directory for the subsystem first; extend an existing record when the decision belongs to the same boundary.
2. Name the invariant, the choice, and what breaks if the choice is reversed.
3. Delete the comment that motivated the record and run `scripts/lint-comments.py --write-baseline` if the ratchet needs to fall.
4. Run `just docs-check` and `just check`.

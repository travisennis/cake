---
name: delete-comment
description: Audit comments in Rust production sources, classify each as tautology, excuse, stale, or decision, delete the first three, file issues for excuses, and carry decisions into the commit message. Preserves doc comments and `// SAFETY:` blocks, then lowers the comment-lint ratchet.
---

# Delete Comment

Use this skill when asked to purge comments or audit comment quality. The end state is the same every time: code holds behavior, the commit message and existing authority documents hold rationale, and `scripts/lint-comments.py` holds the ratchet.

## Scope

Default scope: ordinary `//` and `/* */` comments under `src/`. Those are what `just lint-comments` bans.

Never delete or rewrite these, whatever the request says, unless the user explicitly overrides this skill after being told why:

- **Doc comments** (`///`, `//!`). Clap derives `--help` from the doc comments on the CLI struct; deleting them changes a compatibility surface. Rustdoc renders the rest.
- **`// SAFETY:` blocks.** `clippy::undocumented_unsafe_blocks` requires the justification beside the unsafe operation. The text is part of the proof obligation, not commentary.

If the user asks to purge doc comments too, confirm separately: inventory them, identify every one clap consumes, and treat the rest as a documentation change rather than a comment cleanup. Do not touch docs as a side effect of an ordinary-comment pass.

Scripts, Markdown, and shell are out of scope. This skill changes Rust sources only; it creates no new records.

## Procedure

### 1. Inventory

```bash
python3 scripts/lint-comments.py --list          # every ordinary comment remaining
python3 scripts/lint-comments.py                 # current ratchet position
git status --short --branch
```

Read every listed comment plus enough surrounding code to judge it. A comment's meaning is often in the invariant it protects, not in its wording. If the list is large, partition by directory and keep one agent per partition; never let two editors touch the same file.

### 2. Classify each comment

  | Category            | Test                                                                                                                                                                                | Action                                                |
  | ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------- |
  | **Tautology**       | Repeats what the next line, type, or identifier already says. Section banners and labels beside a literal.                                                                          | Delete.                                               |
  | **Stale**           | Describes removed code, a previous implementation, line numbers, milestones, issue numbers that no longer resolve to the reasoning, or a process artifact ("this change's budget"). | Delete. Verify the replacement behavior exists first. |
  | **Excuse**          | Grants permission for a shortcut: "dummy value", "for now", "keeps the complexity baseline", "the fallback covers future early returns", "might have issues".                       | Delete, then file an issue to remove the shortcut.    |
  | **Decision Record** | States a non-obvious choice and what breaks if it is reversed: ordering, fail-closed behavior, compatibility, safety.                                                               | Put it in the commit body, then delete.               |

Tie-break: if deleting the comment would make a future editor likely to undo a deliberate choice, it is a decision record. If the code alone still teaches the choice, it is a tautology.

Do not classify a comment as a decision just because it is long. Long prose can be stale.

### 3. Delete

Delete the whole line for a full-line comment. For a trailing comment, remove only the comment and keep the code. Do not reflow or "improve" the surrounding code; a comment pass is not a refactor. Run `cargo fmt` afterward because removing a trailing comment can change layout.

### 4. File an issue for every excuse

Before deleting an excuse, capture it:

```bash
gh issue create \
  --title "<short imperative: remove the dummy TurnResult>" \
  --label "type:chore" \
  --label "area:<area>" \
  --body "<what the comment admitted, the file and function, why it is a shortcut, and the behavior the replacement must preserve>"
```

Use only labels from `.github/labels.yml`; inspect the file rather than inventing a name. Reference the issue from the commit body that deletes the comment, so the audit trail connects the shortcut to its removal. Never leave an excuse comment in place because filing felt heavy: delete it and file, or keep it and say why it is a decision instead.

### 5. Carry decisions into the commit message

Do not create an ADR or any other new record. Before committing, put each decision in the commit body: the choice, the failure it prevents, and the condition that would reverse it. Name modules and symbols, never line numbers: line numbers go stale the moment the code moves. If the decision changes a documented contract, update that surface's existing authority document; if it needs discussion, open an issue and link it from the commit. The rationale then lives in git history and the issue tracker, where it is versioned and searchable, without a new file to maintain.

### 6. Lower the ratchet

```bash
python3 scripts/lint-comments.py --write-baseline
```

The baseline must fall or stay flat. The writer refuses a count above the existing entry, so a raise cannot be committed accidentally. Raising an entry to admit a new comment is not the escape hatch: either the sentence is a decision that belongs in the commit message, or it should not exist. If a legitimate exception appears, the right change is an exemption in `scripts/lint-comments.py` with a stated reason.

### 7. Verify

```bash
cargo check --all-targets
just check
just docs-check
```

Comments do not change behavior, so a release build is not required unless the pass also changed code. Report the exact commands and any that could not run. Check `git diff` for accidental code movement: a deletion pass should show comment-only removals plus `cargo fmt` reflows.

### 8. Commit and hand off

Use a `chore` commit, or the type that matches the work if the pass is part of a larger change. The body should state the counts: comments deleted by category, decisions recorded, issues filed. If the pass leaves comments behind deliberately, say why and ensure the baseline records them.

## Failure modes

- Deleting a comment that documents an external protocol or a user-visible string. Those are contracts, not commentary: move them to the authority document instead.
- Treating a `// SAFETY:` continuation line as an ordinary comment and breaking Clippy.
- Moving a decision into a new ADR or other durable record, which recreates the process the repository removed.
- Deleting a decision comment without putting its rationale in the commit body, which loses the invariant.
- Rewriting a comment instead of recording the decision, which produces a better-worded tautology.
- Filing an issue that names no acceptance condition, so the shortcut cannot be closed.
- Running the pass across worktrees concurrently without partitioning files.

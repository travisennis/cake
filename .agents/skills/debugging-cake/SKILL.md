---
name: debugging-cake
description: |
  Triage a recent cake CLI failure. Use only for reactive, user-reported failures:
  - The CLI returned `None`, empty output, or a clearly truncated response
  - The CLI reported "Tool error:" with no further detail
  - A task crashed, hung, or was interrupted mid-stream
  - The user reports their last cake run "broke" and wants to know why
  For deeper session review, quality assessment, or scoring how cake performed,
  use `analyzing-cake-sessions` instead. For sandbox `Operation not permitted`
  errors, use `debugging-sandbox`.
---

Follow the repository-owned [Debugging Failed Cake Runs runbook](../../../docs/runbooks/debugging-cake.md) for this procedure.

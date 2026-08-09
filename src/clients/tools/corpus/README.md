# Command-safety corpus

JSONL case corpus migrated from the deleted `bash_safety` guard (issue #106, Phase A): one JSON object per line, `{"command", "expect", "note"}` with `expect` one of `blocked` / `warned` / `allowed`.

The guard that compiled this corpus in with `include_str!` was removed when the LLM judge became the command-safety gate (issue #72, Milestone 5). The data is preserved here for the judge-driven runner (issue #174, Phase B), which drives the judge path against these cases and adds judge-specific ones.

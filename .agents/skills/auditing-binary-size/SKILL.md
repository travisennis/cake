---
name: auditing-binary-size
description: "Analyze and audit the release binary size to identify what's contributing to bloat. Use when asked to check binary size, audit binary bloat, investigate why the binary is large, or monitor binary size over time."
---

Follow the repository-owned [Auditing Binary Size runbook](../../../docs/runbooks/auditing-binary-size.md) for this procedure. The audit is read-only by default: use already-installed tools when possible, and do not run setup, install audit tools, regenerate tracked baselines, commit, or modify repository files unless the user explicitly requests that follow-up. If a required tool is missing, report the skipped analysis and the command that would enable it.

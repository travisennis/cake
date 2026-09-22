---
status: proposed
date: 2026-09-14
decision-makers: Travis Ennis
informed: issue 294
---

# Observe literal script references before Bash judgment

## Context and Problem Statement

The judge sees a script's name without its contents. Issue #294 calls for bounded, sandbox-authorized reads as untrusted evidence.

## Decision Drivers

- Preserve model judgment, exact raw-command allowlisting, and emergency bypass.
- Never turn repository text into authorization or widen filesystem grants.
- Avoid claiming that inspecting one file proves all execution effects.

## Considered Options

- Inline scripts into the executed command.
- Implement full shell interpretation and recursive dependency discovery.
- Collect a bounded observation for a deliberately narrow literal invocation.

## Decision Outcome

Choose bounded observations for direct `bash`, `sh`, `zsh`, `dash`, `ksh`, `ksh93`, `ash`, `mksh`, and `pdksh` invocations with literal arguments, including quoted paths and an optional `--`. Recognize bare interpreter names and exact `/bin/` and `/usr/bin/` spellings. Unsupported shell syntax receives an explicit uncollected-evidence marker and still goes to the judge. This recognizer supplies evidence; it never authorizes execution. The raw command remains unchanged. Read one regular UTF-8 file, at most 32 KiB, through the same in-process grants used by Read. Denied, missing, oversized, binary, or special files and non-UTF-8 canonical paths fail closed before the provider call. Bypass skips collection.

Enumerated bypass classes are shell expansion, quoting, compound commands and cwd changes; symlink escapes and replacement races; devices and FIFOs; oversized or non-UTF-8 data; prompt injection and forged authorization in file contents; recursive script dependencies; and mutation after observation. JSON encoding and explicit untrusted labeling preserve instruction boundaries. Canonical path validation and directory-handle traversal with no-follow, nonblocking regular-file opens constrain observation. Neither recognition nor the observation promises complete shell coverage or binds later execution to the observed bytes. The OS sandbox remains the filesystem boundary on macOS and Linux. Concurrent mutation remains a documented risk.

Interpreter identity, startup files, and environment-driven behavior are not verified. Script operands resolve against the tool cwd; a missing cwd file fails collection rather than collecting a PATH alternative. Interpreter options (including `+` forms), wrappers such as `env` or `busybox`, and arbitrary interpreter paths remain unobserved. These limits apply equally to the expanded Bourne-family set.

### Consequences

The model can inspect ordinary script invocations without command rewriting. Script contents are sent to the configured judge provider, like Read tool results sent to the agent provider. Normal telemetry contains no script text. Tool output identifies the observed path without echoing contents. Introspection and corpus callers without a tool context report that evidence was not collected; they do not silently read arbitrary host files.

## More Information

This extends [ADR-018](018-llm-judge-command-gate.md)'s command observation while preserving its model-based policy pipeline. Trusted context and generalized evidence contracts remain issues #315 through #317.

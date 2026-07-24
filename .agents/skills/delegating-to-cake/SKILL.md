---
name: delegating-to-cake
description: Use cake as the sole subagent and codebase access boundary for repository research, implementation, debugging, testing, refactoring, documentation, or code review. Activate when the user requires all codebase work to be performed by executing cake, asks Codex to use cake as a subagent, or wants a task delegated end to end through the cake CLI rather than using local file or code tools directly.
---

# Delegate To Cake

Treat cake as the only agent allowed to inspect or change the repository. Own orchestration, prompt design, session management, and synthesis; delegate repository discovery, reads, edits, commands, tests, and review judgments to cake.

## Prevent Recursive Delegation

This skill governs a parent agent that can launch the cake CLI. If the current system identity says `You are cake` or otherwise establishes that this agent was already launched by cake, do not execute another cake process. Fulfill the delegated repository task with cake's native tools and instructions instead.

The parent must translate the user's objective into a role-specific delegated prompt. Do not forward instructions such as "use cake as a subagent" into the child prompt; they describe the parent's execution boundary, not the child's task.

## Enforce The Boundary

After activation:

- Execute `cake` from the target repository's working directory.
- Do not use file-read, search, edit, patch, git, build, test, browser, or other tools to obtain repository facts or mutate the repository.
- Do not inspect cake's persisted session files. Consume its CLI result instead.
- Allow host-side handling of cake's stdout, stderr, exit status, and session ID. This is orchestration, not a second codebase access path.
- Base every code claim on a cake report. Label inference as inference.
- Never silently bypass cake because a direct operation would be faster.

If cake is unavailable or cannot authenticate, report the blocker. Do not fall back to direct codebase access unless the user explicitly relaxes the constraint.

## Choose A Run Mode

Use the least-permissive sandbox that can complete the delegated role:

  | Role        | Session          | Sandbox           | Purpose                                                              |
  | ----------- | ---------------- | ----------------- | -------------------------------------------------------------------- |
  | Scout       | New              | `read-only`       | Locate code, read docs, trace behavior, propose a change             |
  | Reviewer    | New              | `read-only`       | Independently review existing code or a working-tree change          |
  | Implementer | New, then resume | `workspace-write` | Inspect, edit, format, and test one coherent change                  |
  | Verifier    | New              | `workspace-write` | Run checks that need build artifacts; explicitly forbid source edits |

Use one writer at a time in a shared working tree. Parallel read-only runs are acceptable when they answer independent questions. Use `--worktree` only for deliberately isolated competing implementations; delegate any later comparison or integration to cake as well.

## Invoke Cake Reliably

Prefer the machine-readable summary:

```bash
cake --sandbox read-only --output-format json '<complete delegated prompt>'
```

For an implementation:

```bash
cake --sandbox workspace-write --output-format json '<complete delegated prompt>'
```

Read the top-level `result`, `error`, `subtype`, `session_id`, `usage`, `turns`, and `elapsed_time`. Preserve the returned `session_id`; use its UUID instead of `--continue`, whose latest-session selection can become ambiguous:

```bash
cake --resume <UUID> --sandbox workspace-write --output-format json '<follow-up prompt>'
```

Use `--fork <UUID>` only when a new line of reasoning should inherit context without modifying the original conversational branch. Start a wholly new session for an independent review so it does not inherit the implementer's conclusions.

Do not use `--no-session` for multi-step work. Select `--model`, `--profile`, reasoning controls, or `--skills` only when the user or project configuration gives a reason; otherwise honor cake's configured defaults.

### Capture Long-Running Output

Treat process yielding and process completion as different states. `--output-format json` writes its summary when cake finishes; an empty output chunk from a still-running process is not an empty cake result.

For every runner:

1. Start cake and retain its child-process handle, session identifier, job identifier, or equivalent continuation token.
2. If the runner yields or times out while cake is still active, poll or await that same process. Do not launch a replacement invocation.
3. Accumulate stdout and stderr across all chunks until the process reports an exit status.
4. Parse JSON only after completion. Classify empty output as a cake failure only when the process has exited and the accumulated stdout is empty.
5. Preserve stderr for diagnostics; cake may use it for progress or errors even when stdout is machine-readable.

**Codex-specific guidance:** When cake is launched through Codex's nested `exec_command`, inspect the returned object for `session_id`. If present, the command yielded before completion. Repeatedly call `write_stdin` with that `session_id` and an empty `chars` value, append each returned `output`, and stop only when an `exit_code` is returned. A `functions.exec` cell ID identifies the outer JavaScript execution, not the nested cake process; waiting on the outer cell does not replace polling the inner `exec_command` session.

**Other agents and harnesses:** Use the runtime's corresponding long-process primitive, such as awaiting the child process, polling a job handle, reading an asynchronous stream to EOF, or resuming a terminal session. If the runtime exposes no continuation handle, set its command timeout above the expected cake duration or use `--output-format stream-json` and consume records until `task_complete`. Never interpret a timeout, yield notification, or temporarily empty pipe as cake's final response.

## Write Complete Delegated Prompts

Cake cannot infer the parent's unstated plan. Every initial prompt must include:

1. **Role and objective**: scout, implementer, reviewer, or verifier; state the concrete outcome.
2. **Repository intake**: inspect applicable `AGENTS.md` and project workflow docs before acting.
3. **Scope and compatibility**: name relevant constraints, behavior to preserve, and prohibited changes.
4. **Working-tree safety**: inspect existing changes, preserve user work, and do not commit or push unless explicitly requested.
5. **Autonomy**: investigate enough context, complete the task end to end, and resolve routine ambiguity using repository conventions.
6. **Evidence contract**: report file and line references for code claims, files changed, exact commands/checks run and outcomes, skipped checks, remaining risks, and unresolved uncertainty.
7. **Output contract**: ask for a concise final report rather than a plan-only response.

Do not ask cake to dump large files. Ask it to synthesize the relevant behavior with precise references. Narrow follow-up prompts around unanswered questions instead of restarting broad discovery.

## Run The Workflow

### 1. Establish Scope

For a small, well-specified task, start one implementer session directly. For an ambiguous or high-risk task, first run a read-only scout and use its report to construct the implementation prompt.

Ask the scout to identify the owning modules, applicable instructions, current behavior, tests, compatibility risks, and a bounded implementation approach. Treat its answer as a research artifact, not proof that the proposed change is correct.

### 2. Implement In One Session

Start a new workspace-write session with the full objective plus any useful scout findings. Tell cake to inspect the repository itself before trusting those findings, implement surgically, and run risk-appropriate checks.

Resume that same UUID for corrections, failed checks, or clarified requirements. Session continuity avoids repeated discovery and lets cake retain why it made earlier choices.

### 3. Obtain Independent Review

For nontrivial or risky changes, start a fresh read-only reviewer session. Provide the original objective and ask it to inspect the current working-tree diff and surrounding code. Require findings first, ordered by severity, with file/line evidence; require an explicit no-findings statement when applicable and identify test gaps or residual risk.

Do not tell the reviewer what bugs to find or provide the implementer's rationale unless the rationale is itself part of the specification. Fresh context is the independence mechanism.

### 4. Feed Findings Back

Resume the implementer UUID with the raw reviewer findings. Ask cake to validate each finding against the code, fix only substantiated issues, rerun relevant checks, and explain any rejected finding.

Use another fresh review only when changes were material or the first review found correctness, security, compatibility, or data-loss risks.

### 5. Verify Without Editing

When checks require generated artifacts, start a workspace-write verifier session with an explicit prohibition on source edits. Ask it to inspect the final diff, run the exact relevant checks, and report whether the working tree changed during verification. If it finds a defect, return it to the implementer rather than letting the verifier silently repair it.

For narrow low-risk changes, the implementer's checks may be enough. Scale extra cake calls to blast radius and uncertainty.

## Handle Failures

- **Exit 1 or `error`**: read the reported failure. Resume the same UUID when cake can diagnose or continue the task.
- **`subtype: cut_off`**: resume the same UUID and ask it to continue from the interrupted work, inspect current state, and finish verification.
- **Exit 2**: treat as provider, authentication, rate-limit, or network failure. Retry only when transient; otherwise report the configuration blocker.
- **Exit 3**: correct the invocation, model selection, prompt input, or session UUID. Do not ask cake to edit the repository to solve a caller error.
- **Repeated tool failure**: resume once with the exact failure and ask for an alternative. If it persists, use a fresh diagnostic cake run or report the blocker.
- **Missing or vague evidence**: resume and request the omitted references, diff summary, or exact check outcomes before synthesizing a result.

Do not interpret a fluent final response as successful implementation. Require completed checks or a concrete explanation of why they could not run.

## Preserve Epistemic Honesty

This operating model trades direct inspection for a controlled delegation boundary. The parent cannot independently confirm cake's repository claims, so compensate with scoped prompts, structured evidence, session isolation, independent review, and explicit uncertainty.

In the final handoff:

- State what cake changed or found.
- Report exact checks cake says it ran and their outcomes.
- State remaining risks and skipped checks.
- Say that repository work and verification were performed through cake when that distinction matters.
- Do not claim personal inspection of files or tests.

The parent remains accountable for whether the evidence is sufficient, even though cake performs the codebase work.

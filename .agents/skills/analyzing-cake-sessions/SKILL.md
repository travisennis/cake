---
name: analyzing-cake-sessions
description: |
  Analyze a persisted cake CLI session file (`~/.local/share/cake/sessions/{uuid}.jsonl`) to find issues and recommend project improvements, optionally also scoring how well cake performed. Use when asked to read, review, audit, evaluate, assess, analyze, or score a cake session, session UUID, or recent CLI run for tool errors, repeated calls, permission issues, performance problems, missing context, prompt or instruction gaps, missing tools, session integrity problems, agent reasoning quality, or task completion quality. Not for live failure triage of the user's most recent run — use `debugging-cake` for that.
---

# Analyzing Cake Sessions

Use this skill to inspect persisted cake session files and produce a concise,
evidence-backed report on what cake could improve.

The workflow always produces a **findings report**. When the request is
phrased as evaluation, scoring, assessment, or quality review (rather than
investigation or debugging), also append the **Quality scoring appendix**
described in Phase 5. Triggers like "how well did cake do", "rate this run",
"evaluate this session" call for the appendix; "what went wrong", "find
issues", "audit this session" do not.

## Phase 0: Understand CLI Capabilities

Ground analysis in what cake can actually do, not what you (the analyzing
agent) can do.

1. Read these files when relevant to the findings:
   - `ARCHITECTURE.md` — overall architecture
   - `src/clients/tools/mod.rs` — available tools
   - `src/clients/tools/*.rs` — tool capabilities and parameters

2. Only raise issues that are valid for the CLI's actual toolset and prompts.

## Phase 1: Locate and Validate the Session

### Inputs

Accept any of:

- Absolute session path, usually `~/.local/share/cake/sessions/{uuid}.jsonl`
- Session UUID, resolved as `~/.local/share/cake/sessions/{uuid}.jsonl`
- Custom data root from `CAKE_DATA_DIR`, resolved as
  `$CAKE_DATA_DIR/sessions/{uuid}.jsonl`

If the input is ambiguous, list recent candidates and choose by working
directory, timestamp, and visible task content.

```bash
# List recent session files
ls -t ~/.local/share/cake/sessions/*.jsonl 2>/dev/null | head -10

# Latest session
ls -t ~/.local/share/cake/sessions/*.jsonl 2>/dev/null | head -1
```

### Format and Validation

Current persisted cake sessions use append-only JSONL format version 4:

1. First non-empty line: one `session_meta` record.
2. Each CLI invocation appends `task_start`.
3. The invocation may append `prompt_context` audit records.
4. Conversation records are `message`, `reasoning`, `function_call`,
   `function_call_output`.
5. The invocation should end with `task_complete`.

Validate:

- File exists and is valid JSONL (allow a possible partial trailing record).
- First record has `type: session_meta` and `format_version: 4`.
- Inspect: `session_id`, `working_directory`, `model`, `tools`, `cake_version`,
  git metadata.

Treat files beginning with `session_start`, `init`, or `result` as legacy or
unsupported unless compatibility is the subject of the analysis. Redirected
`--output-format stream-json` output is not a persisted resumable session.

## Phase 2: Understand Record Types

Distinguish records restored into model history (which consume LLM context)
from purely diagnostic metadata. This is critical for any recommendation
about "session bloat" or "context growth".

**LLM-visible records** (restored into model history via `--continue` / `--resume`):

| Type | Purpose |
|------|---------|
| `message` | User, assistant, system, or developer text |
| `reasoning` | Model reasoning traces (echoed back to the API) |
| `function_call` | Tool invocation request |
| `function_call_output` | Tool execution result |

**Metadata / audit-only records** (NOT restored into model history):

| Type | Purpose |
|------|---------|
| `session_meta` | Session metadata, first line only |
| `task_start` | CLI invocation boundary |
| `task_complete` | Task result, duration, turn count, token usage, permission denials |
| `prompt_context` | Audit record of mutable prompt context (AGENTS.md, skills, env) |
| `hook_event` | Hook execution diagnostic trail |
| `skill_activated` | Skill usage audit record |

Key implications:

- `hook_event`, `prompt_context`, `skill_activated`, and task boundaries
  occupy session file space but add **zero** LLM context.
- `prompt_context` is an audit snapshot — the LLM receives fresh prompt
  context rebuilt on each invocation, not these records.

> **Reasoning `summary` field caveat**: The `summary` on reasoning records is
> a protocol artifact, not a human-crafted summary. On the Chat Completions
> backend, cake hardcodes `["Thinking..."]` (never sent to the LLM). On the
> Responses API backend, the summary is echoed back for multi-turn
> conversations, but its informativeness depends on the provider. Always
> evaluate reasoning quality by the `content` field, not `summary`.

## Phase 3: Segment Tasks and Reconstruct Flow

- Group records from each `task_start` through its matching `task_complete`.
- For each task, capture: task id, timestamp, duration, subtype, `is_error`,
  turns, `tool_call_count`, usage, final result or error, `permission_denials`.
- Flag trailing tasks without `task_complete`, incomplete assistant messages,
  malformed final records, abrupt endings, suspicious status fields.
- Correlate every `function_call` with its `function_call_output` by
  `call_id`. Flag missing outputs, orphan outputs, duplicate ids, malformed
  arguments, invalid tool names, unexpected ordering.
- Summarize each user request and final assistant response.

## Phase 4: Inspect Each Surface

### Tool Use

- Tool call errors, tool result errors, sandbox/permission denials, network
  failures, parse failures, missing files, command failures, failed tests,
  edit conflicts.
- Repeated or near-identical tool calls (stuck patterns).
- Verbose tool results that polluted context: full logs, broad dumps,
  unfiltered searches, complete build output.
- Cases where `rg`, `jq`, a targeted read, a narrower command, a structured
  parser, or a better argument would have worked.

### Prompt and Context

- Review `prompt_context` for AGENTS.md, skills, environment, cwd, date,
  sandbox permissions, and available tools.
- Missing, stale, unclear, or conflicting context that affected behavior.
- Whether repository instructions were followed (especially verification
  after code/config/dependency changes).
- Missing tools, unclear tool descriptions, missing instructions, or
  permission guidance that would have helped.

### Reasoning

- Clear problem decomposition vs. stuck patterns.
- Appropriate planning and sequencing.
- Self-correction after mistakes.
- Use the `content` field, not `summary`.

### Instruction Following

- Compare the user's request with the assistant's actions and final answer.
- Flag premature final answers, partial implementation, unnecessary
  questions, ignored constraints, missing verification, unreported blockers,
  overbroad changes, poor preservation of user changes.
- Cases where the agent should have used a skill, read local docs first,
  asked for permission, or avoided a risky action.

### Performance and Cost

- Use `task_complete.duration_ms`, `turn_count`, and `usage` for slow,
  expensive, or inefficient tasks.
- High reasoning tokens, many turns for simple tasks, repeated high-token
  tool outputs, large context growth, retry loops, truncation, timeout risks.
- Correlate session id and timestamps with logs at
  `~/.cache/cake/cake.YYYY-MM-DD.log` (or `$CAKE_DATA_DIR/cake.YYYY-MM-DD.log`).

## Phase 5: Produce the Report

### Issue Categories

- `tool_call_error` — tool invocation failed or returned an unexpected error
- `tool_result_error` — tool output malformed, incomplete, verbose, misleading
- `repeated_tool_call` — same/equivalent call repeated without new information
- `permission_issue` — sandbox/filesystem/approval/network/OS handling blocked
  progress or was unclear
- `performance_issue` — excessive duration, turns, tokens, output, retries,
  context growth
- `missing_context` — needed project/environment/file/session/prior-task
  context was absent
- `prompt_or_instruction_gap` — cake prompts, AGENTS.md, skills, or tool docs
  insufficient or ambiguous
- `instruction_following_issue` — agent ignored or partly followed
  user/developer/project/tool instructions
- `missing_tool_or_capability` — unavailable or poorly described tool, parser,
  permission path, or workflow would have materially helped
- `session_integrity_issue` — malformed JSONL, missing task boundaries,
  unsupported format, orphan calls, duplicate ids, incomplete final records

### Evidence Requirements

For every finding include:

- Record type and line number when available
- Task id and timestamp when available
- Tool name and `call_id` for tool-related findings
- Short excerpt of relevant user/assistant/tool text (smallest useful)
- Impact on the task
- Specific cake improvement recommendation

Do not paste large tool outputs. Quote the smallest useful excerpt.

### Findings report (always produced)

Return a concise report with:

1. **Executive Summary** — overall health, top three improvement opportunities
2. **Session Metadata** — session id, format version, model, working
   directory, cake version, tools, task count, duration, turns, token usage
3. **Findings** — severity, category, evidence, impact, recommendation
4. **Task Timeline** — request, outcome, duration, turns, tools, errors, context
5. **Tool Call Analysis** — counts by tool, failures, repeats, large outputs,
   permission denials, missing/orphan outputs
6. **Prompt and Context Analysis** — sufficiency of context, AGENTS.md,
   skills, tools, environment
7. **Performance Notes** — slow tasks, high tokens, excessive turns,
   truncation or timeout risks
8. **Recommended Cake Improvements** — prioritized, implementation-ready

### Quality scoring appendix (only when the request is evaluation/scoring)

Append these sections to the report:

1. **Task Completion** — Completed / Partially completed / Failed, with
   evidence (explicit confirmation, summary of changes, mid-sentence
   truncation, unresolved clarifying questions).
2. **Quality Assessment** — correctness, completeness, efficiency, code
   quality.
3. **Improvement Areas** (prioritized by impact: high / medium / low):
   - **System prompt enhancements** — missing reasoning patterns, scenario
     handling, instruction hierarchy, examples.
   - **Tool descriptions** — when/how to use, examples, pitfalls, parameter
     descriptions, documented limitations.
   - **New tools** — missing capabilities, recurring patterns, compound
     operations.
   - **AGENTS.md improvements** — missing build/test/lint commands, patterns,
     troubleshooting, project context.
   - **Error handling and recovery** — failure handling, self-correction,
     context preservation.

### Persistence

Default to returning the report in the response. Only create a project file
when the user explicitly asks for a persistent report; prefer a named
artifact such as `session-analysis.md`. Do not invent persistent issue
trackers that are not already part of the repo.

## Essential Commands

These four cover the start of nearly every analysis:

```bash
SESSION=~/.local/share/cake/sessions/{uuid}.jsonl

# 1. Validate header
head -1 "$SESSION" | jq '.'

# 2. Count records by type (shape of the session)
jq -r '.type' "$SESSION" | sort | uniq -c

# 3. Inspect every task outcome
jq 'select(.type == "task_complete") | {task_id, subtype, is_error, duration_ms, turn_count, tool_call_count, result, error, usage, permission_denials}' "$SESSION"

# 4. See how the session ended
tail -5 "$SESSION" | jq '.'
```

For the full cookbook — tool-call orphan detection, repeated-call signals,
log/telemetry correlation, conversation searches, hook event queries — see
[`reference/jq-recipes.md`](reference/jq-recipes.md).

Do not modify the session file during analysis.

## Checklist

Session analysis:

- [ ] Located and read the session
- [ ] First record is `session_meta` with `format_version: 4`
- [ ] Segmented by `task_start` / `task_complete`
- [ ] Identified the original task(s)
- [ ] Traced tool calls and results, correlated by `call_id`
- [ ] Reviewed reasoning (`content`, not `summary`)
- [ ] Reviewed `prompt_context` for AGENTS.md, skills, env, cwd, date
- [ ] Checked final response for completion

Issue identification:

- [ ] Tool selection, parameters, result interpretation
- [ ] Repeated/redundant calls
- [ ] Permission and sandbox issues
- [ ] Missing context or prompt gaps
- [ ] Reasoning flaws or stuck patterns
- [ ] Performance / token / turn anomalies
- [ ] Session integrity

Report:

- [ ] Findings include evidence and implementation-ready recommendations
- [ ] Recommendations prioritized by impact
- [ ] Persistent report file only if explicitly requested

## Worked Example

A short, anonymized illustration of the loop: read evidence → extract a
finding → write it up.

**Step 1 — count record types:**

```bash
$ jq -r '.type' "$SESSION" | sort | uniq -c
   1 session_meta
   2 task_start
   2 task_complete
  18 message
  11 reasoning
  14 function_call
  14 function_call_output
   6 prompt_context
```

Balanced calls and outputs (14/14) — no orphans. Two tasks, both completed.

**Step 2 — spot-check the tool calls:**

```bash
$ jq 'select(.type == "function_call") | {name, args: (.arguments[0:120])}' "$SESSION"
{ "name": "bash", "args": "{\"cmd\":\"cargo test --all\"}" }
{ "name": "bash", "args": "{\"cmd\":\"cargo test --all\"}" }
{ "name": "bash", "args": "{\"cmd\":\"cargo test --all\"}" }
...
```

Three identical `cargo test --all` calls in a row stands out.

**Step 3 — check what changed between them:**

```bash
$ jq 'select(.type == "function_call_output") | {call_id, tail: (.output[-200:])}' "$SESSION"
```

If the outputs are nearly identical and no edit happened between them,
this is a stuck pattern.

**Step 4 — write the finding:**

```markdown
**Severity**: Medium
**Category**: `repeated_tool_call`
**Evidence**: function_call records at lines 42, 51, 60 (call_ids
  fc_a1, fc_a2, fc_a3), all `bash` with identical `cargo test --all`,
  no intervening `edit` or `write`.
**Impact**: ~90 s wasted, +12k tokens of duplicated test output in
  context.
**Recommendation**: Tighten the system-prompt guidance on re-running
  full test suites without changes, or add a tool-result hint when the
  same command is invoked twice with no edits between.
```

This is the unit the report is built from. Multiply across categories.

## File Locations

| File                                        | Purpose                                                     |
| ------------------------------------------- | ----------------------------------------------------------- |
| `~/.local/share/cake/sessions/{uuid}.jsonl` | Session files (or `$CAKE_DATA_DIR/sessions/` if set)        |
| `~/.cache/cake/session-telemetry/{uuid}.ndjson` | Per-session telemetry (timings, retries, tool durations) |
| `~/.cache/cake/cake.YYYY-MM-DD.log`         | Daily logs (or `$CAKE_DATA_DIR/cake.YYYY-MM-DD.log` if set) |

# Current Issues - Acai CLI Evaluation

## Session: 2026-03-24 - Documentation Update Task

### Task Summary
The acai CLI was asked to find and remove outdated `--providers` flag references from README.md and documentation files.

### What Worked Well

1. **Systematic Search Approach**: The CLI correctly:
   - Searched for all markdown files in the project
   - Used grep to find `--providers` references
   - Verified the flag doesn't exist in the current CLI implementation by reading `src/main.rs`
   - Found the two outdated references in README.md

2. **Thoroughness**: The CLI:
   - Checked multiple file types (`.md`, `.rs`, `.py`)
   - Verified against the actual source code
   - Confirmed other docs were already correct
   - Made the edits successfully

3. **Tool Usage**: 27 tool calls were made, showing a methodical approach:
   - Bash commands for searching (`grep`, `find`)
   - Read tool for examining files
   - Edit tool for making changes

4. **Verification**: After making changes, the CLI ran verification searches to confirm all references were removed.

### Issues Identified

#### 1. Redundant Tool Calls (Medium Priority)
**Pattern**: The CLI made multiple overlapping grep searches:
- First searched for `--providers` in `.md` files
- Then searched for `--providers` in `.py` files (unnecessary for this task)
- Then searched for `--providers` in `.rs` files
- Then searched for "providers" with various patterns

**Impact**: Increased token usage and execution time without adding value.

**Recommendation**: The system prompt could include guidance on:
- Planning searches before executing them
- Consolidating related searches into single commands
- Avoiding unnecessary file type searches when the task scope is clear

#### 2. Excessive File Reading (Low Priority)
**Pattern**: The CLI read entire files (README.md, ARCHITECTURE.md, CHANGELOG.md) when targeted grep output already showed the relevant lines.

**Impact**: Larger context window consumption, though the model handled it well.

**Recommendation**: Consider adding guidance to use targeted reads (with line ranges) when the location is already known from grep output.

#### 3. No Clear Task Completion Signal (Low Priority)
**Pattern**: The CLI provided a summary at the end, which is good, but didn't explicitly indicate "task complete" status.

**Impact**: Minor - the summary was clear enough.

**Recommendation**: Consider adding a standardized task completion format.

### No Critical Issues Found

The CLI successfully completed the task. The issues above are optimization opportunities, not blockers.

---

## Recommendations for General Improvements

### System Prompt Enhancements

1. **Search Efficiency Guidance**: Add a section on efficient search patterns:
   ```
   When searching for patterns in codebases:
   - Plan your search strategy before executing
   - Combine related searches when possible
   - Use targeted line ranges when location is known
   - Avoid redundant searches across file types when scope is clear
   ```

2. **Task Completion Checklist**: Add guidance for verifying task completion:
   - Re-run verification commands after edits
   - Provide explicit "Task completed" status
   - Summarize changes made

### Tool Description Improvements

1. **Read Tool**: Consider adding guidance about using `startLine` and `lineCount` parameters when the relevant section is already known from grep output.

2. **Bash Tool**: Consider adding examples of efficient search patterns in the tool description.

### AGENTS.md Improvements

1. **Documentation Update Workflow**: Consider adding a section on how to handle documentation updates:
   - Check for outdated references
   - Verify against current implementation
   - Update all affected files
   - Verify changes

---

## Metrics

- **Total tool calls**: 27 (13 function calls + 13 outputs + 1 session_start)
- **Total messages**: 9 (1 user + 8 assistant)
- **Execution time**: ~140 seconds
- **Task outcome**: Success

---

## Session: 2026-03-28 - Hooks Research Task

### Task Summary
The acai CLI was asked to research hooks implementations in other coding agents (Claude Code, Codex, Cursor) and update hooks.md with findings relevant to acai's implementation.

### What Worked Well

1. **Successful Task Completion**: The CLI:
   - Fetched documentation from all four URLs
   - Extracted relevant information about hooks implementations
   - Produced a comprehensive 645-line summary document
   - Included comparison tables, code examples, and implementation recommendations
   - Correctly updated ./hooks.md

2. **Pagination Strategy**: For large documents, the CLI used `head` and `tail` to paginate through content:
   ```bash
   curl -sL "URL" | head -200
   curl -sL "URL" | tail -n +200 | head -400
   curl -sL "URL" | tail -n +600 | head -400
   ```
   This approach respects output size limits while capturing full content.

3. **Verification**: After writing the file, the CLI read it back to verify the output was complete.

4. **Comprehensive Output**: The final document includes:
   - Executive summary
   - Comparison matrices
   - Detailed source analysis for each system
   - Implementation recommendations specific to acai
   - Rust code examples
   - Implementation phases

### Issues Identified

#### 1. No Explicit acai Architecture Context (Low Priority)
**Pattern**: The prompt asked to "keep acai's implementation in context" but the output was a general comparison.

**Impact**: The recommendations section references acai but doesn't explicitly connect findings to acai's existing architecture (sandboxing, session management, etc.).

**Recommendation**: The CLI could have read ARCHITECTURE.md or relevant source files to provide more context-specific recommendations.

#### 2. Many Sequential Tool Calls (Low Priority)
**Pattern**: 12 curl calls were made sequentially (not batched) to fetch documentation in chunks.

**Impact**: Longer execution time. The calls could have been batched since they were independent.

**Recommendation**: When making multiple independent HTTP requests, batch them in a single tool call block.

### Metrics

- **Total tool calls**: 40 (17 function calls + 17 outputs + 5 messages + 1 session_start)
- **Bash calls**: 13 (12 curl + 1 verification)
- **Read calls**: 3
- **Write calls**: 1
- **Execution time**: ~97.6 seconds
- **Task outcome**: Success

### No Critical Issues Found

The CLI successfully completed the research task with comprehensive output. The issues above are optimization opportunities.

**Note**: The CLI correctly used `curl` via Bash for fetching web content because the acai CLI does not have a WebFetch tool. This is the expected behavior given the CLI's available toolset.

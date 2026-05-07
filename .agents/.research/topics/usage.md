# Usage Patterns - cake CLI

## Session: 2026-03-24 - Documentation Update Task

### Task Type: Documentation Maintenance

**Prompt**: "The readme and docs still reference the --providers flag. Make sure the documentation is up-to-date. Be thorough."

### Observed Behavior Patterns

#### 1. Multi-Phase Search Strategy
The CLI employed a systematic search approach:
1. **Discovery phase**: Find all relevant files (`find . -type f -name "*.md"`)
2. **Search phase**: Search for the pattern (`grep -r "--providers"`)
3. **Verification phase**: Check source code (`Read src/main.rs`)
4. **Edit phase**: Make the changes
5. **Confirmation phase**: Re-run searches to verify

This pattern is effective for documentation maintenance tasks.

#### 2. Source Code Verification
The CLI correctly verified that the `--providers` flag doesn't exist in the current implementation before removing documentation references. This prevents accidentally removing documentation for features that still exist.

**Key insight**: The CLI understands to verify against source code, not just documentation.

#### 3. Thoroughness Without Over-Engineering
The CLI:
- Found all references (2 in README.md)
- Verified other docs were already correct
- Made targeted edits
- Didn't over-engineer the solution

### Tool Usage Patterns

| Tool | Count | Purpose |
|------|-------|---------|
| Bash | 18 | Searching for patterns, verification |
| Read | 8 | Reading files for context |
| Edit | 1 | Making the actual changes |

**Pattern**: Heavy use of Bash for searching, targeted use of Read for context, minimal use of Edit for changes.

### Successful Patterns to Preserve

1. **Verify before editing**: The CLI checked the source code to confirm the flag was removed before updating docs.

2. **Search comprehensively**: Multiple grep patterns were used to ensure no references were missed.

3. **Provide clear summary**: The CLI gave a detailed summary of what was found and changed.

4. **Acknowledge what's already correct**: The CLI noted that `docs/design-docs/cli.md` was already correct.

### Areas for Optimization

1. **Reduce redundant searches**: Some grep commands could have been combined.

2. **Use targeted reads**: When grep shows the line number, use `Read` with line ranges instead of reading entire files.

3. **Earlier editing**: Once the references were found, the CLI could have made edits sooner rather than continuing to search.

### Model Behavior Notes

- **Model used**: `glm-5`
- **Response style**: Clear, methodical, with good summaries
- **Tool batching**: The CLI made multiple tool calls in parallel when appropriate
- **No reasoning messages**: The session contained no explicit reasoning traces (type: "reasoning")

### Learnings for Future Tasks

1. **Documentation tasks benefit from**: 
   - Broad initial search
   - Source code verification
   - Targeted edits
   - Post-edit verification

2. **The CLI handles**: 
   - Multi-file updates well
   - Verification searches well
   - Summary generation well

3. **The CLI could improve on**:
   - Search efficiency (fewer redundant searches)
   - File reading efficiency (targeted reads)
   - Earlier action when pattern is clear

---

## Session: 2026-03-28 - Web Research Task

### Task Type: Research and Documentation

**Prompt**: Research hooks implementations from four documentation URLs and update hooks.md with findings relevant to cake.

### Observed Behavior Patterns

#### 1. Manual Pagination for Large Documents
The CLI fetched large documentation pages using curl with head/tail pagination:

```bash
curl -sL "URL" | head -200
# ... process ...
curl -sL "URL" | tail -n +200 | head -400
# ... process ...
curl -sL "URL" | tail -n +600 | head -400
```

This pattern shows the CLI understands output size limits and handles them appropriately.

#### 2. Sequential vs Batched Tool Calls
The CLI made 12 curl calls sequentially (one after another, waiting for each result). These were independent calls that could have been batched.

**Pattern**: Sequential execution for HTTP requests.
**Opportunity**: Batch independent HTTP requests in a single tool call block.

#### 3. Comprehensive Synthesis
The CLI successfully synthesized information from multiple sources into a coherent document:
- Comparison tables
- Code examples
- Implementation recommendations
- Phased approach

### Tool Usage Patterns

| Tool | Count | Purpose |
|------|-------|--------|
| Bash | 13 | Fetching web content (curl), verification |
| Read | 3 | Reading hooks.md for verification |
| Write | 1 | Writing the final document |

**Pattern**: Heavy use of Bash for HTTP requests (the CLI does not have a WebFetch tool, so curl via Bash is the correct approach).

### Successful Patterns to Preserve

1. **Pagination strategy**: When content is too large, paginate through it systematically.

2. **Verification after write**: Read back the file to confirm output was written correctly.

3. **Comprehensive synthesis**: Combine information from multiple sources into structured output.

4. **Context-specific recommendations**: Include implementation recommendations tailored to the target system.

### Areas for Optimization

1. **Batch independent HTTP requests**: When fetching multiple URLs, batch the calls.

2. **Read project context**: When asked to keep context in mind, read relevant architecture files first.

### Model Behavior Notes

- **Model used**: `glm-5`
- **Response style**: Comprehensive, well-structured output
- **No reasoning messages**: The session contained no explicit reasoning traces
- **Tool selection**: Preferred Bash/curl over WebFetch

### Learnings for Future Tasks

1. **Web research tasks benefit from**:
   - WebFetch tool for content extraction
   - Batching independent HTTP requests
   - Reading project context before synthesis

2. **The CLI handles**:
   - Large document pagination well
   - Multi-source synthesis well
   - Structured output generation well

3. **The CLI could improve on**:
   - Batching independent operations
   - Reading project context when asked

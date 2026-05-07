# Task Index

This index summarizes the task files in this directory. Use it as the manually maintained work queue, then open the linked task file for the full problem statement, files involved, and acceptance notes.

## Status Summary

- Completed: 27
- Pending: 73
- Tracking: 2
- Open: 1
- Blocked: 12

## How to Choose Next Work

1. Prefer the lowest priority number first: P0, then P1, P2, P3.
2. Skip tasks marked `Completed`, `Blocked`, or `Tracking`.
3. Treat parent tracker tasks as planning references, not direct implementation tasks.
4. Check the `Depends on` column before starting. If a dependency is incomplete, do that dependency first.

## Next Ready Queue

These are the highest-priority tasks currently available from the task metadata:

| Task | Title | Status | Priority | Depends on |
| --- | --- | --- | --- | --- |
| [046](046.md) | Persist Activated Skills as Structured Session Metadata | Pending | P1 | - |

With P0 cleared, [048](048.md) is an important P1 unlock because multiple larger refactors depend on it.

## Parent Trackers

| Task | Title | Status | Priority | Action |
| --- | --- | --- | --- | --- |
| [047](047.md) | Plumb ToolContext Through Tool Execution (Parent) | Tracking | P1 | Work [047a](047a.md), then [047b](047b.md), then [047c](047c.md). |
| [060](060.md) | Make Hooks Observable in Typed Session Flow (Parent) | Tracking | P1 | Work [060a](060a.md), [060b](060b.md), and [060c](060c.md). |

## All Tasks

| Task | Title | Status | Priority | Depends on |
| --- | --- | --- | --- | --- |
| [001](001.md) | Fix README Default Model Mismatch | Completed | - | - |
| [002](002.md) | Fix Stdin 100ms Timeout Silently Dropping Slow Input | Completed | - | - |
| [003](003.md) | Fix Session Save Requiring Valid UUID at Runtime | Completed | - | - |
| [004](004.md) | Fix mtime-Based Newest Session Resolution Fragility | Blocked | - | - |
| [005](005.md) | Add CLI Introspection Subcommands | Blocked | - | - |
| [006](006.md) | Add Lightweight Spinner/Status Line on stderr by Default | Blocked | - | - |
| [007](007.md) | Support --prompt-file for Reading Prompts from Files | Blocked | - | - |
| [008](008.md) | Add --output-file Flag with Metadata | Blocked | - | - |
| [009](009.md) | Add Session Browsing Commands | Blocked | - | - |
| [010](010.md) | Add --dry-run Flag for Cost Estimation | Blocked | - | - |
| [011](011.md) | Add --add-file for Injecting Single Files into Context | Blocked | - | - |
| [012](012.md) | Add Template Prompts from .cake/prompts/ | Blocked | - | - |
| [013](013.md) | Document Meaningful Exit Codes | Completed | - | - |
| [014](014.md) | Add Task Chaining with --then Flag | Blocked | - | - |
| [015](015.md) | Consolidate Per-Function unwrap_used Allow to Module-Level in Tests | Blocked | - | - |
| [016](016.md) | Refactor send() and execute_bash() into Smaller Functions | Blocked | - | - |
| [017](017.md) | Extract Tool Execution Helpers from Agent send() | Completed | - | - |
| [018](018.md) | Fix Concurrent Skill Activation Race | Pending | - | - |
| [019](019.md) | Remove Write-Only prior_skill_activations State | Completed | - | - |
| [020](020.md) | Simplify with_activated_skills() | Completed | - | - |
| [021](021.md) | Deduplicate Session Streaming Helpers | Completed | - | - |
| [022](022.md) | Avoid Duplicate User Message Construction | Completed | - | - |
| [023](023.md) | Reduce Retry Logic Duplication in complete_turn() | Completed | - | - |
| [024](024.md) | Revisit Retryable HTTP Status Codes | Completed | - | - |
| [025](025.md) | Centralize Agent Test Construction | Completed | - | - |
| [026](026.md) | Fix Double INIT and Stream Ordering for Continued Sessions | Pending | - | - |
| [027](027.md) | Clean Up Pre-Existing Dead Code | Pending | - | - |
| [028](028.md) | Warn (Don't Silently Swallow) StreamRecord Serialization Failures | Pending | - | - |
| [029](029.md) | Reduce SessionRecord / StreamRecord Boilerplate | Pending | - | - |
| [030](030.md) | Investigate Session-Level Hook Observability | Pending | - | - |
| [031](031.md) | Fix rm -rf Safety Checks for Multiple Targets | Completed | P0 | - |
| [032](032.md) | Stop Treating Literal TMPDIR as Safe rm Target | Completed | P0 | - |
| [033](033.md) | Remove Bypass-Prone Bash Data Context Skips | Completed | P0 | - |
| [034](034.md) | Clarify Bash Safety Boundary | Completed | P0 | - |
| [035](035.md) | Retire Bash Shell Parser Expansion | Completed | P0 | 033 (already complete), 034 |
| [036](036.md) | Preserve Edit Tool Line Endings Without Rewriting Bytes | Completed | P0 | - |
| [037](037.md) | Fail on Missing Function Call Fields in Responses API | Completed | P0 | - |
| [038](038.md) | Fail on Missing Chat Completion Response IDs | Completed | P0 | - |
| [039](039.md) | Escape Home Paths in macOS Sandbox Profiles | Completed | P0 | - |
| [040](040.md) | Introduce a Seatbelt Profile Builder | Completed | P0 | - |
| [041](041.md) | Replace Sandbox Violation Detection by Output Substrings | Completed | P0 | - |
| [042](042.md) | Stop Silently Truncating Responses History | Completed | P0 | - |
| [043](043.md) | Report Unknown Responses Output Types | Completed | P0 | - |
| [044](044.md) | Add Clap Exclusivity for Session Mode Flags | Completed | P0 | - |
| [045](045.md) | Fix HTTP Client Build Error Handling | Completed | P1 | - |
| [046](046.md) | Persist Activated Skills as Structured Session Metadata | Pending | P1 | - |
| [047](047.md) | Plumb ToolContext Through Tool Execution (Parent) | Tracking | P1 | 048 (e2e test as safety net) |
| [047a](047a.md) | Introduce ToolContext (no plumbing yet) | Pending | P1 | 048 (e2e test as safety net) |
| [047b](047b.md) | Plumb &ToolContext Through Tool Execution | Pending | P1 | 047a (ToolContext exists), 051 (Tool abstraction so the trait can carry the context) |
| [047c](047c.md) | Delete the Tool OnceLocks | Pending | P1 | 047b (no callers reach into globals anymore) |
| [048](048.md) | Add End-to-End Agent Loop Test with Stub Backend | Pending | P1 | - |
| [049](049.md) | Split Agent Responsibilities | Pending | P1 | 048 (e2e test as safety net), 050 (Backend abstraction), 051 (Tool abstraction), 047 (ToolContext) |
| [050](050.md) | Add Backend Abstraction for API Providers | Pending | P1 | 048 (e2e test as safety net) |
| [051](051.md) | Add Tool Abstraction and Registry | Pending | P1 | 048 (e2e test as safety net) |
| [052](052.md) | Move Provider Quirks Behind Provider Strategy | Pending | P1 | 050 (Backend abstraction) |
| [053](053.md) | Model Session Mode as a Typed Run Mode | Pending | P1 | - |
| [054](054.md) | Refactor CodingAssistant Run Into Smaller Steps | Pending | P1 | - |
| [055](055.md) | Replace Boolean Task Completion API with Outcome Enum | Pending | P1 | - |
| [056](056.md) | Remove Redundant StreamRecord Success Booleans | Pending | P1 | - |
| [057](057.md) | Add Typed Reasoning Effort | Pending | P1 | - |
| [058](058.md) | Type Reasoning Content Kinds | Pending | P1 | - |
| [059](059.md) | Add Structured Output Sink for CLI Output | Pending | P1 | - |
| [060](060.md) | Make Hooks Observable in Typed Session Flow (Parent) | Tracking | P1 | - |
| [060a](060a.md) | Type Hook Matcher Sources | Pending | P1 | - |
| [060b](060b.md) | Model Hook Continue/Block as a Proper Enum | Pending | P1 | - |
| [060c](060c.md) | Implement or Remove suppress_output Hook Field | Pending | P1 | - |
| [061](061.md) | Consolidate Conversation Serialization Paths | Pending | P1 | - |
| [062](062.md) | Store Timestamps as DateTime Values Internally | Pending | P1 | - |
| [063](063.md) | Normalize Optional Fields After Session Deserialization | Pending | P1 | - |
| [064](064.md) | Refactor Chat build_messages State Handling | Pending | P2 | 050 (Backend abstraction) |
| [065](065.md) | Reduce Cloning in Agent Tool Loop | Pending | P2 | 048 (e2e test as safety net) |
| [066](066.md) | Encapsulate Agent Public Mutable Fields | Pending | P2 | 048 (e2e test as safety net) |
| [067](067.md) | Handle Activated Skills Mutex Poisoning Explicitly | Pending | P2 | - |
| [068](068.md) | Replace with_history Debug Assertion with Real Invariant | Pending | P2 | - |
| [069](069.md) | Rework Fork Session Storage Path | Pending | P2 | - |
| [070](070.md) | Simplify Resolved Model Configuration Naming | Pending | P2 | - |
| [071](071.md) | Replace looks_like_uuid with Parser-Based Validation | Pending | P2 | - |
| [072](072.md) | Add Size Limit or Streaming Plan for Stdin Input | Pending | P2 | - |
| [073](073.md) | Add Structured Prompt and Stdin Combination | Pending | P2 | - |
| [074](074.md) | Replace Hand-Rolled Binary Magic Detection | Pending | P2 | - |
| [075](075.md) | Check Whole File or Trust UTF-8 for Edit Binary Detection | Pending | P2 | - |
| [076](076.md) | Optimize Edit Application Allocation | Pending | P2 | - |
| [077](077.md) | Parse Only Needed Fields in Edit Argument Summaries | Pending | P2 | - |
| [078](078.md) | Delete Dead Diff Header Construction | Pending | P2 | - |
| [079](079.md) | Introduce RetryPolicy Configuration Type | Pending | P2 | - |
| [080](080.md) | Simplify Overloaded Retry Classification | Pending | P2 | - |
| [081](081.md) | Replace Retry Signal Body Substring Matching Where Possible | Pending | P2 | - |
| [082](082.md) | Make Stream JSON Conversion Live or Delete It | Pending | P2 | - |
| [083](083.md) | Fix serde_json to_value Error Policy | Pending | P2 | - |
| [084](084.md) | Use a Shared Duration Formatting Helper | Pending | P3 | - |
| [085](085.md) | Make Human-Readable Size Formatting Less Clever | Pending | P3 | - |
| [086](086.md) | Flatten format_api_error_body | Pending | P3 | - |
| [087](087.md) | Simplify Responses Reasoning Config Construction | Pending | P3 | - |
| [088](088.md) | Clean Up Dead Code Allows | Pending | P3 | - |
| [089](089.md) | Rationalize Clippy Allow Policy | Pending | P3 | Resolve in the same PR as 107 (missing_docs lint policy) for consistency. |
| [090](090.md) | Decide on Tokio Runtime Entry Style | Pending | P3 | - |
| [091](091.md) | Clean Up macOS Sandbox String Construction Style | Pending | P3 | 040 (Seatbelt profile builder) |
| [092](092.md) | Simplify Binary Ratio Threshold Constants | Pending | P3 | - |
| [093](093.md) | Rename with_text_progress to Avoid Builder Semantics | Pending | P3 | - |
| [094](094.md) | Simplify Test-Only Bash Sandbox Constructor | Pending | P3 | - |
| [095](095.md) | Fix Dead Code in is_allowed_rm_target | Pending | P3 | - |
| [096](096.md) | Make Git Branch Delete Detection Parser Safe | Pending | P3 | - |
| [097](097.md) | Revisit macOS Sandbox Probe Caching and Cleanup | Pending | P3 | - |
| [098](098.md) | Reorganize Type Modules | Pending | P3 | 049 (Split Agent), 050 (Backend abstraction), 051 (Tool abstraction), 058 (Typed reasoning content), 061 (Consolidate serialization), 062 (DateTime types), 063 (Optional field normalization). Do this last. |
| [099](099.md) | Remove Library-Style Doctests From Binary-Only Crate | Pending | P3 | - |
| [100](100.md) | Decide Linux Landlock Default Feature Policy | Pending | P3 | - |
| [101](101.md) | Review Module Size and Ownership Boundaries | Pending | P3 | 047 (ToolContext), 049 (Split Agent), 050 (Backend abstraction), 051 (Tool abstraction), 052 (Provider strategy), 053 (Typed SessionMode), 054 (Refactor run), 064 (build_messages refactor). Do this after behavior-oriented refactors land. |
| [102](102.md) | Validate Stream Hook Record Contract | Pending | P3 | 030 (existing - see notes), 059 (Output sink). Confirm not superseded by 059 once that lands. |
| [103](103.md) | Add Builder or Fixture Infrastructure for Agent Tests | Pending | P3 | 066 (Encapsulate Agent fields) |
| [104](104.md) | Add Structured Provider Header Configuration | Pending | P3 | 050 (Backend abstraction), 052 (Provider strategy) |
| [105](105.md) | Improve Unknown or Missing API Field Diagnostics | Pending | P3 | - |
| [106](106.md) | Audit Public Module Visibility in Tools | Pending | P3 | 051 (Tool abstraction) |
| [107](107.md) | Reconsider Missing Docs Lint Policy | Pending | P3 | Resolve in the same PR as 089 (clippy allow policy) for consistency. |
| [108](108.md) | Track Manual Review Findings Without Losing Original Numbers | Pending | P3 | - |
| [109](109.md) | Edit tool: CRLF replacement text double-encoding | Open | - | - |

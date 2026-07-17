# FFF Search Integration

Status: active
Created: 2026-05-21
Updated: 2026-05-21
Related tasks: -
Related plans: -
Confidence: medium

## Summary

`fff_search` is a plausible native backend for dedicated cake `Search` and `Find` tools. The strongest fit is to embed the Rust crate and keep a session-scoped `FilePicker` alive, rather than shelling out or relying on an MCP server. `FilePicker` provides background indexing, file watching, frecency-ranked fuzzy path search, and grep over indexed files.

The integration is not just adding two tool files. cake's tool executor currently receives `Arc<ToolContext>` only, while `fff` wants shared long-lived picker/frecency/query-tracker state. The likely design is to extend tool execution state with an `FffSearchState` owned by the agent session and shared by the new tools.

## Notes / Evidence

Sources:

- docs.rs `fff_search` latest, 0.8.1: https://docs.rs/fff-search/latest/fff_search/
- `FilePicker` API docs: https://docs.rs/fff-search/latest/fff_search/file_picker/struct.FilePicker.html
- `grep` module docs: https://docs.rs/fff-search/latest/fff_search/grep/index.html
- GitHub README and MCP notes: https://github.com/dmtrKovalenko/fff
- FFF MCP server source: https://github.com/dmtrKovalenko/fff/blob/main/crates/fff-mcp/src/server.rs

Relevant `fff_search` capabilities:

- `FilePicker::new_with_shared_state` initializes a picker in a shared handle and starts background indexing plus a filesystem watcher.
- `FilePicker::collect_files` is available for synchronous one-shot indexing when direct picker access is preferable.
- `FilePicker::fuzzy_search` searches indexed files with `FuzzySearchOptions` and `PaginationArgs`.
- `FilePicker::grep` searches indexed files with `GrepSearchOptions`.
- `FilePicker::multi_grep` supports OR-style multi-pattern content search.
- `SharedFilePicker`, `SharedFrecency`, and `SharedQueryTracker` wrap shared state behind locks.
- The crate has dependencies that may affect cake's binary size and build profile, including `git2`, `heed`/LMDB, `notify`, `rayon`, `regex`, and `memmap2`.

Relevant `fff` MCP behavior worth copying:

- `find_files` accepts `query`, optional `maxResults`, and optional cursor.
- `grep` accepts `query`, optional `maxResults`, optional cursor, and output mode.
- `multi_grep` accepts `patterns`, constraints, optional cursor, output mode, and context.
- Grep defaults to plain-text unless regex metacharacters are detected.
- The MCP prompt strongly encourages bare-identifier grep, broad constraints, and reading top results instead of repeated search variations.
- Output includes "Read this file" suggestions, frecency/git suffixes, cursors for pagination, and compact match lines.

Relevant cake architecture:

- Tool registration lives in `src/clients/tools/mod.rs`.
- Each tool has a `Tool` definition, executor function, summarizer, and registry entry.
- `Agent::new` installs `default_tool_registry()` and `ToolContext::from_current_process()`.
- Tool calls are executed concurrently in `Agent::run`.
- `ToolContext` currently contains path roots and sandbox-relevant directories, not long-lived tool state.
- Existing `Read` path validation accepts cwd, temp dirs, settings dirs, `--add-dir`, and skill dirs. New search tools should reuse the same path policy.

## Implications for cake

Recommended MVP:

1. Add a small `src/clients/tools/fff.rs` module behind a Cargo feature, probably `fff-search`.
2. Add a shared state handle such as:

   ```rust
   pub struct SearchState {
       picker: fff_search::SharedFilePicker,
       frecency: fff_search::SharedFrecency,
       query_tracker: fff_search::SharedQueryTracker,
       root: PathBuf,
   }
   ```

3. Extend the tool execution context from `Arc<ToolContext>` to a new context wrapper containing `ToolContext` plus optional `SearchState`, or add `search_state: Option<Arc<SearchState>>` to `ToolContext`.
4. Initialize `SearchState` per agent session from `ToolContext.cwd`; later, decide whether `--add-dir` roots should get separate pickers.
5. Add two model-facing tools first:

   - `Find`: fuzzy path search over repo-relative paths.
   - `Search`: content search over indexed files.

6. Consider `MultiSearch` only after the basic path/content tools are stable. It is valuable, but not required for a first integration.

Suggested `Find` schema:

```json
{
  "path": "optional base directory; defaults to cwd",
  "query": "short fuzzy path query",
  "limit": "default 20, max 100",
  "offset": "default 0"
}
```

Suggested `Search` schema:

```json
{
  "path": "optional base directory or file constraint; defaults to cwd",
  "query": "bare identifier, literal text, or simple regex",
  "mode": "auto | plain | regex | fuzzy",
  "case_sensitive": "optional bool",
  "context": "default 0",
  "limit": "default 20, max 100",
  "offset": "default 0"
}
```

Output should be compact and line-oriented:

```text
Search: query (20/143 matches)
src/foo.rs:42: matching line text
src/bar.rs:10: matching line text
[... more results: call Search with offset=20 ...]
```

Important design choices:

- Native crate integration is preferred over an MCP subprocess because cake already has native tool registration and sandbox/path validation.
- `fff` should not replace `Read`; search results should point to `Read` for larger context.
- Search roots must be validated against cake's existing readable roots before indexing or searching.
- Avoid exposing every `fff` feature immediately. Keep model-facing schema smaller than the backend.
- If frecency is enabled, place DBs under cake's data/cache directories rather than `fff` defaults.
- Background watchers should be disabled or made configurable if they cause CI flakiness or sandbox issues.
- Because tool calls can run concurrently, access to the picker must be lock-safe and search calls must not hold locks while doing unrelated formatting work if avoidable.

## Risks

- Binary size may grow materially due to `git2`, LMDB/heed, notify, rayon, and mmap-related dependencies. Run the binary-size audit before making `fff` a default dependency.
- `fff_search` docs are only partially documented, so source-reading and spike tests are necessary before relying on edge-case behavior.
- `FilePicker::new_with_shared_state` spawns background work. cake will need lifecycle handling for shutdown and tests.
- Existing tool execution signatures are simple function pointers. Adding shared search state may require either changing the executor signature or embedding optional search state into `ToolContext`.
- Path validation matters: `fff` must not index outside cake's allowed read roots.
- Model naming deserves care. `Find` and `Search` are familiar, but cake's prompt currently says there is no Glob/Grep/LS tool. The system prompt must be updated with clear usage rules.

## Follow-ups

- Create a task for a feature-gated spike that adds `fff-search` as an optional dependency and measures release binary size impact.
- Read `fff` source around `GrepSearchOptions`, output formatting, and path constraints before implementation.
- Decide whether `Find`/`Search` should be enabled by default or behind settings.
- Add snapshot tests for tool definitions and focused unit tests for argument parsing/output formatting.
- Add integration tests using temporary repositories and small fixture files.

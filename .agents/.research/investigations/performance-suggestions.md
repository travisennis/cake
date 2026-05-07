# Performance Suggestions

## 1. Full history sent every API turn
**File:** `src/clients/agent.rs:322–333`

Every `complete_turn()` serializes and sends the **entire** `self.history` to the API. As conversations grow long (many tool calls), this means exponentially increasing request sizes. There's no history windowing, summarization, or token-budget trimming.

## 2. `read_file` reads the entire file into memory
**File:** `src/clients/tools/read.rs:117`

`std::fs::read_to_string(path)` loads the whole file even when `start_line`/`end_line` request a tiny range. For large files (multi-MB), this is wasteful — it should use a `BufReader` to skip/stop at the requested lines.

## 3. `edit_tool` reads the file twice
**File:** `src/clients/tools/edit.rs:143–154`

`execute_edit` calls `std::fs::read(&path)` to get bytes for the binary check, then converts those same bytes to a `String`. Meanwhile, `validate_path_in_cwd` already calls `canonicalize()` which does a stat. The binary check could use the first 8KB of a memory-mapped or buffered read instead of loading everything.

## 4. `validate_path_in_cwd` called on hot path with repeated syscalls
**File:** `src/clients/tools/mod.rs:65–102`

Every tool invocation calls `std::env::current_dir()`, `path.canonicalize()`, and iterates `get_temp_directories()` — which itself calls `std::fs::canonicalize` on `/tmp`, `/var/folders`, and `$TMPDIR` each time. These could be computed once at startup and cached.

## 5. `get_temp_directories()` does 3 filesystem canonicalize calls per tool invocation
**File:** `src/clients/tools/mod.rs:105–126`

This function is called from `validate_path_in_cwd` which runs on every Read/Edit/Write. The canonical temp paths never change during the process lifetime — they should be cached in a `OnceLock`.

## 6. `get_history_without_system` clones the entire history
**File:** `src/clients/agent.rs:132–143`

`.cloned().collect()` does a deep clone of every `ConversationItem` (including potentially large tool outputs with 50KB+ strings). This is called when saving sessions. Consider returning a reference/slice instead.

## 7. Session save re-serializes redundant metadata per line
**File:** `src/config/session.rs:160–173`

Every `SessionLine` redundantly includes `session_id`, `timestamp`, `working_directory`, and `model` cloned for each message. For long sessions, this inflates file size and serialization cost. The session ID and working directory are already in the header.

## 8. `build_messages` and `build_input` allocate new strings via `.clone()` for every item
**Files:** `src/clients/chat_completions.rs:107–167`, `src/clients/types.rs:63–145`

Every conversation item's content, arguments, call_id, etc. are cloned when building API requests. For large tool outputs (50KB bash results), this doubles memory usage. Consider using references or `Cow<str>`.

## 9. Double serialization in `send_request`
**Files:** `src/clients/chat_completions.rs:55–65`, `src/clients/responses.rs:61–72`

The request is serialized once via `serde_json::to_string` for trace logging, then serialized again by `reqwest::Client::json()`. The trace-level serialization should be gated behind `tracing::enabled!(Level::TRACE)`.

## 10. No connection timeout on `reqwest::Client`
**File:** `src/clients/agent.rs:79`

`reqwest::Client::new()` uses default timeouts. A slow/hanging API server could block indefinitely. Set explicit connect/read timeouts via `reqwest::Client::builder()`.

---

## Priority

**Highest-impact fixes** would be #1 (conversation growth), #4/#5 (caching path validation), and #2 (buffered file reading). The others are worth addressing but won't be felt until conversations or files get large.


## TODO
- [ ] #1 — Full history sent every API turn (history windowing/trimming)
- [X] #2 — `read_file` reads entire file into memory (use BufReader)
- [X] #3 — `edit_tool` reads file twice (reuse bytes from first read)
- [X] #4 — `validate_path_in_cwd` repeated syscalls (cache at startup)
- [X] #5 — `get_temp_directories()` repeated canonicalize calls (use OnceLock)
- [x] #6 — `get_history_without_system` clones entire history (use references)
- [ ] #7 — Session save redundant metadata per line (deduplicate)
- [X] #8 — `build_messages`/`build_input` clone strings (use Cow<str>)
- [X] #9 — Double serialization in `send_request` (gate behind trace check)
- [X] #10 — No connection timeout on reqwest::Client (set explicit timeouts)

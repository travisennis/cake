# Property Testing Ideas for acai

## Overview

Property testing (via [`proptest`](https://crates.io/crates/proptest)) verifies that invariants hold for *all* inputs by generating random test cases and shrinking failures to minimal reproductions. This document outlines four areas where property testing would add value in acai.

### Setup

Add to `Cargo.toml`:

```toml
[dev-dependencies]
proptest = "1"
```

---

## 1. `truncate_output` — bash tool output truncation

**Location:** `src/clients/tools/bash.rs`, line 395

When command output exceeds `BASH_OUTPUT_MAX_BYTES`, this function builds a head/tail preview. It uses `floor_char_boundary` and `ceil_char_boundary` to avoid splitting multi-byte UTF-8 characters.

**Properties to verify:**

- **Panic-freedom on arbitrary UTF-8.** The char-boundary logic is correct in theory, but fuzzing with diverse Unicode strings (emoji, CJK, combining characters) ensures it holds in practice.
- **Output is always bounded.** The returned string should never exceed `BASH_OUTPUT_MAX_BYTES` plus a fixed metadata overhead.
- **Metadata always present.** The exit code and elapsed time must appear in every result, regardless of the input content.

**Example:**

```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn truncate_output_never_panics(
        output in "\\PC{0,200000}",
        exit_code in any::<i32>(),
        elapsed_ms in any::<u128>(),
    ) {
        let _ = truncate_output(&output, exit_code, elapsed_ms);
    }

    #[test]
    fn truncate_output_contains_metadata(
        output in ".{0,100}",
        exit_code in 0i32..256,
        elapsed_ms in 0u128..100_000,
    ) {
        let result = truncate_output(&output, exit_code, elapsed_ms);
        prop_assert!(result.contains(&format!("exit code: {exit_code}")));
    }
}
```

---

## 2. `truncate_display` — general display truncation

**Location:** `src/clients/tools/mod.rs`, line 177

This helper truncates a string to a max length for UI/log display:

```rust
fn truncate_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}
```

**Known bug:** `&s[..max]` indexes by *byte* offset. If `max` lands inside a multi-byte UTF-8 character (e.g., `"café"` where `é` is 2 bytes), this panics at runtime. A property test catches this immediately.

**Properties to verify:**

- **Panic-freedom.** Any combination of string content and `max` value must not panic.
- **Length bound.** The returned string's char count should be ≤ `max` + 3 (for the `...` suffix).
- **Short strings unchanged.** If `s.len() <= max`, the output equals the input exactly.

**Example:**

```rust
proptest! {
    #[test]
    fn truncate_display_never_panics(s in "\\PC{0,500}", max in 0usize..200) {
        // This WILL find the bug: slicing at a non-char-boundary panics
        let _ = truncate_display(&s, max);
    }
}
```

**Suggested fix** (after the test confirms the bug):

```rust
fn truncate_display(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max);
        format!("{}...", &s[..end])
    }
}
```

---

## 3. Tool argument parsing — fuzz for graceful errors

**Locations:**

- `src/clients/tools/bash.rs` — `BashExecutionArgs::from_json`
- `src/clients/tools/read.rs` — `execute_read` (parses `ReadArgs`)
- `src/clients/tools/edit.rs` — `execute_edit` (parses `EditArgs`)
- `src/clients/tools/write.rs` — `execute_write` (parses `WriteArgs`)

All of these take a raw JSON string from the model and parse it via `serde_json::from_str`. Since the model can produce arbitrary strings, these must never panic.

**Property to verify:**

- **Any string input returns `Ok` or `Err`, never panics.** This covers malformed JSON, missing fields, wrong types, empty strings, and binary-like content.

**Example:**

```rust
proptest! {
    #[test]
    fn read_args_parsing_never_panics(input in "\\PC{0,1000}") {
        let _ = execute_read(&input);
    }

    #[test]
    fn bash_args_parsing_never_panics(input in "\\PC{0,1000}") {
        let _ = BashExecutionArgs::from_json(&input);
    }

    #[test]
    fn edit_args_parsing_never_panics(input in "\\PC{0,1000}") {
        // Only test parsing, not execution (which touches the filesystem)
        let _ = serde_json::from_str::<EditArgs>(&input);
    }
}
```

**Note:** For tools that have side effects (file I/O, command execution), test only the *parsing* layer, not the full `execute_*` function. This keeps the tests fast, safe, and focused on the argument handling boundary.

---

## 4. Sandbox path validation — `validate_path_in_cwd` and `SandboxConfig`

**Locations:**

- `src/clients/tools/mod.rs`, line 65 — `validate_path_in_cwd`
- `src/clients/tools/sandbox/mod.rs`, line 44 — `SandboxConfig::build`
- `src/clients/tools/sandbox/mod.rs`, line 203 — `deduplicated_with_canonical`

These functions decide *which paths are permitted* for agent operations. `validate_path_in_cwd` checks whether a path falls within the cwd, temp directories, or `--add-dir` directories. `SandboxConfig::build` constructs the allow-lists passed to the OS-level sandbox. None of these actually enforce the sandbox or modify the filesystem — they build data structures and compare path prefixes. That makes them safe to fuzz.

**Why this is safe:** The functions under test only call `canonicalize()` (read-only) and `starts_with()` (pure comparison). They don't write, delete, or execute anything. The risk of "altering the system" comes from the `SandboxStrategy::apply` layer, which we intentionally skip.

**Properties to verify:**

- **Panic-freedom on adversarial paths.** Paths with `..`, null bytes, extremely long segments, symlink loops, and unicode should produce `Ok` or `Err`, never panic.
- **cwd is always allowed.** Any path that is a child of the current working directory must return `Ok`.
- **Paths outside all allowed directories are rejected.** A path not under cwd, temp, or additional dirs must return `Err`.
- **`deduplicated_with_canonical` preserves all input paths.** Every input path should appear in the output (dedup only removes duplicates, not unique entries).
- **`deduplicated_with_canonical` has no duplicates.** The output should never contain the same `PathBuf` twice.

**Example:**

```rust
use proptest::prelude::*;
use std::path::PathBuf;
use tempfile::TempDir;

proptest! {
    #[test]
    fn validate_path_rejects_outside_cwd(
        // Generate path segments that won't be under cwd
        segments in prop::collection::vec("[a-z]{1,20}", 1..5),
    ) {
        let fake_path = format!("/nonexistent/{}", segments.join("/"));
        let result = validate_path_in_cwd(&fake_path);
        // Should be Err (path doesn't exist or is outside cwd), never panic
        prop_assert!(result.is_err());
    }

    #[test]
    fn validate_path_allows_cwd_children(
        filename in "[a-zA-Z0-9_]{1,30}\\.txt",
    ) {
        // Create a real file under a temp dir, then set cwd to that dir
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join(&filename);
        std::fs::write(&file_path, "test").unwrap();

        // Note: validate_path_in_cwd uses std::env::current_dir(),
        // so this test would need to either:
        // (a) refactor the function to accept cwd as a parameter, or
        // (b) use a helper that checks starts_with directly
        // Option (a) is recommended — it also makes the function more testable.
    }

    #[test]
    fn deduplicated_preserves_unique_paths(
        paths in prop::collection::vec(
            prop::string::string_regex("[a-z/]{1,50}").unwrap().prop_map(PathBuf::from),
            0..20,
        ),
    ) {
        let result = deduplicated_with_canonical(&paths);
        // Every input path must appear in the output
        for p in &paths {
            prop_assert!(result.contains(p));
        }
        // No duplicates
        let mut seen = std::collections::HashSet::new();
        for p in &result {
            prop_assert!(seen.insert(p.clone()), "Duplicate found: {:?}", p);
        }
    }
}
```

**Refactoring suggestion:** `validate_path_in_cwd` currently calls `std::env::current_dir()` internally, which makes it hard to test without changing global process state. Refactoring it to accept `cwd: &Path` as a parameter would make it purely functional and trivial to property-test with arbitrary directory structures via `tempfile::TempDir`.

---

## 5. Serde roundtrips — config and message types

**Why test this:** Roundtrip tests (`T → serialize → deserialize → T`) verify that custom types survive the serialization boundary without silent data loss. This is most valuable when types use non-trivial serde attributes like `#[serde(default)]`, `#[serde(rename)]`, `#[serde(flatten)]`, or custom deserializers.

**When it's worth doing:**

- Types stored to disk (config files, session state)
- Types sent over the wire (API request/response bodies)
- Types with `Option` fields where `None` vs. missing-key semantics matter

**When it's not worth doing:**

- Simple structs with only primitive fields and `#[derive(Serialize, Deserialize)]`
- Types that are only ever serialized *or* deserialized, never both

**Example (if applicable):**

```rust
use proptest::prelude::*;

// Requires implementing or deriving Arbitrary for your config type
proptest! {
    #[test]
    fn config_roundtrip(config in arb_model_config()) {
        let json = serde_json::to_string(&config).unwrap();
        let decoded: ModelConfig = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(config, decoded);
    }
}
```

**Assessment for acai:** The config types in `src/config/` are relatively straightforward. Roundtrip testing would provide low-to-moderate value here unless the serde annotations become more complex. The other three areas above are higher priority.

---

## Summary

| Area | Priority | Why |
|---|---|---|
| `truncate_display` | **High** | Has a known byte-slicing bug that property testing catches immediately |
| `truncate_output` | **High** | Complex string manipulation with char-boundary logic; fuzzing builds confidence |
| Sandbox path validation | **Medium** | Security-critical allow/deny logic; safe to fuzz since it only reads and compares paths |
| Tool argument parsing | **Medium** | Boundary between untrusted model output and internal logic; should never panic |
| Serde roundtrips | **Low** | Config types are simple today; revisit if serde annotations grow more complex |

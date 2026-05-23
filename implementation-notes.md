# Implementation Notes - Task 169: Extract Bash Safety Check Rules into a Checks Module

## Overview

Extract the individual `check_*` rule functions, their private helpers, and their tests from `src/clients/tools/bash_safety/mod.rs` into a new `src/clients/tools/bash_safety/checks.rs` submodule, following the same module-splitting pattern established by the earlier `parse.rs` extraction.

## Design Decisions

### 1. Tests move to `checks.rs`

The task spec says to keep `mod.rs` focused on orchestration. Moving tests alongside the checks keeps related code together and is consistent with how the existing `parse.rs` module has no tests (all safety tests test the check functions through the public entry point). The tests in `checks.rs` call `super::validate_command_safety()` (the public entry point in `mod.rs`) and test individual helpers like `is_allowed_rm_target` directly.

**Alternative considered:** Keeping tests in `mod.rs` --- rejected because it would leave `mod.rs` still heavy with test infrastructure, defeating the purpose of the extraction.

### 2. `blocked()` stays in `mod.rs`

Per the task spec ("The `blocked()` helper or equivalent shared result constructor"). Child modules (`checks.rs`, `parse.rs`) access it via `super::blocked()`. This works because Rust's private visibility allows descendant modules to access their parent's private items.

### 3. Check functions use `pub(super)` visibility

Following the same pattern as `parse.rs` functions, the check functions are `pub(super)` so `mod.rs` (their parent module) can call them. Helper functions (`has_unsafe_message_flag`, `is_allowed_rm_target`) remain private within `checks.rs`.

### 4. No behavior changes

The extraction is purely structural. No rule logic, error messages, or ordering changes were made. File-level section comments were updated to reflect the new module structure.

## Deviations

None. The implementation follows the task spec exactly.

## Open Questions

None.

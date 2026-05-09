# Clippy Stricter Configuration (External Sources)

Status: active
Created: 2026-05-09
Updated: 2026-05-09
Related tasks: 110
Related plans: -
Confidence: high
Sources:
  - https://emschwartz.me/your-clippy-config-should-be-stricter/ (Evan Schwartz, 2026-04-30)
  - https://billylevin.dev/posts/clippy-config/ (Billy Levin, 2026-04-30)

## Summary

Two articles advocate for stricter Clippy configurations than the default, especially in the age of coding agents. They disagree on the approach: Schwartz recommends selective opt-in of ~40 specific lints; Levin recommends enabling the `pedantic` and `restriction` categories wholesale and selectively allowing what you disagree with. The project already follows Levin's approach for `pedantic` and `nursery` but does not enable `restriction`, which is where most of Schwartz's recommended lints live.

## Notes / Evidence

### Schwartz's Approach: Selective Opt-In

Schwartz's motivating bug was a UTF-8 string slicing panic that silently killed a Tokio worker thread in production. He groups recommended lints into categories:

**Don't Panic**: `string_slice`, `indexing_slicing`, `unwrap_used`, `panic`, `todo`, `unimplemented`, `unreachable`, `get_unwrap`, `unwrap_in_result`, `unchecked_time_subtraction`, `panic_in_result_fn`. Judgement calls: `expect_used`, `arithmetic_side_effects` (~15% useful, 85% noise).

**Don't Fail Silently**: `let_underscore_future`, `let_underscore_must_use`, `unused_result_ok`, `map_err_ignore`, `assertions_on_result_states`.

**Don't Do Bad Async Stuff**: `await_holding_lock`, `await_holding_refcell_ref`, `if_let_mutex` (pre-2024 edition only), `large_futures`.

**Don't Do Unsafe Things with Memory**: `mem_forget`, `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block`, `unnecessary_safety_doc`, `unnecessary_safety_comment`.

**Don't Do Potentially Incorrect Things with Numbers**: `float_cmp`, `float_cmp_const`, `lossy_float_literal`, `cast_sign_loss`, `invalid_upcast_comparisons`. Judgement calls: `cast_possible_wrap`, `cast_precision_loss`, `cast_possible_truncation`.

**Don't Do Bad Things That are Easy to Avoid**: `rc_mutex`, `debug_assert_with_mut_call`, `iter_not_returning_iterator`, `expl_impl_clone_on_copy`, `infallible_try_from`, `dbg_macro`.

**Don't `allow` Your Way Around**: `allow_attributes`, `allow_attributes_without_reason` -- every suppression must be `#[expect(lint, reason = "...")]`.

### Levin's Approach: Category Enablement + Selective Allow

Levin argues:

1. Enable `pedantic` and `restriction` categories (and optionally `nursery`)
2. Go through every warning, decide whether to: allow case-by-case with `#[expect]`, always allow, or keep as warn
3. Prefer allowlists over denylists -- it's impossible to overlook a useful lint
4. The friction of confronting every lint is good -- it forces intentionality

### Config Patterns

Schwartz provides a `clippy.toml` for test allowances:

```toml
allow-indexing-slicing-in-tests = true
allow-panic-in-tests = true
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-dbg-in-tests = true
```

Schwartz recommends `warn` level for most lints, using `-D warnings` on CI, which balances local iteration speed with enforcement.

### Workspace Considerations

If using a Cargo workspace, each crate must opt in with `lints.workspace = true`. On nightly there is a `missing_lints_inheritance` lint. On stable, use `cargo-workspace-lints` or a shell script on CI.

## Implications for cake

### Current State vs Recommendations

The project's current config:

```toml
[lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
pedantic = { level = "warn", priority = -1 }
nursery = { level = "warn", priority = -1 }
missing_docs_in_private_items = "allow"
missing_errors_doc = "deny"
enum_glob_use = "deny"
exit = "deny"
if_then_some_else_none = "warn"
map_err_ignore = "warn"
implicit_clone = "warn"
```

The project partially follows Levin's approach (enables `pedantic` and `nursery` categories) but does NOT enable `restriction`. Most of Schwartz's recommended lints are in the `restriction` category and are therefore currently off.

### Gaps Identified

1. **`restriction` category not enabled.** This is the single largest gap. Enabling it would bring in `string_slice`, `indexing_slicing`, `unwrap_in_result`, `panic_in_result_fn`, `let_underscore_must_use`, `let_underscore_future`, `await_holding_lock`, `await_holding_refcell_ref`, `large_futures`, `dbg_macro`, `allow_attributes`, `allow_attributes_without_reason`, and many more.

2. **No `clippy.toml` for test allowances.** The project denies `unwrap_used` and `expect_used` globally, which forces tests to avoid `.unwrap()` entirely. A `clippy.toml` with `allow-unwrap-in-tests = true` etc. would let tests use these while keeping enforcement in production code.

3. **`allow_attributes` + `allow_attributes_without_reason` missing.** These are coding-agent guardrails: prevent silent `#[allow]` without justification. Both articles recommend them.

4. **`deny` vs `warn` strategy.** The project uses `deny` for several lints but also runs `-D warnings` on CI via `clippy-strict`. Schwartz recommends `warn` + CI `-D warnings` for better local iteration. The project is mixed on this.

5. **Async lints missing.** With Tokio as the async runtime, lints like `await_holding_lock`, `await_holding_refcell_ref`, and `large_futures` are particularly relevant. Schwartz's motivating bug was a silently killed Tokio worker thread.

### Priority Lints to Evaluate

From highest to lowest relevance for this project:

| Category | Lint | Rationale |
|----------|------|-----------|
| Panic prevention | `string_slice` | UTF-8 boundary panics in CLI output/processing |
| Panic prevention | `indexing_slicing` | Out-of-bounds panics in data handling |
| Silent failure | `let_underscore_must_use` | Swallowed errors from fallible operations |
| Silent failure | `let_underscore_future` | Dropped futures silently canceling work |
| Async safety | `await_holding_lock` | Deadlock from holding locks across .await |
| Async safety | `await_holding_refcell_ref` | Same for RefCell |
| Async safety | `large_futures` | Stack overflow from oversized futures |
| Panic prevention | `unwrap_in_result` | .unwrap() in functions already returning Result |
| Panic prevention | `panic_in_result_fn` | panic!/assert! in Result-returning functions |
| Coding-agent guardrail | `allow_attributes` | Force #[expect(..., reason = "...")] |
| Coding-agent guardrail | `allow_attributes_without_reason` | Require justification on every suppression |
| Debug hygiene | `dbg_macro` | Stray dbg!() in committed code |
| Error handling | `unused_result_ok` | result.ok() discarding Err variant |
| Error handling | `map_err_ignore` | Already enabled as warn |
| Unsafe hygiene | `undocumented_unsafe_blocks` | Require SAFETY comments |
| Unsafe hygiene | `multiple_unsafe_ops_per_block` | One unsafe op per block |
| Numeric safety | `cast_sign_loss` | Sign-loss casts producing large unsigned values |
| Numeric safety | `float_cmp` | Direct float equality comparisons |
| Test config | `clippy.toml` allowances | Allow unwrap/expect/dbg in tests |

## Follow-ups

- Create a task for evaluating and potentially applying these Clippy configuration changes
- Evaluate whether to enable `restriction` category wholesale (Levin approach) or selectively (Schwartz approach)
- Create `clippy.toml` for test allowances regardless of which approach is taken
- Review current `deny` vs `warn` usage and align with CI strategy

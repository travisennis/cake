//! Shared resource limits for one agent task.

/// Fixed fan-out bound for per-turn planning, tool scheduling groups, and hook
/// subprocesses. Eight preserves the largest ordinary batch observed in local
/// Cake sessions while bounding pathological batches without adding a user
/// setting or changing model-visible ordering.
pub const MAX_CONCURRENT_AGENT_OPERATIONS: usize = 8;

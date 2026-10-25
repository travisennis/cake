//! The external-policy gate seam.
//!
//! Slice 1 makes the seam real without changing user-visible behavior: the
//! `cake-gate/1` protocol types, the process runner used by external gates, the
//! pure composer, and the built-in judge adapter. `[gates]` settings are not
//! read until slice 4, so the in-process judge remains the only production gate.

pub mod composer;
pub mod judge_gate;
pub mod protocol;
pub mod runner;

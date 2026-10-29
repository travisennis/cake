//! The external-policy gate seam.
//!
//! `cake-gate/1` protocol types, the process runner, the pure composer, and the
//! configuration-driven runtime live here. Policy stays outside Cake.

pub mod composer;
pub mod context;
pub mod protocol;
pub mod runner;

pub mod runtime;

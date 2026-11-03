//! Compile-time build identity recorded in sessions and telemetry.

/// The cake repository commit this binary was built from.
pub fn commit() -> Option<String> {
    option_env!("CAKE_BUILD_COMMIT")
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Whether the working tree had tracked changes when this binary was built.
pub fn dirty() -> Option<bool> {
    option_env!("CAKE_BUILD_DIRTY").and_then(|value| value.parse().ok())
}

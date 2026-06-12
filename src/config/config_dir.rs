/// XDG config directory resolution for cake.
///
/// Provides a single helper that owns the config-directory lookup, respecting
/// `$XDG_CONFIG_HOME` with a `~/.config` fallback.  All call sites that need
/// the user-level config root go through this function so the resolution order
/// is defined in one place.
use std::path::PathBuf;

/// Returns the XDG config home directory.
///
/// Resolution order:
/// 1. `$XDG_CONFIG_HOME` — if set and non-empty, used verbatim.
/// 2. `~/.config` — standard XDG fallback (current default).
/// 3. `./.config` — last resort when no home directory is available.
///
/// # Examples
///
/// ```ignore
/// let path = config_dir();
/// ```
pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }

    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
}

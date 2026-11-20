//! Shared filesystem object validation for the file-reading tools.

use std::path::Path;

/// Reject anything that is not a regular file before a tool opens it.
///
/// `Read` and `ReadImage` open files with a blocking `std::fs::File::open`.
/// Their byte limits bound the read, not the open, so a named pipe with no
/// writer never returns. Both tools call this metadata check first. Callers
/// pass a canonical path, so the check follows an allowed symlink to a regular
/// file exactly as the later open does, and a directory keeps its own
/// diagnostic.
///
/// This is a precheck, not a race-free open: a path replaced between the check
/// and the open is not covered.
pub(super) fn validate_regular_file(path: &Path) -> Result<(), String> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("Path not found: {}", path.display())
        } else {
            format!("Failed to access file '{}': {error}", path.display())
        }
    })?;

    if metadata.is_file() {
        Ok(())
    } else if metadata.is_dir() {
        Err(format!(
            "Path is a directory, not a file: {}",
            path.display()
        ))
    } else {
        Err(format!(
            "Path is not a regular file: {} (only regular files can be read)",
            path.display()
        ))
    }
}

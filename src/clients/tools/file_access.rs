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
/// and the open is not covered (task 043).
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

/// Process isolation for tests whose regression mode is an indefinite block.
///
/// [`validate_regular_file`] is what keeps a FIFO test from blocking. If that
/// check regresses, running the test body in-process would hang the whole
/// suite and runtime shutdown, so a test that opens a pipe runs in a child
/// process with an external deadline instead.
#[cfg(test)]
#[cfg(unix)]
pub(super) mod process_isolation {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// Marks the child so it runs the test body instead of spawning again.
    const CHILD_ENV: &str = "CAKE_TOOL_READ_FIFO_TEST_CHILD";

    /// Whether this process is the isolated child of [`run_isolated`].
    pub fn in_child() -> bool {
        std::env::var_os(CHILD_ENV).is_some()
    }

    /// Re-run `test_name` (a unique test-name filter) in a child process and
    /// require it to finish within `deadline`.
    ///
    /// The child starts with [`CHILD_ENV`] set, so the test body runs its real
    /// assertions there. A blocked open leaves the parent to kill and report
    /// the child after the deadline instead of hanging the suite.
    pub fn run_isolated(test_name: &str, deadline: Duration) {
        let exe = std::env::current_exe().unwrap();
        let mut child = Command::new(exe)
            .arg(test_name)
            .arg("--test-threads=1")
            .env(CHILD_ENV, "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();

        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if started.elapsed() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("isolated test {test_name} blocked for more than {deadline:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        };

        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            status.success() && stdout.contains("test result: ok. 1 passed"),
            "isolated test {test_name} failed ({status}); expected exactly one passing test\n\
             stdout:\n{stdout}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

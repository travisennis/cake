#![expect(
    clippy::expect_used,
    reason = "test helper uses expect for fixture setup"
)]

//! Trust helpers for integration tests that exercise project authority.
//!
//! Included explicitly by test files that need it, because a shared helper
//! compiled into every test binary would be dead code in the others.

use crate::support::TestEnv;

impl TestEnv {
    /// Run `cake trust` for the fixture workspace.
    pub fn trust_project(&self) {
        let output = self
            .command()
            .arg("trust")
            .output()
            .expect("failed to execute cake trust");
        assert!(
            output.status.success(),
            "cake trust should succeed. stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

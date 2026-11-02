//! One-run trust helper for integration tests that only need an accepted
//! invocation and not a persistent record.

use std::process::Command;

use crate::support::TestEnv;

impl TestEnv {
    /// A command that accepts the current project snapshot for one invocation.
    pub fn trusted_command(&self) -> Command {
        let mut cmd = self.command();
        cmd.arg("--trust-project");
        cmd
    }
}

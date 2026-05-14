//! Linux sandbox implementation using Landlock LSM
//!
//! Landlock is a Linux Security Module available since kernel 5.13 that
//! allows unprivileged processes to sandbox themselves.
//!
//! This implementation uses `CommandExt::pre_exec` to apply Landlock rules
//! in the child process after `fork()` but before `exec()`.

use crate::clients::tools::sandbox::{SandboxConfig, SandboxStrategy};

/// Linux sandbox strategy using Landlock LSM
#[derive(Debug, Clone, Copy)]
pub struct LandlockSandbox;

impl LandlockSandbox {
    #[cfg(feature = "landlock")]
    fn enforce_full_ruleset(status: &landlock::RulesetStatus) -> Result<(), std::io::Error> {
        match status {
            landlock::RulesetStatus::FullyEnforced => Ok(()),
            landlock::RulesetStatus::PartiallyEnforced => Err(std::io::Error::other(
                "Linux sandbox unavailable: Landlock only partially enforced the filesystem \
                 ruleset. Set CAKE_SANDBOX=off to run Bash commands without filesystem \
                 sandboxing.",
            )),
            landlock::RulesetStatus::NotEnforced => Err(std::io::Error::other(
                "Linux sandbox unavailable: Landlock did not enforce the filesystem ruleset. \
                 This usually means the kernel lacks required Landlock support. Set \
                 CAKE_SANDBOX=off to run Bash commands without filesystem sandboxing.",
            )),
        }
    }

    /// Apply Landlock rules in the current process (to be called in `pre_exec`)
    #[cfg(feature = "landlock")]
    fn apply_landlock_rules(config: &SandboxConfig) -> Result<(), std::io::Error> {
        use landlock::{ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr};

        let abi = ABI::V5;

        let mut ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .map_err(|e| std::io::Error::other(format!("Failed to configure ruleset access: {e}")))?
            .create()
            .map_err(|e| {
                std::io::Error::other(format!("Failed to create Landlock ruleset: {e}"))
            })?;

        // Add read-write rules for cwd and temp dirs
        let rw_access = AccessFs::from_all(abi);
        for path in &config.read_write {
            if path.exists() {
                ruleset = ruleset
                    .add_rules(landlock::path_beneath_rules(&[path], rw_access))
                    .map_err(|e| {
                        std::io::Error::other(format!(
                            "Failed to add rw rule for {}: {e}",
                            path.display()
                        ))
                    })?;
            }
        }

        // Add read-only + exec rules for system paths
        let ro_exec_access = AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute;
        for path in &config.read_only_exec {
            if path.exists() {
                ruleset = ruleset
                    .add_rules(landlock::path_beneath_rules(&[path], ro_exec_access))
                    .map_err(|e| {
                        std::io::Error::other(format!(
                            "Failed to add ro+exec rule for {}: {e}",
                            path.display()
                        ))
                    })?;
            }
        }

        // Add read-only rules
        let read_access = AccessFs::ReadFile | AccessFs::ReadDir;
        for path in &config.read_only {
            if path.exists() {
                ruleset = ruleset
                    .add_rules(landlock::path_beneath_rules(&[path], read_access))
                    .map_err(|e| {
                        std::io::Error::other(format!(
                            "Failed to add ro rule for {}: {e}",
                            path.display()
                        ))
                    })?;
            }
        }

        let status = ruleset.restrict_self().map_err(|e| {
            std::io::Error::other(format!("Failed to restrict process with Landlock: {e}"))
        })?;

        Self::enforce_full_ruleset(&status.ruleset)
    }
}

impl SandboxStrategy for LandlockSandbox {
    fn apply(
        &self,
        command: &mut tokio::process::Command,
        config: &SandboxConfig,
    ) -> Result<(), String> {
        #[cfg(feature = "landlock")]
        {
            let config = config.clone();
            // SAFETY: `pre_exec` runs in the child process immediately before
            // `exec`; this closure only installs Landlock rules for that child.
            unsafe {
                command.pre_exec(move || Self::apply_landlock_rules(&config));
            }

            Ok(())
        }

        #[cfg(not(feature = "landlock"))]
        {
            let _ = (
                &config.read_write,
                &config.read_only_exec,
                &config.read_only,
            );
            let _ = command;
            Err(
                "Linux sandbox unavailable: cake was built without Landlock support. Rebuild \
                 with --features landlock, or set CAKE_SANDBOX=off to run Bash commands without \
                 filesystem sandboxing."
                    .to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(target_os = "linux", not(feature = "landlock")))]
    #[test]
    fn apply_fails_closed_without_landlock_feature() {
        let sandbox = LandlockSandbox;
        let context = crate::clients::tools::ToolContext::with_temp_dirs(
            std::path::PathBuf::from("/tmp"),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let config = SandboxConfig::build(&context);
        let mut command = tokio::process::Command::new("bash");

        let Err(error) = sandbox.apply(&mut command, &config) else {
            panic!("Linux sandboxing should fail closed without Landlock support");
        };

        assert!(error.contains("built without Landlock support"));
        assert!(error.contains("CAKE_SANDBOX=off"));
    }

    #[cfg(all(target_os = "linux", feature = "landlock"))]
    #[test]
    fn landlock_status_must_be_fully_enforced() {
        use landlock::RulesetStatus;

        assert!(LandlockSandbox::enforce_full_ruleset(&RulesetStatus::FullyEnforced).is_ok());

        let Err(partial) = LandlockSandbox::enforce_full_ruleset(&RulesetStatus::PartiallyEnforced)
        else {
            panic!("partial enforcement must fail closed");
        };
        let partial = partial.to_string();
        assert!(partial.contains("partially enforced"));

        let Err(missing) = LandlockSandbox::enforce_full_ruleset(&RulesetStatus::NotEnforced)
        else {
            panic!("missing enforcement must fail closed");
        };
        let missing = missing.to_string();
        assert!(missing.contains("did not enforce"));
    }
}

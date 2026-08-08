//! Linux sandbox implementation using Landlock LSM
//!
//! Landlock is a Linux Security Module available since kernel 5.13 that
//! allows unprivileged processes to sandbox themselves.
//!
//! This implementation prepares a Landlock ruleset before spawning, then uses
//! `CommandExt::pre_exec` only to restrict the child process after `fork()` and
//! before `exec()`.

use crate::clients::tools::sandbox::{SandboxConfig, SandboxGuard, SandboxStrategy};

#[derive(Debug, PartialEq, Eq)]
enum EnforcementFailure {
    PartiallyEnforced,
    NotEnforced,
}

#[derive(Debug, PartialEq, Eq)]
struct RulePaths {
    writable: Vec<std::path::PathBuf>,
    system_paths: Vec<std::path::PathBuf>,
    readable: Vec<std::path::PathBuf>,
}

/// Linux sandbox strategy using Landlock LSM
#[derive(Debug, Clone, Copy)]
pub struct LandlockSandbox;

impl LandlockSandbox {
    fn enforce_full_ruleset(status: &landlock::RulesetStatus) -> Result<(), EnforcementFailure> {
        match status {
            landlock::RulesetStatus::FullyEnforced => Ok(()),
            landlock::RulesetStatus::PartiallyEnforced => {
                Err(EnforcementFailure::PartiallyEnforced)
            },
            landlock::RulesetStatus::NotEnforced => Err(EnforcementFailure::NotEnforced),
        }
    }

    fn prepare_rule_paths(config: &SandboxConfig) -> RulePaths {
        let existing = |paths: &[std::path::PathBuf]| {
            paths.iter().filter(|path| path.exists()).cloned().collect()
        };

        RulePaths {
            writable: existing(&config.writable),
            system_paths: existing(&config.system_paths),
            readable: existing(&config.readable),
        }
    }

    /// Create and populate the ruleset in the parent process.
    fn prepare_ruleset(config: &SandboxConfig) -> Result<landlock::RulesetCreated, String> {
        use landlock::{ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr};

        let abi = ABI::V5;

        let mut ruleset = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .map_err(|e| format!("Failed to configure ruleset access: {e}"))?
            .create()
            .map_err(|e| format!("Failed to create Landlock ruleset: {e}"))?;

        // Resolve path existence and open the rule FDs before fork. The
        // resulting RulesetCreated owns everything the child needs.
        let paths = Self::prepare_rule_paths(config);

        // Add read-write rules for cwd and temp dirs.
        let rw_access = AccessFs::from_all(abi);
        for path in &paths.writable {
            ruleset = ruleset
                .add_rules(landlock::path_beneath_rules([path], rw_access))
                .map_err(|e| format!("Failed to add rw rule for {}: {e}", path.display()))?;
        }

        // Add read-only + exec rules for system paths.
        let ro_exec_access = AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute;
        for path in &paths.system_paths {
            ruleset = ruleset
                .add_rules(landlock::path_beneath_rules([path], ro_exec_access))
                .map_err(|e| format!("Failed to add ro+exec rule for {}: {e}", path.display()))?;
        }

        // Add read-only + exec rules for readable paths. Execute is included
        // so read-only paths (skill dirs, --add-dir, and everything the
        // read-only policy demotes from writable: workspace, toolchain
        // caches) can still run scripts and binaries, matching macOS Seatbelt
        // where file-read* plus the global process-exec allow is sufficient
        // to exec. Read-only denies mutations, not execution.
        for path in &paths.readable {
            ruleset = ruleset
                .add_rules(landlock::path_beneath_rules([path], ro_exec_access))
                .map_err(|e| format!("Failed to add ro rule for {}: {e}", path.display()))?;
        }

        Ok(ruleset)
    }

    /// Restrict the post-fork child without allocation or filesystem discovery.
    fn restrict_child(ruleset: landlock::RulesetCreated) -> Result<(), std::io::Error> {
        let status = ruleset.restrict_self().map_err(|error| {
            let errno = landlock::Errno::from(error);
            std::io::Error::from_raw_os_error(*errno)
        })?;

        Self::enforce_full_ruleset(&status.ruleset)
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
    }
}

impl SandboxStrategy for LandlockSandbox {
    fn apply(
        &self,
        command: &mut tokio::process::Command,
        config: &SandboxConfig,
    ) -> Result<SandboxGuard, String> {
        let mut ruleset = Some(Self::prepare_ruleset(config)?);
        // SAFETY: all path lookup, allocation, error formatting, ruleset
        // creation, and rule insertion happened above in the parent. After
        // fork, this closure only moves the inherited ruleset value, performs
        // the Landlock crate's no_new_privs and restrict_self syscalls, maps
        // failures to errno-backed io::Errors, and closes the ruleset FD.
        // Those operations neither allocate nor acquire process-global locks.
        unsafe {
            command.pre_exec(move || {
                let Some(ruleset) = ruleset.take() else {
                    return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
                };
                Self::restrict_child(ruleset)
            });
        }

        Ok(SandboxGuard::empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn landlock_status_must_be_fully_enforced() {
        use landlock::RulesetStatus;

        assert!(LandlockSandbox::enforce_full_ruleset(&RulesetStatus::FullyEnforced).is_ok());

        let Err(partial) = LandlockSandbox::enforce_full_ruleset(&RulesetStatus::PartiallyEnforced)
        else {
            panic!("partial enforcement must fail closed");
        };
        assert_eq!(partial, EnforcementFailure::PartiallyEnforced);

        let Err(missing) = LandlockSandbox::enforce_full_ruleset(&RulesetStatus::NotEnforced)
        else {
            panic!("missing enforcement must fail closed");
        };
        assert_eq!(missing, EnforcementFailure::NotEnforced);
    }

    #[test]
    fn rule_paths_are_filtered_and_classified_before_fork() {
        let temp = tempfile::tempdir().unwrap();
        let writable = temp.path().join("writable");
        let readable = temp.path().join("readable");
        let readable_file = temp.path().join("tool");
        std::fs::create_dir(&writable).unwrap();
        std::fs::create_dir(&readable).unwrap();
        std::fs::write(&readable_file, b"#!/bin/sh\n").unwrap();
        let missing = temp.path().join("missing");

        let config = SandboxConfig {
            writable: vec![missing.clone(), writable.clone()],
            system_paths: vec![missing],
            readable: vec![readable.clone(), readable_file.clone()],
            policy: crate::clients::tools::sandbox::SandboxPolicy::WorkspaceWrite,
        };

        assert_eq!(
            LandlockSandbox::prepare_rule_paths(&config),
            RulePaths {
                writable: vec![writable],
                system_paths: Vec::new(),
                // A `[sandbox].read_only` file grant survives classification
                // and is handed to `path_beneath_rules`, which applies the
                // rights to the file object itself.
                readable: vec![readable, readable_file],
            }
        );
    }
}

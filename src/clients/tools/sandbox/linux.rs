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
    read_execute: Vec<std::path::PathBuf>,
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
            read_execute: existing(&config.read_execute),
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

        let paths = Self::prepare_rule_paths(config);

        let rw_access = AccessFs::from_all(abi);
        for path in &paths.writable {
            ruleset = ruleset
                .add_rules(landlock::path_beneath_rules([path], rw_access))
                .map_err(|e| format!("Failed to add rw rule for {}: {e}", path.display()))?;
        }

        let read_execute_access = AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute;
        for path in &paths.read_execute {
            ruleset = ruleset
                .add_rules(landlock::path_beneath_rules([path], read_execute_access))
                .map_err(|e| {
                    format!(
                        "Failed to add read-and-execute rule for {}: {e}",
                        path.display()
                    )
                })?;
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

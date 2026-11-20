//! Hermetic construction of `git` subprocesses.
//!
//! Git exports repository-pinning variables such as `GIT_DIR` into hook
//! processes and everything those hooks spawn. In a linked worktree `GIT_DIR`
//! is absolute, so it survives into unrelated child processes and redirects
//! every `git` invocation at the exporting repository no matter which working
//! directory the child chose. Cake always selects a repository by working
//! directory, so those variables are never wanted here.

use std::{path::Path, process::Command};

/// Environment variables that redirect or reconfigure `git` independently of
/// the working directory, and that cake therefore never inherits.
///
/// The first eight pin git to a specific repository, index, or object store,
/// overriding discovery. `GIT_CONFIG_PARAMETERS` is the serialized form git
/// uses to propagate one invocation's `-c` options into the subprocesses it
/// spawns; git documents it as internal, and options meant for a parent
/// invocation are not settings for cake's unrelated ones.
pub const AMBIENT_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_CONFIG_PARAMETERS",
];

/// Build a `git` command that discovers its repository from `working_dir`,
/// ignoring any repository or parent `-c` options pinned by the ambient
/// environment.
///
/// The user's own configuration is deliberately left intact: cake acts on the
/// user's repository and must honor their git config, identity, and hooks.
pub fn command(working_dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(working_dir);
    for var in AMBIENT_ENV_VARS {
        cmd.env_remove(var);
    }
    cmd
}

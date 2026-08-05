//! Hermetic construction of `git` subprocesses.
//!
//! Git exports repository-pinning variables such as `GIT_DIR` into hook
//! processes and everything those hooks spawn. In a linked worktree `GIT_DIR`
//! is absolute, so it survives into unrelated child processes and redirects
//! every `git` invocation at the exporting repository no matter which working
//! directory the child chose. Cake always selects a repository by working
//! directory, so those variables are never wanted here.

use std::{path::Path, process::Command};

/// Environment variables that pin `git` to a specific repository, index, or
/// object store, overriding discovery from the working directory.
pub const REPOSITORY_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// Environment variables that inject configuration at command scope, which
/// outranks a repository's own local configuration.
///
/// `GIT_CONFIG_COUNT` gates the numbered `GIT_CONFIG_KEY_<n>` and
/// `GIT_CONFIG_VALUE_<n>` pairs, so clearing it disables all of them.
/// `GIT_CONFIG_PARAMETERS` is the serialized form git uses to propagate its
/// own `-c` options into subprocesses, which is how these reach a test suite
/// launched from a hook.
///
/// Test-only: production honors the user's configuration at every scope, so
/// only fixtures drop these.
#[cfg(test)]
pub const CONFIG_ENV_VARS: &[&str] = &["GIT_CONFIG", "GIT_CONFIG_COUNT", "GIT_CONFIG_PARAMETERS"];

/// Build a `git` command that discovers its repository from `working_dir`,
/// ignoring any repository pinned by the ambient environment.
///
/// User-facing configuration is deliberately left intact: cake acts on the
/// user's repository and must honor the user's git config and hooks.
pub fn command(working_dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(working_dir);
    for var in REPOSITORY_ENV_VARS {
        cmd.env_remove(var);
    }
    cmd
}

#[cfg(test)]
pub mod test_support {
    use std::{path::Path, process::Command};

    /// Build a `git` command for a fixture repository at `working_dir`.
    ///
    /// On top of [`crate::config::git::command`], this isolates the fixture from the
    /// developer's environment entirely: no configuration is read or written at
    /// any scope, no hooks run, and the committer identity is supplied inline.
    /// A test must never be able to reach the repository it runs in.
    pub fn git(working_dir: &Path) -> Command {
        let mut cmd = crate::config::git::command(working_dir);
        for var in crate::config::git::CONFIG_ENV_VARS {
            cmd.env_remove(var);
        }
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd.env("GIT_CONFIG_NOSYSTEM", "1");
        cmd.args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Cake Test",
            "-c",
            "user.email=cake-test@example.invalid",
        ]);
        cmd
    }

    /// Run `args` in `working_dir` through [`git`] and panic on failure.
    pub fn run_git(working_dir: &Path, args: &[&str]) {
        let output = git(working_dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn git {args:?}: {e}"));
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Create a fixture repository at `dir` with one empty commit.
    ///
    /// The repository pins `core.hooksPath` locally so that commands cake's
    /// production code runs against the fixture cannot pick up hooks from the
    /// developer's global configuration either.
    pub fn init_repo(dir: &Path) {
        run_git(dir, &["init"]);
        run_git(dir, &["config", "core.hooksPath", "/dev/null"]);
        run_git(dir, &["commit", "--allow-empty", "-m", "initial"]);
    }

    /// Create a fixture repository at `main_repo` holding one committed file,
    /// plus a detached linked worktree at `wt_path`.
    pub fn init_repo_with_linked_worktree(main_repo: &Path, wt_path: &Path) {
        std::fs::create_dir_all(main_repo).expect("fixture repository directory");
        run_git(main_repo, &["init", "--initial-branch=main"]);
        run_git(main_repo, &["config", "core.hooksPath", "/dev/null"]);
        std::fs::write(main_repo.join("README.md"), b"# test\n").expect("fixture README");
        run_git(main_repo, &["add", "README.md"]);
        run_git(main_repo, &["commit", "-m", "initial"]);
        run_git(
            main_repo,
            &[
                "worktree",
                "add",
                "--detach",
                &wt_path.to_string_lossy(),
                "main",
            ],
        );
        assert!(
            wt_path.join(".git").is_file(),
            ".git in a linked worktree must be a file"
        );
    }
}

#[cfg(test)]
mod tests {
    use crate::config::git::{command, test_support};
    use std::fs;
    use tempfile::TempDir;

    /// Absolute git directory that production [`command`] resolves for
    /// `working_dir`.
    fn resolved_git_dir(working_dir: &std::path::Path) -> std::path::PathBuf {
        let output = command(working_dir)
            .args(["rev-parse", "--absolute-git-dir"])
            .output()
            .expect("git rev-parse must spawn");
        assert!(
            output.status.success(),
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let reported = String::from_utf8(output.stdout).expect("git output must be UTF-8");
        fs::canonicalize(reported.trim()).expect("reported git dir must exist")
    }

    #[test]
    fn command_discovers_the_repository_at_the_working_directory() {
        let dir = TempDir::new().unwrap();
        test_support::init_repo(dir.path());

        assert_eq!(
            resolved_git_dir(dir.path()),
            fs::canonicalize(dir.path().join(".git")).unwrap()
        );
    }

    #[test]
    fn command_ignores_an_inherited_git_dir() {
        let workspace = TempDir::new().unwrap();
        test_support::init_repo(workspace.path());
        let elsewhere = TempDir::new().unwrap();
        test_support::init_repo(elsewhere.path());
        let poison = fs::canonicalize(elsewhere.path().join(".git")).unwrap();

        // Setting the variable process-wide is safe precisely because every
        // git invocation in the suite goes through this module and drops it.
        let resolved = temp_env::with_var("GIT_DIR", Some(&poison), || {
            resolved_git_dir(workspace.path())
        });

        assert_eq!(
            resolved,
            fs::canonicalize(workspace.path().join(".git")).unwrap(),
            "an inherited GIT_DIR must not redirect the command"
        );
    }

    #[test]
    fn a_fixture_ignores_command_scope_config_from_the_environment() {
        // Command-scope configuration outranks a repository's local config, so
        // an inherited `GIT_CONFIG_COUNT` would reach fixtures for every key
        // the fixture builder does not pin inline — `core.hooksPath` among
        // them, for any command that carries no `-c` of its own.
        let dir = TempDir::new().unwrap();
        test_support::init_repo(dir.path());

        let leaked = temp_env::with_vars(
            [
                ("GIT_CONFIG_COUNT", Some("1")),
                ("GIT_CONFIG_KEY_0", Some("cake.sentinel")),
                ("GIT_CONFIG_VALUE_0", Some("leaked")),
            ],
            || {
                test_support::git(dir.path())
                    .args(["config", "--get", "cake.sentinel"])
                    .output()
                    .expect("git config must spawn")
            },
        );

        assert!(
            !leaked.status.success(),
            "command-scope config must not reach a fixture, got: {}",
            String::from_utf8_lossy(&leaked.stdout)
        );
    }

    #[test]
    fn an_inherited_git_dir_cannot_be_mutated_by_a_fixture() {
        // The observed damage: `git init` run with an inherited GIT_DIR and a
        // working directory that is not that repository's work tree flips
        // `core.bare` in the pinned repository and rewrites its identity.
        let victim = TempDir::new().unwrap();
        test_support::init_repo(victim.path());
        let poison = fs::canonicalize(victim.path().join(".git")).unwrap();
        let before = fs::read_to_string(victim.path().join(".git/config")).unwrap();

        let fixture = TempDir::new().unwrap();
        temp_env::with_var("GIT_DIR", Some(&poison), || {
            test_support::init_repo(fixture.path());
            test_support::run_git(fixture.path(), &["config", "user.name", "Test User"]);
        });

        let after = fs::read_to_string(victim.path().join(".git/config")).unwrap();
        assert_eq!(
            before, after,
            "a fixture repository must not write to the repository pinned by GIT_DIR"
        );
        assert!(
            fixture.path().join(".git").is_dir(),
            "the fixture must have initialized its own git directory"
        );
    }
}

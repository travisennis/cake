use std::{fs, path::PathBuf, process::Command};

use tempfile::TempDir;

fn binary_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cake"))
}

/// Environment variables no test may hand to `git` or to the binary under
/// test: those that redirect or reconfigure git independently of the working
/// directory, plus those that outrank a fixture's own `-c` options.
///
/// Integration tests are a separate crate and cannot reach `src/config/git.rs`.
/// This list is the union of that module's two constants, so it is
/// intentionally stricter than production, which honors the user's own
/// configuration and identity. Keep it a superset of `AMBIENT_ENV_VARS`.
pub const GIT_AMBIENT_ENV_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
    "GIT_CONFIG",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
];

pub struct TestEnv {
    _root: TempDir,
    pub workspace_dir: PathBuf,
    home_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl TestEnv {
    pub fn new(prefix: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("failed to create temp test root");
        let workspace_dir = root.path().join("workspace");
        let home_dir = root.path().join("home");
        let data_dir = root.path().join("data");

        fs::create_dir_all(&workspace_dir).expect("failed to create temp workspace");
        fs::create_dir_all(home_dir.join(".config")).expect("failed to create temp home config");

        Self {
            _root: root,
            workspace_dir,
            home_dir,
            data_dir,
        }
    }

    pub fn command(&self) -> Command {
        let mut cmd = Command::new(binary_path());
        cmd.current_dir(&self.workspace_dir)
            .env("HOME", &self.home_dir)
            .env("XDG_CONFIG_HOME", self.home_dir.join(".config"))
            .env("CAKE_DATA_DIR", &self.data_dir);
        // Never hand the binary under test a repository, configuration, or
        // identity pinned by the environment the suite was launched from.
        for var in GIT_AMBIENT_ENV_VARS {
            cmd.env_remove(var);
        }
        cmd
    }

    pub fn write_project_settings(&self, content: &str) {
        let settings_dir = self.workspace_dir.join(".cake");
        fs::create_dir_all(&settings_dir).expect("failed to create .cake directory");
        fs::write(settings_dir.join("settings.toml"), content)
            .expect("failed to write project settings.toml");
    }
}

use std::{fs, path::PathBuf, process::Command};

use tempfile::TempDir;

/// Resolve the path to the `cake` binary under test.
///
/// Prefer the path Cargo exports to the test process at **runtime** via
/// `CARGO_BIN_EXE_cake`, so the resolved binary always matches the current
/// build. A compile-time `env!` bake records whichever target directory first
/// produced the test artifact; when that target layout later moves (e.g. a
/// shared/parent workspace target dir), the baked path dangles and
/// `Command::new` fails with `NotFound` until the test artifact is recompiled.
/// Reading the variable at runtime, validated against the filesystem, keeps a
/// warm `target/` from breaking the pre-push gate.
fn binary_path() -> PathBuf {
    choose_binary_path(
        std::env::var_os("CARGO_BIN_EXE_cake").map(PathBuf::from),
        PathBuf::from(env!("CARGO_BIN_EXE_cake")),
    )
}

/// Pick the binary path to use, preferring a runtime-resolved path that exists
/// on disk over the compile-time baked fallback.
fn choose_binary_path(runtime: Option<PathBuf>, baked: PathBuf) -> PathBuf {
    runtime.filter(|p| p.is_file()).unwrap_or(baked)
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
        // identity pinned by the environment the suite was launched from, and
        // never let an ambient `CAKE_TOOLBOX` point the binary at a host
        // program the tests did not stage. Toolbox tests set `CAKE_TOOLBOX`
        // (or `--toolbox`) explicitly for themselves.
        for var in GIT_AMBIENT_ENV_VARS {
            cmd.env_remove(var);
        }
        cmd.env_remove("CAKE_TOOLBOX");
        cmd
    }

    pub fn write_project_settings(&self, content: &str) {
        let settings_dir = self.workspace_dir.join(".cake");
        fs::create_dir_all(&settings_dir).expect("failed to create .cake directory");
        fs::write(settings_dir.join("settings.toml"), content)
            .expect("failed to write project settings.toml");
    }
}

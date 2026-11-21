//! OS-level sandboxing for the Bash tool
//!
//! Provides filesystem sandboxing using:
//! - macOS: `sandbox-exec` (Seatbelt profile)
//! - Linux: Landlock LSM (kernel 5.13+)
//!
//! The sandbox restricts filesystem access to:
//! - Read, write, and execute: workspace, temp, and configured writable paths
//! - Read and execute, no write: system, config, skill, and demoted paths
//! - Deny: everything else

use std::path::{Path, PathBuf};

use crate::clients::tools::ToolContext;

mod grants_report;
pub use grants_report::{GrantReport, GrantReportInputs};

// =============================================================================
// Platform-specific implementations
// =============================================================================

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(super) use macos::MacOsSandbox;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(super) use linux::LandlockSandbox;

//=============================================================================
// Sandbox Policy
//=============================================================================

/// The filesystem sandbox policy applied to model-generated shell commands.
///
/// `WorkspaceWrite` reproduces the historical default: read-write access to the
/// project directory, temp directories, and toolchain/runtime caches, with
/// read-only access to system and config paths. `ReadOnly` is strictly more
/// restrictive: the project directory and toolchain caches become read-only,
/// while temp directories stay read-write so commands can still produce
/// intermediate output. `WorkspaceWriteInteractive` keeps those path grants
/// and adds the macOS capabilities that let a command launch and automate
/// other applications. `DangerFullAccess` skips the sandbox entirely.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum SandboxPolicy {
    /// Read-only: deny writes to the workspace and toolchain dirs.
    ReadOnly,
    /// Read-write workspace with toolchain caches (current default).
    #[default]
    WorkspaceWrite,
    /// Workspace-write access plus macOS app launching and automation, for
    /// interactive sessions.
    WorkspaceWriteInteractive,
    /// No sandbox restrictions.
    DangerFullAccess,
}

impl SandboxPolicy {
    /// The stable CLI spelling of this policy.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::WorkspaceWriteInteractive => "workspace-write-interactive",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    /// Whether Cake applies an OS filesystem sandbox under this policy.
    pub const fn applies_os_sandbox(self) -> bool {
        !matches!(self, Self::DangerFullAccess)
    }

    /// Whether model-generated commands may launch and automate other
    /// applications.
    ///
    /// Only `WorkspaceWriteInteractive` grants the macOS `lsopen` and
    /// `appleevent-send` primitives. They are platform capabilities rather
    /// than filesystem grants, so they never appear in [`SandboxConfig`]'s
    /// path lists; this predicate is the single place that decides whether a
    /// platform strategy emits them.
    #[cfg(target_os = "macos")]
    pub(super) const fn allows_app_interaction(self) -> bool {
        matches!(self, Self::WorkspaceWriteInteractive)
    }
}

/// Where the effective sandbox policy came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SandboxPolicySource {
    /// The explicit `--sandbox` flag.
    Cli,
    /// The backward-compatible `CAKE_SANDBOX=off` environment variable.
    Environment,
    /// The compiled default.
    Default,
}

impl SandboxPolicySource {
    /// The stable label used in diagnostic output.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cli => "--sandbox",
            Self::Environment => "CAKE_SANDBOX",
            Self::Default => "default",
        }
    }
}

/// Resolve the effective sandbox policy and where it came from.
pub fn resolve_sandbox_policy_with_source(
    cli: Option<SandboxPolicy>,
) -> (SandboxPolicy, SandboxPolicySource) {
    if let Some(policy) = cli {
        return (policy, SandboxPolicySource::Cli);
    }
    if is_sandbox_disabled() {
        (
            SandboxPolicy::DangerFullAccess,
            SandboxPolicySource::Environment,
        )
    } else {
        (SandboxPolicy::WorkspaceWrite, SandboxPolicySource::Default)
    }
}

/// Resolve the effective sandbox policy from an optional CLI `--sandbox`
/// value and the `CAKE_SANDBOX` environment variable.
///
/// An explicit CLI value always wins. When no flag is passed, `CAKE_SANDBOX=off`
/// (and its aliases) maps to `DangerFullAccess` for backward compatibility;
/// otherwise the policy defaults to `WorkspaceWrite`.
pub fn resolve_sandbox_policy(cli: Option<SandboxPolicy>) -> SandboxPolicy {
    resolve_sandbox_policy_with_source(cli).0
}

// =============================================================================
// Core Types
// =============================================================================

#[derive(Clone, Debug)]
pub(super) struct SandboxConfig {
    /// Paths with read, write, and execute access.
    pub writable: Vec<PathBuf>,
    /// Paths with read and execute access, but no write access.
    pub read_execute: Vec<PathBuf>,
    /// The sandbox policy this config was built for. The macOS strategy uses
    /// this to adjust rules that are not derived purely from the path lists
    /// (e.g. macOS Keychain file grants and app-launching capabilities); the
    /// Linux strategy derives every rule from the path lists alone.
    #[cfg(target_os = "macos")]
    pub policy: SandboxPolicy,
    /// Directories the user granted directly --- `settings_dirs`,
    /// `additional_dirs`, and `skill_dirs` --- in lexical and canonical form.
    ///
    /// Captured before the read-only policy demotes settings directories out
    /// of the writable set, so it survives that demotion. Used only to decide
    /// whether a Bash call may select a directory as its `cwd`; the platform
    /// sandbox profiles never read it.
    pub user_grants: Vec<PathBuf>,
}

const SCM_CLI_PATHS: &[&str] = &[
    ".config/gh",
    ".cache/gh",
    ".local/share/gh",
    ".local/state/gh",
    ".config/glab-cli",
    ".cache/glab-cli",
    ".local/share/glab-cli",
    ".local/state/glab-cli",
];

/// Credential-bearing paths that the in-process file tools never expose.
///
/// A file entry denies exactly that path; a directory entry denies its
/// subtree. Entries are expanded under the resolved home directory and stored
/// in both lexical and canonical form so a symlink cannot bypass them.
const DENIED_HOME_PATHS: &[(&str, DenyKind)] = &[
    (".ssh", DenyKind::Directory),
    (".aws", DenyKind::Directory),
    (".azure", DenyKind::Directory),
    (".config/gcloud", DenyKind::Directory),
    (".kube", DenyKind::Directory),
    (".docker/config.json", DenyKind::File),
    (".config/gh", DenyKind::Directory),
    (".config/glab-cli", DenyKind::Directory),
    (".npmrc", DenyKind::File),
    (".yarnrc", DenyKind::File),
    (".yarnrc.yml", DenyKind::File),
    (".cargo/credentials", DenyKind::File),
    (".cargo/credentials.toml", DenyKind::File),
    (".netrc", DenyKind::File),
    (".git-credentials", DenyKind::File),
    ("Library/Keychains", DenyKind::Directory),
];

/// Whether a denied path names a single file or a directory subtree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DenyKind {
    File,
    Directory,
}

/// Canonicalize a path, resolving the deepest existing ancestor and appending
/// any missing components lexically.
///
/// Deny entries must match a target that does not exist yet, such as a new
/// `~/.npmrc` or a file under a not-yet-created `~/.ssh`. Canonicalizing only
/// the existing prefix also keeps the comparison correct when `HOME` itself
/// is reached through a symlink (for example `/var` -> `/private/var`).
fn canonicalize_allow_missing(path: &Path) -> Option<PathBuf> {
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }

    let parent = path.parent()?;
    let name = path.file_name()?;
    Some(canonicalize_allow_missing(parent)?.join(name))
}

/// One credential-bearing path that wins over every ordinary grant.
#[derive(Clone, Debug)]
pub(super) struct DeniedPath {
    path: PathBuf,
    canonical: Option<PathBuf>,
    kind: DenyKind,
}

impl DeniedPath {
    fn new(path: PathBuf, kind: DenyKind) -> Self {
        let canonical = canonicalize_allow_missing(&path);
        Self {
            path,
            canonical,
            kind,
        }
    }

    fn matches(&self, path: &Path, canonical: Option<&Path>) -> bool {
        let matches_candidate = |candidate: &Path| match self.kind {
            DenyKind::File => {
                self.path == candidate || self.canonical.as_deref() == Some(candidate)
            },
            DenyKind::Directory => {
                candidate.starts_with(&self.path)
                    || self
                        .canonical
                        .as_deref()
                        .is_some_and(|denied| candidate.starts_with(denied))
            },
        };

        matches_candidate(path) || canonical.is_some_and(matches_candidate)
    }
}

/// The single owner of resolved filesystem grants.
///
/// The OS sandbox and the in-process file tools intentionally see different
/// projections of this value: Bash may write toolchain caches and can read
/// OS-only paths, while the in-process tools keep toolchain paths read-only and
/// apply the credential deny list. Building both views from one value prevents
/// the two enforcement layers from drifting.
#[derive(Clone, Debug)]
pub(super) struct PathGrants {
    /// Paths the in-process tools may read and write.
    pub read_write: Vec<PathBuf>,
    /// Paths the in-process tools may read but not write.
    pub read_only: Vec<PathBuf>,
    /// Toolchain paths Bash may write under `WorkspaceWrite`.
    pub sandbox_writable: Vec<PathBuf>,
    /// OS-only read paths excluded from the in-process projection.
    pub sandbox_only_read: Vec<PathBuf>,
    /// Credential paths that win over every ordinary grant.
    pub denied: Vec<DeniedPath>,
    /// User-granted directories used for Bash `cwd` admission.
    pub user_grants: Vec<PathBuf>,
    /// The policy this value was built for.
    pub policy: SandboxPolicy,
}

impl PathGrants {
    /// Build the grant value from the resolved path inputs.
    ///
    /// `additional_dirs` are read-only grants from `--add-dir` and
    /// `[sandbox].read_only`; `settings_dirs` are read-write grants from
    /// `settings.toml` and `directories`; `skill_dirs` are read-only grants.
    /// `home` is the resolved account home, or `None` when it is unavailable.
    pub(super) fn build(
        policy: SandboxPolicy,
        cwd: &Path,
        temp_dirs: &[PathBuf],
        additional_dirs: &[PathBuf],
        settings_dirs: &[PathBuf],
        skill_dirs: &[PathBuf],
        home: Option<&Path>,
    ) -> Self {
        let linked_worktree_dirs = resolve_linked_worktree_dirs(cwd);
        Self::build_with_linked_dirs(
            policy,
            cwd,
            temp_dirs,
            &linked_worktree_dirs,
            additional_dirs,
            settings_dirs,
            skill_dirs,
            home,
        )
    }

    /// Build the grant value from resolved inputs, including linked-worktree
    /// Git directories resolved once by the caller.
    ///
    /// [`Self::build`] resolves the linked-worktree directories itself. The
    /// diagnostic report resolves them once so its source list and the
    /// effective sets describe the same paths.
    #[expect(
        clippy::too_many_arguments,
        reason = "the grant inputs are the security-relevant path sources"
    )]
    pub(super) fn build_with_linked_dirs(
        policy: SandboxPolicy,
        cwd: &Path,
        temp_dirs: &[PathBuf],
        linked_worktree_dirs: &[PathBuf],
        additional_dirs: &[PathBuf],
        settings_dirs: &[PathBuf],
        skill_dirs: &[PathBuf],
        home: Option<&Path>,
    ) -> Self {
        let mut read_write = vec![cwd.to_path_buf()];
        read_write.extend(temp_dirs.iter().cloned());
        read_write.extend(linked_worktree_dirs.iter().cloned());
        push_dirs_with_canonical(&mut read_write, settings_dirs);
        let read_write = deduplicated_with_canonical(&read_write);

        let mut sandbox_writable = Vec::new();
        if let Some(home) = home {
            Self::extend_with_toolchain_paths(&mut sandbox_writable, home);
        }
        let sandbox_writable = deduplicated_with_canonical(&sandbox_writable);

        let (mut read_only, sandbox_only_read) = Self::system_read_paths(home);
        push_dirs_with_canonical(&mut read_only, additional_dirs);
        push_dirs_with_canonical(&mut read_only, skill_dirs);
        read_only.extend(sandbox_writable.iter().cloned());
        let read_only = dedup_vec(read_only);

        let user_grants = dedup_vec({
            let mut grants = Vec::new();
            push_dirs_with_canonical(&mut grants, settings_dirs);
            push_dirs_with_canonical(&mut grants, additional_dirs);
            push_dirs_with_canonical(&mut grants, skill_dirs);
            grants
        });

        let (read_write, read_only, sandbox_writable) = if policy == SandboxPolicy::ReadOnly {
            Self::demote_read_write(read_write, temp_dirs, read_only)
        } else {
            (read_write, read_only, sandbox_writable)
        };

        Self {
            read_write,
            read_only,
            sandbox_writable,
            sandbox_only_read,
            denied: Self::denied_paths(home),
            user_grants,
            policy,
        }
    }

    /// Build the grant value against the current process home directory.
    pub(super) fn build_with_home_from_env(
        policy: SandboxPolicy,
        cwd: &Path,
        temp_dirs: &[PathBuf],
        additional_dirs: &[PathBuf],
        settings_dirs: &[PathBuf],
        skill_dirs: &[PathBuf],
    ) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        Self::build(
            policy,
            cwd,
            temp_dirs,
            additional_dirs,
            settings_dirs,
            skill_dirs,
            home.as_deref(),
        )
    }

    /// Whether `path` is a denied credential path.
    ///
    /// The lexical path and its canonical form are both checked, so a symlink
    /// from a granted directory to a denied file still fails closed.
    pub(super) fn is_denied(&self, path: &Path) -> bool {
        let canonical = canonicalize_allow_missing(path);
        self.denied
            .iter()
            .any(|denied| denied.matches(path, canonical.as_deref()))
    }

    fn denied_paths(home: Option<&Path>) -> Vec<DeniedPath> {
        let Some(home) = home else {
            return Vec::new();
        };
        DENIED_HOME_PATHS
            .iter()
            .map(|(relative, kind)| DeniedPath::new(home.join(relative), *kind))
            .collect()
    }

    /// Swap the writable/read-only split for the read-only policy.
    ///
    /// Temp directories stay writable; every other read-write grant is demoted
    /// into the read-only set. Toolchain paths are already read-only to the
    /// in-process tools and become read-only to Bash as well.
    fn demote_read_write(
        read_write: Vec<PathBuf>,
        temp_dirs: &[PathBuf],
        mut read_only: Vec<PathBuf>,
    ) -> (Vec<PathBuf>, Vec<PathBuf>, Vec<PathBuf>) {
        let mut temp_set = std::collections::HashSet::new();
        for dir in temp_dirs {
            temp_set.insert(dir.clone());
            if let Ok(canonical) = dir.canonicalize() {
                temp_set.insert(canonical);
            }
        }

        let writable = deduplicated_with_canonical(temp_dirs);
        for path in read_write {
            if temp_set.contains(&path) {
                continue;
            }
            read_only.push(path.clone());
            if let Ok(canonical) = path.canonicalize() {
                read_only.push(canonical);
            }
        }

        (writable, dedup_vec(read_only), Vec::new())
    }

    /// Split the built-in read paths into the in-process grant set and the
    /// Linux-only OS projection. Landlock grants `/proc`, `/dev`, and `/sys`,
    /// but `/proc/self/environ` must never become readable through `Read`.
    fn system_read_paths(home: Option<&Path>) -> (Vec<PathBuf>, Vec<PathBuf>) {
        let paths = Self::get_read_execute_paths(home);

        #[cfg(target_os = "linux")]
        {
            let mut in_process = Vec::new();
            let mut os_only = Vec::new();
            for path in paths {
                if path == Path::new("/dev")
                    || path == Path::new("/proc")
                    || path == Path::new("/sys")
                {
                    os_only.push(path);
                } else {
                    in_process.push(path);
                }
            }
            (in_process, os_only)
        }

        #[cfg(not(target_os = "linux"))]
        {
            (paths, Vec::new())
        }
    }

    /// Get built-in paths that need read and execute access.
    fn get_read_execute_paths(home: Option<&Path>) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        #[cfg(target_os = "macos")]
        {
            paths.extend([
                PathBuf::from("/usr"),
                PathBuf::from("/bin"),
                PathBuf::from("/sbin"),
                PathBuf::from("/Library"),
                PathBuf::from("/System/Library"),
                // macOS exposes command-line shims and sealed-system content
                // through Cryptex paths. npm's script runner may touch this
                // path while resolving its shell, even when the final shell is
                // /bin/sh.
                PathBuf::from("/System/Cryptexes"),
                PathBuf::from("/Applications"),
                PathBuf::from("/opt/homebrew"),
                PathBuf::from("/opt/local"),
                PathBuf::from("/etc"),
                PathBuf::from("/private/etc"),
                PathBuf::from("/private/var"),
                PathBuf::from("/dev"),
                PathBuf::from("/var"),
            ]);
        }

        #[cfg(target_os = "linux")]
        {
            paths.extend([
                PathBuf::from("/usr"),
                PathBuf::from("/bin"),
                PathBuf::from("/sbin"),
                PathBuf::from("/lib"),
                PathBuf::from("/lib64"),
                PathBuf::from("/etc/alternatives"),
                PathBuf::from("/snap"),
                PathBuf::from("/etc"),
                PathBuf::from("/dev"),
                PathBuf::from("/proc"),
                PathBuf::from("/sys"),
            ]);
        }

        // Git configuration (read-only).
        if let Some(home) = home {
            paths.extend([home.join(".config/git"), home.join(".gitattributes")]);
        }

        paths.into_iter().filter(|p| p.exists()).collect()
    }
}

impl SandboxConfig {
    /// Build a sandbox configuration for the current context from its cached
    /// grant table.
    pub fn build(context: &ToolContext) -> Self {
        Self::from_grants(context.path_grants())
    }

    /// Project the shared grants into the Bash-facing path classes.
    ///
    /// The OS sandbox does not apply the in-process credential deny list:
    /// Bash remains governed by the OS sandbox and the external gate, and its
    /// diagnostics must describe what the OS profile allows.
    pub(super) fn from_grants(grants: &PathGrants) -> Self {
        let mut writable = grants.read_write.clone();
        writable.extend(grants.sandbox_writable.iter().cloned());
        let writable = dedup_vec(writable);

        let sandbox_writable: std::collections::HashSet<&Path> = grants
            .sandbox_writable
            .iter()
            .map(PathBuf::as_path)
            .collect();
        let mut read_execute: Vec<PathBuf> = grants
            .read_only
            .iter()
            .filter(|path| !sandbox_writable.contains(path.as_path()))
            .cloned()
            .collect();
        read_execute.extend(grants.sandbox_only_read.iter().cloned());
        let read_execute = dedup_vec(read_execute);

        let config = Self {
            writable,
            read_execute,
            #[cfg(target_os = "macos")]
            policy: grants.policy,
            user_grants: grants.user_grants.clone(),
        };

        // On unsupported platforms, no sandbox strategy reads the config
        // fields. Explicit reads prevent dead_code warnings without relying
        // on #[expect(dead_code)], which is reserved for serde fields.
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = &config.writable;
            let _ = &config.read_execute;
        }

        config
    }
}

impl PathGrants {
    /// Return the built-in toolchain paths before deduplication.
    ///
    /// The diagnostic report lists these as their own source. Keeping the
    /// expansion in one function prevents the report from drifting from the
    /// paths a run grants Bash.
    fn toolchain_paths(home: &Path) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        Self::extend_with_toolchain_paths(&mut paths, home);
        paths
    }

    /// Extend the writable paths with directories needed by common toolchains
    /// and CLIs.
    ///
    /// Patterned after the Safehouse macOS profile (see
    /// `examples/safehouse.custom.generated.sb`). The goal is for cake to
    /// work across the languages, package managers, and runtime managers a
    /// coding agent typically encounters, without users having to add per-tool
    /// `--add-dir` flags.
    #[expect(
        clippy::too_many_lines,
        reason = "toolchain path discovery covers many language ecosystems"
    )]
    fn extend_with_toolchain_paths(writable: &mut Vec<PathBuf>, home: &Path) {
        // Rust: cargo + rustup (env var overrides honored).
        let cargo_home =
            std::env::var_os("CARGO_HOME").map_or_else(|| home.join(".cargo"), PathBuf::from);
        let rustup_home =
            std::env::var_os("RUSTUP_HOME").map_or_else(|| home.join(".rustup"), PathBuf::from);
        writable.push(cargo_home);
        writable.push(rustup_home);
        writable.extend([
            home.join(".config/cargo"),
            home.join(".cache/cargo"),
            home.join(".cache/sccache"),
        ]);

        // Pre-commit hook caches.
        writable.push(home.join(".cache/prek"));

        // SCM CLIs: gh and glab. This shared list is the only owner; platform
        // strategies translate these paths like every other filesystem grant.
        writable.extend(SCM_CLI_PATHS.iter().map(|path| home.join(path)));

        // Cross-language runtime managers.
        writable.extend([
            // mise
            home.join(".config/mise"),
            home.join(".local/share/mise"),
            home.join(".local/state/mise"),
            home.join(".cache/mise"),
            home.join(".mise.toml"),
            // volta
            home.join(".volta"),
            // asdf
            home.join(".asdf"),
            home.join(".config/asdf"),
            home.join(".local/share/asdf"),
            home.join(".local/state/asdf"),
            home.join(".cache/asdf"),
            home.join(".asdfrc"),
            home.join(".tool-versions"),
            // proto
            home.join(".proto"),
            home.join(".prototools"),
            // pkgx
            home.join(".pkgx"),
            home.join(".local/share/pkgx"),
            home.join(".cache/pkgx"),
        ]);

        // Node.js ecosystem.
        writable.extend([
            // version managers
            home.join(".nvm"),
            home.join(".fnm"),
            home.join(".local/share/fnm"),
            home.join(".local/state/fnm"),
            home.join(".local/state/fnm_multishells"),
            home.join(".cache/fnm"),
            // npm
            home.join(".npm"),
            home.join(".config/npm"),
            home.join(".cache/npm"),
            home.join(".cache/node"),
            home.join(".npmrc"),
            // configstore (used by npm and many node CLIs)
            home.join(".config/configstore"),
            // node-gyp
            home.join(".node-gyp"),
            home.join(".cache/node-gyp"),
            // pnpm
            home.join(".config/pnpm"),
            home.join(".pnpm-state"),
            home.join(".pnpm-store"),
            home.join(".local/share/pnpm"),
            home.join(".local/state/pnpm"),
            // yarn (classic + modern)
            home.join(".yarn"),
            home.join(".yarnrc"),
            home.join(".yarnrc.yml"),
            home.join(".config/yarn"),
            home.join(".cache/yarn"),
            // corepack
            home.join(".cache/node/corepack"),
            // turborepo
            home.join(".cache/turbo"),
            // browser automation / test runners (Linux/XDG locations)
            home.join(".cache/puppeteer"),
            home.join(".cache/prisma"),
        ]);

        // Bun.
        writable.extend([
            home.join(".bun"),
            home.join(".cache/bun"),
            home.join(".local/state/bun"),
            home.join(".local/share/bun"),
            home.join(".bunfig.toml"),
            home.join(".config/bunfig.toml"),
        ]);

        // Deno.
        writable.extend([home.join(".deno"), home.join(".cache/deno")]);

        // Go (env var overrides honored).
        let gopath = std::env::var_os("GOPATH").map_or_else(|| home.join("go"), PathBuf::from);
        writable.push(gopath);
        let gomodcache = std::env::var_os("GOMODCACHE").map(PathBuf::from);
        if let Some(p) = gomodcache {
            writable.push(p);
        }
        let gocache = std::env::var_os("GOCACHE").map(PathBuf::from);
        if let Some(p) = gocache {
            writable.push(p);
        }
        writable.extend([
            home.join(".cache/go-build"),
            home.join(".config/go"),
            home.join(".cache/golangci-lint"),
            home.join(".config/golangci-lint"),
            home.join(".local/share/go"),
            home.join(".goenv"),
            home.join(".cache/gopls"),
        ]);

        // Java / JVM toolchains (Maven, Gradle, SBT, Coursier, jenv, sdkman).
        writable.extend([
            home.join(".m2"),
            home.join(".gradle"),
            home.join(".ivy2"),
            home.join(".sbt"),
            home.join(".jenv"),
            home.join(".sdkman"),
            home.join(".cache/coursier"),
            home.join(".coursier"),
            home.join(".java"),
            home.join(".mavenrc"),
        ]);

        // Python: uv, pip, pipx, poetry, pdm, conda, hatch, ruff, mypy, jupyter, pyenv, etc.
        writable.extend([
            home.join(".local/bin/uv"),
            home.join(".local/bin/uvx"),
            home.join(".local/share/uv"),
            home.join(".local/state/uv"),
            home.join(".local/pipx"),
            home.join(".cache/uv"),
            home.join(".config/uv"),
            home.join(".cache/pip"),
            home.join(".config/pip"),
            home.join(".cache/pypoetry"),
            home.join(".config/pypoetry"),
            home.join(".local/share/pypoetry"),
            home.join(".cache/pdm"),
            home.join(".config/pdm"),
            home.join(".local/share/pdm"),
            home.join(".cache/pre-commit"),
            home.join(".cache/mypy"),
            home.join(".cache/ruff"),
            home.join(".virtualenvs"),
            home.join(".ipython"),
            home.join(".jupyter"),
            home.join(".pyenv"),
            home.join(".pypirc"),
            home.join(".python_history"),
            // conda / miniconda / miniforge
            home.join(".conda"),
            home.join("miniconda3"),
            home.join("miniforge3"),
            home.join(".condarc"),
            // hatch
            home.join(".cache/hatch"),
            home.join(".config/hatch"),
            home.join(".local/share/hatch"),
        ]);

        // Ruby: rbenv, rvm, gem, bundler, etc.
        writable.extend([
            home.join(".rbenv"),
            home.join(".rvm"),
            home.join(".rubies"),
            home.join(".bundle"),
            home.join(".gem"),
            home.join(".cache/bundler"),
            home.join(".cache/rubygems"),
            home.join(".gemrc"),
            home.join(".irbrc"),
            home.join(".irb_history"),
            home.join(".pryrc"),
            home.join(".pry_history"),
        ]);

        // Perl: perlbrew, plenv, cpan(m), local::lib.
        writable.extend([
            home.join(".perlbrew"),
            home.join(".plenv"),
            home.join(".cpan"),
            home.join(".cpanm"),
            home.join(".perl-cpm"),
            home.join("perl5"),
            home.join(".local/lib/perl5"),
            home.join(".cpanrc"),
        ]);

        // PHP / Composer.
        writable.extend([
            home.join(".composer"),
            home.join(".config/composer"),
            home.join(".cache/composer"),
            home.join(".local/share/composer"),
            home.join(".phpenv"),
            home.join(".config/php"),
            home.join(".cache/php"),
            home.join(".pearrc"),
        ]);

        // macOS-specific cache and application-support locations under ~/Library.
        #[cfg(target_os = "macos")]
        writable.extend([
            // Rust
            home.join("Library/Caches/cargo"),
            // sccache's default disk cache on macOS (config lives in
            // Library/Application Support/Mozilla.sccache below).
            home.join("Library/Caches/Mozilla.sccache"),
            home.join("Library/Caches/sccache"),
            home.join("Library/Application Support/Mozilla.sccache"),
            // Runtime managers
            home.join("Library/Caches/mise"),
            home.join("Library/Caches/asdf"),
            home.join("Library/Caches/pkgx"),
            home.join("Library/Packages"),
            // Node.js ecosystem
            home.join("Library/Caches/fnm"),
            home.join("Library/Application Support/fnm"),
            home.join("Library/Caches/npm"),
            home.join("Library/pnpm"),
            home.join("Library/Caches/pnpm"),
            home.join("Library/Preferences/pnpm"),
            home.join("Library/Caches/Yarn"),
            home.join("Library/Caches/node/corepack"),
            home.join("Library/Caches/ms-playwright"),
            home.join("Library/Caches/Cypress"),
            home.join("Library/Caches/typescript"),
            home.join("Library/Caches/prisma-nodejs"),
            home.join("Library/Caches/checkpoint-nodejs"),
            home.join("Library/Caches/turbo"),
            home.join("Library/Application Support/turborepo"),
            // Bun
            home.join("Library/Caches/bun"),
            // Deno
            home.join("Library/Caches/deno"),
            // Go
            home.join("Library/Caches/go-build"),
            home.join("Library/Caches/golangci-lint"),
            // Java
            home.join("Library/Java"),
            home.join("Library/Application Support/Coursier"),
            home.join("Library/Caches/Coursier"),
            // Python
            home.join("Library/Caches/uv"),
            home.join("Library/Caches/pip"),
            home.join("Library/Caches/pypoetry"),
            // Ruby
            home.join("Library/Caches/bundle"),
            // Perl
            home.join("Library/Caches/cpanm"),
            // PHP
            home.join("Library/Caches/composer"),
        ]);
    }
}

impl SandboxConfig {
    /// Whether `path` is inside any ordinary grant that permits reading or
    /// execution under this config. Use [`Self::is_path_writable`] when the
    /// operation being audited is a write.
    ///
    /// A path is allowed when it is inside a writable directory
    /// (read/write/execute) or a read-and-execute directory (read/execute).
    /// Both the lexical path and its canonical form are checked so symlinked
    /// paths (e.g. `/tmp` -> `/private/tmp` on macOS) match their grant.
    ///
    /// This is a best-effort hint for naming a missing directory grant after
    /// a sandbox denial; it is not an exact model of Seatbelt/Landlock
    /// enforcement. A sibling of a `[sandbox].read_only` file grant is still
    /// reported as allowed here even though the OS denies it, because the
    /// command token resolves to a path outside the granted file.
    pub(super) fn is_path_allowed(&self, path: &Path) -> bool {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.writable
            .iter()
            .chain(self.read_execute.iter())
            .any(|allowed| path.starts_with(allowed) || canonical.starts_with(allowed))
    }

    /// Whether `path` is inside a read-write grant. Read-only grants must not
    /// suppress a write-denial audit label even though they allow reads and
    /// execution. Standard device nodes are included because platform profiles
    /// grant them separately from the ordinary path classes.
    pub(super) fn is_path_writable(&self, path: &Path) -> bool {
        if is_standard_writable_device(path) {
            return true;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.writable
            .iter()
            .any(|allowed| path.starts_with(allowed) || canonical.starts_with(allowed))
    }

    /// Whether `path` may be selected as a Bash working directory: it is inside
    /// a read-write grant or inside a directory the user granted directly with
    /// `--add-dir`, a `[sandbox]` settings entry, or a skill directory.
    ///
    /// Under the read-only policy the demoted settings directories are still
    /// selectable, because selecting a directory is not a write and read-only
    /// work happens in read-only directories. The built-in read-and-execute
    /// system paths are not selectable; see `user_grants`.
    ///
    /// Like [`Self::is_path_writable`], this is a best-effort predicate that
    /// compares path prefixes over this config's own lists: it is coarse enough
    /// to admit a directory the OS still denies. That is acceptable here
    /// because it gates admission only --- the OS governs every access from the
    /// selected directory, so a wrong admission degrades the error message and
    /// never widens the command's authority.
    pub(super) fn is_path_selectable(&self, path: &Path) -> bool {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.writable
            .iter()
            .chain(self.user_grants.iter())
            .any(|granted| path.starts_with(granted) || canonical.starts_with(granted))
    }
}

/// Device paths with explicit write access in the platform sandbox profiles.
fn is_standard_writable_device(path: &Path) -> bool {
    [
        "/dev/null",
        "/dev/tty",
        "/dev/ptmx",
        "/dev/dtracehelper",
        "/dev/stdout",
        "/dev/stderr",
    ]
    .iter()
    .any(|candidate| path == Path::new(candidate))
        || path.starts_with(Path::new("/dev/fd"))
}

/// Push each existing directory and its canonical form into the target vector.
/// Both original and canonical paths are pushed so sandbox rules match
/// regardless of symlink resolution (e.g., /tmp and /private/tmp on macOS).
fn push_dirs_with_canonical(target: &mut Vec<PathBuf>, dirs: &[PathBuf]) {
    for dir in dirs {
        if dir.exists() {
            target.push(dir.clone());
            if let Ok(canonical) = dir.canonicalize() {
                target.push(canonical);
            }
        }
    }
}

/// Include both original and canonical paths, deduplicated.
/// This ensures sandbox rules match regardless of symlink resolution
/// (e.g., /tmp and /private/tmp on macOS).
fn deduplicated_with_canonical(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for p in paths {
        if seen.insert(p.clone()) {
            result.push(p.clone());
        }
        if let Ok(canonical) = p.canonicalize()
            && seen.insert(canonical.clone())
        {
            result.push(canonical);
        }
    }

    result
}

/// Deduplicate a path vector preserving first-seen order.
fn dedup_vec(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    paths.retain(|p| seen.insert(p.clone()));
    paths
}

/// Guard returned by [`SandboxStrategy::apply`] that keeps sandbox resources
/// (e.g., temporary Seatbelt profile files on macOS) alive for the duration
/// of command execution. When the guard is dropped, any held resources are
/// cleaned up deterministically.
pub(super) struct SandboxGuard {
    _inner: Option<Box<dyn std::any::Any + Send>>,
}

impl SandboxGuard {
    /// Create an empty guard (no cleanup needed).
    #[cfg(target_os = "linux")]
    pub(super) const fn empty() -> Self {
        Self { _inner: None }
    }

    /// Create a guard that owns a cleanup resource.
    #[cfg(target_os = "macos")]
    pub(super) fn new<T: Send + 'static>(resource: T) -> Self {
        Self {
            _inner: Some(Box::new(resource)),
        }
    }
}

/// Platform-specific sandbox strategy trait
pub(super) trait SandboxStrategy: Send + Sync {
    /// Wrap the given Command with sandbox restrictions.
    ///
    /// Returns a [`SandboxGuard`] that must be kept alive for the duration
    /// of command execution so any cleanup resources (e.g., temp profile
    /// files on macOS) are not removed prematurely.
    ///
    /// On macOS: replace the command with `sandbox-exec -f <profile> bash -c <cmd>`
    /// On Linux: apply Landlock rules before spawning
    fn apply(
        &self,
        command: &mut tokio::process::Command,
        config: &SandboxConfig,
    ) -> Result<SandboxGuard, String>;
}

/// Detect the appropriate sandbox strategy for the current platform.
///
/// If sandboxing is expected on a supported platform but cannot be enforced,
/// return an error instead of silently falling back to unsandboxed execution.
/// The exception is a macOS process already constrained by Seatbelt, where
/// applying a nested profile is not permitted and the inherited sandbox remains
/// the enforcement boundary.
// Linux detection is infallible, but macOS and unsupported platforms can fail closed.
#[cfg_attr(
    target_os = "linux",
    expect(
        clippy::unnecessary_wraps,
        reason = "shared cross-platform API also represents macOS detection failures"
    )
)]
pub(super) fn detect_platform() -> Result<Option<Box<dyn SandboxStrategy>>, String> {
    #[cfg(target_os = "macos")]
    {
        detect_macos_platform(
            std::path::Path::new("/usr/bin/sandbox-exec").exists(),
            MacOsSandbox::can_apply_profile(),
            MacOsSandbox::profile_probe_failure(),
        )
    }

    #[cfg(target_os = "linux")]
    {
        tracing::debug!("Using Linux Landlock LSM for filesystem sandboxing");
        Ok(Some(Box::new(LandlockSandbox)))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(format!(
            "sandbox unavailable: no supported OS sandbox is available for platform '{}'. \
             Set CAKE_SANDBOX=off to run Bash commands without filesystem sandboxing.",
            std::env::consts::OS
        ))
    }
}

#[cfg(target_os = "macos")]
fn detect_macos_platform(
    sandbox_exec_exists: bool,
    can_apply_profile: bool,
    profile_probe_failure: Option<&str>,
) -> Result<Option<Box<dyn SandboxStrategy>>, String> {
    if !sandbox_exec_exists {
        return Err(
            "macOS sandbox unavailable: /usr/bin/sandbox-exec was not found. \
             Set CAKE_SANDBOX=off to run Bash commands without filesystem sandboxing."
                .to_string(),
        );
    }

    if !can_apply_profile {
        if profile_probe_failure
            .is_some_and(|failure| failure.contains("sandbox_apply: Operation not permitted"))
        {
            tracing::warn!(
                "macOS sandbox-exec cannot apply a nested Seatbelt profile; relying on \
                 inherited Seatbelt enforcement and running Bash commands without cake's \
                 filesystem sandbox"
            );
            return Ok(None);
        }

        let details = profile_probe_failure
            .map(|failure| format!(" Probe failure: {failure}."))
            .unwrap_or_default();
        return Err(format!(
            "macOS sandbox unavailable: sandbox-exec could not apply a Seatbelt profile \
             in this process context.{details} Set CAKE_SANDBOX=off to run Bash commands \
             without filesystem sandboxing."
        ));
    }

    tracing::debug!("Using macOS sandbox-exec for filesystem sandboxing");
    Ok(Some(Box::new(MacOsSandbox)))
}

/// Check if sandboxing should be disabled via environment variable
pub(super) fn is_sandbox_disabled() -> bool {
    match std::env::var("CAKE_SANDBOX").as_deref() {
        Ok("off" | "0" | "false" | "no") => {
            tracing::warn!("Sandbox disabled via CAKE_SANDBOX environment variable");
            true
        },
        Ok("warn") => {
            tracing::warn!("Sandbox 'warn' mode requested; falling back to enforce mode");
            false
        },
        _ => false,
    }
}

/// Resolve git directories for a linked worktree, if the workspace
/// `cwd/.git` is a file (created by `git worktree add`) rather than a
/// directory.
///
/// Returns the per-worktree gitdir and the common git dir that git needs
/// to operate. The caller adds these to the sandbox writable set so git
/// commands (status, add, commit, etc.) can access HEAD, index, refs,
/// objects, and config outside the workspace subtree.
///
/// If `.git` is not a file, returns an empty vec (silent no-op). If the
/// gitdir cannot be parsed or the target directories don't exist, logs a
/// warning and returns an empty vec so the session can start (the user
/// will see the warning before the first git command fails).
pub fn resolve_linked_worktree_dirs(cwd: &Path) -> Vec<PathBuf> {
    let git_file = cwd.join(".git");
    if !git_file.is_file() {
        return Vec::new();
    }

    let content = match std::fs::read_to_string(&git_file) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Cannot read .git file at {}: {}. Git commands inside a linked worktree may fail under the sandbox.",
                git_file.display(),
                e
            );
            return Vec::new();
        },
    };

    // Parse the gitdir: line (must be absolute or relative to the .git file's directory)
    let gitdir_line = content.lines().find(|line| {
        let trimmed = line.trim();
        trimmed.starts_with("gitdir:")
    });

    let gitdir_raw = if let Some(line) = gitdir_line {
        // Extract everything after "gitdir:" and trim whitespace
        let path_str = line
            .strip_prefix("gitdir: ")
            .or_else(|| line.strip_prefix("gitdir:"))
            .map_or("", str::trim);
        if path_str.is_empty() {
            tracing::warn!(
                ".git file at {} contains a gitdir: line with no path. Git commands inside a linked worktree may fail under the sandbox.",
                git_file.display()
            );
            return Vec::new();
        }
        path_str.to_string()
    } else {
        tracing::warn!(
            ".git file at {} does not contain a gitdir: line. Git commands inside a linked worktree may fail under the sandbox.",
            git_file.display()
        );
        return Vec::new();
    };

    // Resolve relative paths against the workspace (the .git file's directory)
    let gitdir_path = if Path::new(&gitdir_raw).is_relative() {
        cwd.join(&gitdir_raw)
    } else {
        PathBuf::from(&gitdir_raw)
    };

    // Canonicalize to resolve any symlinks /.. components
    let per_worktree_dir = match gitdir_path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                "Linked worktree gitdir at {} (from .git file at {}) does not exist or cannot be resolved: {}. Git commands may fail under the sandbox.",
                gitdir_path.display(),
                git_file.display(),
                e
            );
            return Vec::new();
        },
    };

    // Determine the common git dir from commondir if present, or derive it
    // from the per-worktree path structure.
    let Some(common_dir) = resolve_worktree_commondir(&per_worktree_dir) else {
        tracing::warn!(
            "Cannot determine common git directory for linked worktree at {} (from .git file at {}). \
             Git commands may fail under the sandbox.",
            per_worktree_dir.display(),
            git_file.display()
        );
        return Vec::new();
    };

    vec![per_worktree_dir, common_dir]
}

/// Resolve the common git directory from a per-worktree gitdir.
///
/// Reads the `commondir` file (which contains a relative or absolute path to
/// the common directory) and resolves it against the per-worktree gitdir.
///
/// Git always writes a `commondir` file when creating a linked worktree.
/// If the file is missing, empty, or its content cannot be resolved to a
/// real directory, the function logs a warning and returns `None`. This
/// ensures the common directory is resolved from metadata written by git
/// itself, not inferred from directory layout.
fn resolve_worktree_commondir(worktree_gitdir: &Path) -> Option<PathBuf> {
    let commondir_file = worktree_gitdir.join("commondir");
    let content = match std::fs::read_to_string(&commondir_file) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "Cannot read commondir file at {}: {}. \
                 Git commands inside a linked worktree may fail under the sandbox.",
                commondir_file.display(),
                e
            );
            return None;
        },
    };

    let commondir_path = content.trim();
    if commondir_path.is_empty() {
        tracing::warn!(
            "commondir file at {} is empty. \
             Git commands inside a linked worktree may fail under the sandbox.",
            commondir_file.display()
        );
        return None;
    }

    let resolved = if Path::new(commondir_path).is_relative() {
        // commondir is relative to the per-worktree gitdir
        worktree_gitdir.join(commondir_path)
    } else {
        PathBuf::from(commondir_path)
    };

    match resolved.canonicalize() {
        Ok(canonical) => Some(canonical),
        Err(e) => {
            tracing::warn!(
                "commondir path '{}' (from {}) cannot be resolved: {}. \
                 Git commands inside a linked worktree may fail under the sandbox.",
                resolved.display(),
                commondir_file.display(),
                e
            );
            None
        },
    }
}

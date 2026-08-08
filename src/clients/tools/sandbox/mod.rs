//! OS-level sandboxing for the Bash tool
//!
//! Provides filesystem sandboxing using:
//! - macOS: `sandbox-exec` (Seatbelt profile)
//! - Linux: Landlock LSM (kernel 5.13+)
//!
//! The sandbox restricts filesystem access to:
//! - Read-write: current working directory, temp directories
//! - Read-only + exec: system paths (/usr, /bin, /lib, etc.)
//! - Read-only: config/device paths (/etc, /dev/null, etc.)
//! - Deny: everything else

use std::path::{Path, PathBuf};

use crate::clients::tools::ToolContext;

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
/// intermediate output. `DangerFullAccess` skips the sandbox entirely.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum SandboxPolicy {
    /// Read-only: deny writes to the workspace and toolchain dirs.
    ReadOnly,
    /// Read-write workspace with toolchain caches (current default).
    #[default]
    WorkspaceWrite,
    /// No sandbox restrictions.
    DangerFullAccess,
}

/// Resolve the effective sandbox policy from an optional CLI `--sandbox`
/// value and the `CAKE_SANDBOX` environment variable.
///
/// An explicit CLI value always wins. When no flag is passed, `CAKE_SANDBOX=off`
/// (and its aliases) maps to `DangerFullAccess` for backward compatibility;
/// otherwise the policy defaults to `WorkspaceWrite`.
pub fn resolve_sandbox_policy(cli: Option<SandboxPolicy>) -> SandboxPolicy {
    if let Some(policy) = cli {
        return policy;
    }
    if is_sandbox_disabled() {
        SandboxPolicy::DangerFullAccess
    } else {
        SandboxPolicy::WorkspaceWrite
    }
}

// =============================================================================
// Core Types
// =============================================================================

#[derive(Clone, Debug)]
pub(super) struct SandboxConfig {
    /// Directories with read-write access (cwd, temp dirs)
    pub writable: Vec<PathBuf>,
    /// Directories with read-only + execute access (system paths)
    pub system_paths: Vec<PathBuf>,
    /// Directories with read-only access
    pub readable: Vec<PathBuf>,
    /// The sandbox policy this config was built for. Platform strategies use
    /// this to adjust rules that are not derived purely from the path lists
    /// (e.g. macOS SCM CLI read-write grants).
    pub policy: SandboxPolicy,
}

impl SandboxConfig {
    /// Build a sandbox configuration for the current context, honoring the
    /// sandbox policy carried by the tool context.
    pub fn build(context: &ToolContext) -> Self {
        Self::build_with_policy(
            context.sandbox_policy,
            &context.cwd,
            &context.temp_dirs,
            &context.additional_dirs,
            &context.settings_dirs,
            &context.skill_dirs,
        )
    }

    /// Build a sandbox configuration with additional directories and an
    /// explicit sandbox policy.
    ///
    /// `additional_dirs` are added as read-only (from `--add-dir`).
    /// `settings_dirs` are added as read-write (from `settings.toml`).
    /// `skill_dirs` are added as read-only (parent dirs of SKILL.md files).
    pub fn build_with_policy(
        policy: SandboxPolicy,
        cwd: &std::path::Path,
        temp_dirs: &[std::path::PathBuf],
        additional_dirs: &[std::path::PathBuf],
        settings_dirs: &[std::path::PathBuf],
        skill_dirs: &[std::path::PathBuf],
    ) -> Self {
        let mut writable = vec![cwd.to_path_buf()];

        // Add temp directories
        writable.extend(temp_dirs.iter().cloned());

        // If cwd is a linked git worktree (.git is a file pointing elsewhere),
        // add the per-worktree gitdir and common git dir to the writable set
        // so git commands can operate under the sandbox.
        let linked_dirs = resolve_linked_worktree_dirs(cwd);
        writable.extend(linked_dirs);

        // Add user home toolchain and integration paths
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            Self::extend_with_toolchain_paths(&mut writable, &home);
        }

        // Add settings directories from settings.toml as read-write
        push_dirs_with_canonical(&mut writable, settings_dirs);

        // Include both original and canonical paths to handle symlinks
        // (e.g., /tmp → /private/tmp on macOS)
        let writable = deduplicated_with_canonical(&writable);

        let system_paths = Self::get_system_paths();
        let mut readable = Self::get_read_only_paths();

        // Add additional directories from --add-dir flag as read-only
        push_dirs_with_canonical(&mut readable, additional_dirs);

        // Add skill directories as read-only (so scripts like x-fetch.js can execute)
        push_dirs_with_canonical(&mut readable, skill_dirs);

        // Read-only policy: keep only temp directories writable and move the
        // workspace dir, toolchain caches, and settings dirs into the
        // read-only set. Temp directories stay read-write so commands can
        // still produce intermediate output (pipes, mktemp, overflow temp
        // files).
        let (writable, readable) = if policy == SandboxPolicy::ReadOnly {
            Self::partition_read_only(&writable, temp_dirs, readable)
        } else {
            // WorkspaceWrite and DangerFullAccess use the same config; only
            // DangerFullAccess skips applying the sandbox entirely.
            (writable, readable)
        };

        let config = Self {
            writable,
            system_paths,
            readable,
            policy,
        };

        // On unsupported platforms, no sandbox strategy reads the config
        // fields. Explicit reads prevent dead_code warnings without relying
        // on #[expect(dead_code)], which is reserved for serde fields.
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = &config.writable;
            let _ = &config.system_paths;
            let _ = &config.readable;
        }

        config
    }

    /// Move every workspace/toolchain/settings path that is not a temp dir
    /// from `writable` into `readable`, leaving only temp dirs writable.
    fn partition_read_only(
        full_writable: &[std::path::PathBuf],
        temp_dirs: &[std::path::PathBuf],
        mut readable: Vec<PathBuf>,
    ) -> (Vec<PathBuf>, Vec<PathBuf>) {
        // Build the set of temp paths (original and canonical) that must stay
        // writable so a command can still produce intermediate output.
        let mut temp_set = std::collections::HashSet::new();
        for dir in temp_dirs {
            temp_set.insert(dir.clone());
            if let Ok(canonical) = dir.canonicalize() {
                temp_set.insert(canonical);
            }
        }

        // Writable shrinks to temp dirs only (deduplicated with canonical
        // forms), matching the same dedup approach as the workspace-write path.
        let writable_writable: Vec<PathBuf> = temp_dirs.to_vec();
        let writable = deduplicated_with_canonical(&writable_writable);

        // Everything that was writable and is not a temp dir becomes read-only.
        for path in full_writable {
            if temp_set.contains(path) {
                continue;
            }
            readable.push(path.clone());
            if let Ok(canonical) = path.canonicalize() {
                readable.push(canonical);
            }
        }

        (writable, dedup_vec(readable))
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

        // SCM CLIs: gh and glab.
        writable.extend([
            home.join(".config/gh"),
            home.join(".cache/gh"),
            home.join(".local/share/gh"),
            home.join(".local/state/gh"),
            home.join(".config/glab-cli"),
            home.join(".cache/glab-cli"),
            home.join(".local/share/glab-cli"),
            home.join(".local/state/glab-cli"),
        ]);

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

    /// Get system paths that need read + execute access
    fn get_system_paths() -> Vec<PathBuf> {
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
            ]);
        }

        paths.into_iter().filter(|p| p.exists()).collect()
    }

    /// Get paths that need read-only access
    fn get_read_only_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        #[cfg(target_os = "macos")]
        paths.extend([
            PathBuf::from("/etc"),
            PathBuf::from("/private/etc"),
            PathBuf::from("/private/var"),
            PathBuf::from("/dev"),
            PathBuf::from("/var"),
        ]);

        #[cfg(target_os = "linux")]
        paths.extend([
            PathBuf::from("/etc"),
            PathBuf::from("/dev"),
            PathBuf::from("/proc"),
            PathBuf::from("/sys"),
        ]);

        // Git configuration (read-only)
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            paths.extend([home.join(".config/git"), home.join(".gitattributes")]);
        }

        paths.into_iter().filter(|p| p.exists()).collect()
    }
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
    #[expect(
        dead_code,
        reason = "field is read only on Drop; it holds the cleanup resource"
    )]
    inner: Option<Box<dyn std::any::Any + Send>>,
}

impl SandboxGuard {
    /// Create an empty guard (no cleanup needed).
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            dead_code,
            reason = "used only on Linux where LandlockSandbox needs a no-op guard"
        )
    )]
    pub(super) const fn empty() -> Self {
        Self { inner: None }
    }

    /// Create a guard that owns a cleanup resource.
    pub(super) fn new<T: Send + 'static>(resource: T) -> Self {
        Self {
            inner: Some(Box::new(resource)),
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
// Linux detection is infallible, but macOS detection can fail closed.
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
        tracing::warn!(
            "No sandbox available for this platform ({}); bash commands will run unsandboxed",
            std::env::consts::OS
        );
        Ok(None)
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

/// Check whether this process can enforce the platform sandbox.
#[cfg(all(test, target_os = "macos"))]
pub(super) fn can_enforce_platform_sandbox() -> bool {
    std::path::Path::new("/usr/bin/sandbox-exec").exists() && MacOsSandbox::can_apply_profile()
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

#[cfg(test)]
mod tests {
    use crate::clients::tools::ToolContext;
    use crate::clients::tools::sandbox::{SandboxConfig, SandboxPolicy};
    use std::path::{Path, PathBuf};
    #[cfg(target_os = "macos")]
    use std::sync::{Arc, Mutex};

    #[test]
    fn resolve_policy_explicit_cli_wins_over_env() {
        // An explicit CLI policy always wins, even when CAKE_SANDBOX=off.
        temp_env::with_var("CAKE_SANDBOX", Some("off"), || {
            assert_eq!(
                super::resolve_sandbox_policy(Some(SandboxPolicy::ReadOnly)),
                SandboxPolicy::ReadOnly
            );
            assert_eq!(
                super::resolve_sandbox_policy(Some(SandboxPolicy::WorkspaceWrite)),
                SandboxPolicy::WorkspaceWrite
            );
        });
    }

    #[test]
    fn resolve_policy_env_off_maps_to_danger_full_access() {
        temp_env::with_var("CAKE_SANDBOX", Some("off"), || {
            assert_eq!(
                super::resolve_sandbox_policy(None),
                SandboxPolicy::DangerFullAccess
            );
        });
    }

    #[test]
    fn resolve_policy_default_is_workspace_write() {
        temp_env::with_var("CAKE_SANDBOX", None::<&str>, || {
            assert_eq!(
                super::resolve_sandbox_policy(None),
                SandboxPolicy::WorkspaceWrite
            );
        });
    }

    #[test]
    fn read_only_policy_moves_workspace_and_toolchain_to_readable() {
        temp_env::with_var("HOME", Some("/tmp/cake-sandbox-test-home"), || {
            let home = PathBuf::from("/tmp/cake-sandbox-test-home");
            let temp_dir = PathBuf::from("/tmp/cake-sandbox-ro-temp");
            let settings_dir = tempfile::tempdir().unwrap();
            let settings_dir_path = settings_dir.path().to_path_buf();
            let config = SandboxConfig::build_with_policy(
                SandboxPolicy::ReadOnly,
                Path::new("/workspace"),
                std::slice::from_ref(&temp_dir),
                &[],
                std::slice::from_ref(&settings_dir_path),
                &[],
            );

            // The workspace directory must not be writable under read-only.
            assert!(
                !config.writable.contains(&PathBuf::from("/workspace")),
                "read-only policy must not grant writes to the workspace"
            );
            assert!(
                config.readable.contains(&PathBuf::from("/workspace")),
                "read-only policy must grant reads to the workspace"
            );

            // A representative toolchain path must be readable, not writable.
            // Use `.bun` which has no env-var override, unlike `CARGO_HOME`.
            let bun = home.join(".bun");
            assert!(
                !config.writable.contains(&bun),
                "read-only policy must not grant writes to {}",
                bun.display()
            );
            assert!(
                config.readable.contains(&bun),
                "read-only policy must grant reads to {}",
                bun.display()
            );

            // sccache's cache dir must be read-only, not writable, under the
            // read-only policy (covers the Linux default `~/.cache/sccache` and
            // the macOS default `~/Library/Caches/Mozilla.sccache`).
            let sccache_cache = home.join(".cache/sccache");
            assert!(
                !config.writable.contains(&sccache_cache),
                "read-only policy must not grant writes to {}",
                sccache_cache.display()
            );
            assert!(
                config.readable.contains(&sccache_cache),
                "read-only policy must grant reads to {}",
                sccache_cache.display()
            );

            // Temp directories stay writable so commands can produce output.
            assert!(
                config.writable.contains(&temp_dir),
                "read-only policy must keep temp dirs writable"
            );

            // Settings dirs ([sandbox].writable and directories) demote to
            // read-only under the read-only policy, matching workspace and
            // toolchain paths.
            assert!(
                !config.writable.contains(&settings_dir_path),
                "read-only policy must not grant writes to {}",
                settings_dir_path.display()
            );
            assert!(
                config.readable.contains(&settings_dir_path),
                "read-only policy must grant reads to {}",
                settings_dir_path.display()
            );
        });
    }

    #[test]
    fn workspace_write_policy_keeps_workspace_writable() {
        temp_env::with_var("HOME", Some("/tmp/cake-sandbox-test-home"), || {
            let config = SandboxConfig::build_with_policy(
                SandboxPolicy::WorkspaceWrite,
                Path::new("/workspace"),
                &[],
                &[],
                &[],
                &[],
            );
            assert!(
                config.writable.contains(&PathBuf::from("/workspace")),
                "workspace-write policy must keep the workspace writable"
            );
        });
    }

    #[test]
    fn build_allows_fnm_runtime_manager_paths() {
        // Pin HOME via temp_env so this test serializes with other tests that
        // mutate HOME (e.g. test_profile_escapes_home_based_paths). Without
        // the lock, parallel tests can swap HOME mid-build and break the
        // expected paths below.
        temp_env::with_var("HOME", Some("/tmp/cake-sandbox-test-home"), || {
            let home = PathBuf::from("/tmp/cake-sandbox-test-home");
            let context = ToolContext::with_temp_dirs(
                PathBuf::from("/workspace"),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            let config = SandboxConfig::build(&context);

            for expected in [
                home.join(".fnm"),
                home.join(".local/share/fnm"),
                home.join(".local/state/fnm"),
                home.join(".local/state/fnm_multishells"),
                home.join(".cache/fnm"),
            ] {
                assert!(
                    config.writable.contains(&expected),
                    "expected read-write sandbox access for {}",
                    expected.display()
                );
            }

            #[cfg(target_os = "macos")]
            for expected in [
                home.join("Library/Caches/fnm"),
                home.join("Library/Application Support/fnm"),
            ] {
                assert!(
                    config.writable.contains(&expected),
                    "expected read-write sandbox access for {}",
                    expected.display()
                );
            }
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_system_paths_include_cryptexes() {
        let paths = SandboxConfig::get_system_paths();

        if PathBuf::from("/System/Cryptexes").exists() {
            assert!(
                paths.contains(&PathBuf::from("/System/Cryptexes")),
                "expected read access for macOS Cryptex command resolution paths"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_detection_fails_closed_when_sandbox_exec_missing() {
        let Err(error) = super::detect_macos_platform(false, true, None) else {
            panic!("missing sandbox-exec must fail closed");
        };

        assert!(error.contains("macOS sandbox unavailable"));
        assert!(error.contains("/usr/bin/sandbox-exec was not found"));
        assert!(error.contains("CAKE_SANDBOX=off"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_detection_falls_back_for_nested_seatbelt_failure_and_warns() {
        #[derive(Clone)]
        struct LogWriter(Arc<Mutex<Vec<u8>>>);

        impl std::io::Write for LogWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let logs = Arc::new(Mutex::new(Vec::new()));
        let log_writer = LogWriter(Arc::clone(&logs));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .without_time()
            .with_writer(move || log_writer.clone())
            .finish();

        let strategy = tracing::subscriber::with_default(subscriber, || {
            super::detect_macos_platform(
                true,
                false,
                Some(
                    "sandbox-exec exited with exit status: 1; stderr: sandbox-exec: \
                     sandbox_apply: Operation not permitted",
                ),
            )
        })
        .unwrap();

        assert!(strategy.is_none());
        let log_output = String::from_utf8(logs.lock().unwrap().clone()).unwrap();
        assert!(log_output.contains("cannot apply a nested Seatbelt profile"));
        assert!(log_output.contains("relying on inherited Seatbelt enforcement"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_detection_fails_closed_for_non_nested_probe_failure() {
        let Err(error) = super::detect_macos_platform(true, false, Some("sandbox_apply failed"))
        else {
            panic!("profile probe failure must fail closed");
        };

        assert!(error.contains("could not apply a Seatbelt profile"));
        assert!(error.contains("Probe failure: sandbox_apply failed."));
        assert!(error.contains("CAKE_SANDBOX=off"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_detection_fails_closed_without_probe_details() {
        let Err(error) = super::detect_macos_platform(true, false, None) else {
            panic!("profile probe failure must fail closed");
        };

        assert!(error.contains("could not apply a Seatbelt profile"));
        assert!(!error.contains("Probe failure:"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_detection_uses_sandbox_when_available() {
        let strategy = super::detect_macos_platform(true, true, None).unwrap();

        assert!(strategy.is_some());
    }

    /// Smoke test that every major toolchain category from the safehouse
    /// reference profile is represented in the default sandbox config. This
    /// guards against accidentally regressing the broad-coverage approach the
    /// project relies on so cake works across many codebases.
    #[test]
    fn build_covers_common_toolchains() {
        // Pin HOME via temp_env so this test serializes with other tests that
        // mutate HOME and does not depend on the ambient HOME value.
        temp_env::with_var("HOME", Some("/tmp/cake-sandbox-test-home"), || {
            let home = PathBuf::from("/tmp/cake-sandbox-test-home");
            let context = ToolContext::with_temp_dirs(
                PathBuf::from("/workspace"),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            );
            let config = SandboxConfig::build(&context);

            let cross_platform_expected = [
                // Node ecosystem
                home.join(".npm"),
                home.join(".npmrc"),
                home.join(".config/configstore"),
                home.join(".node-gyp"),
                home.join(".pnpm-store"),
                home.join(".yarn"),
                home.join(".cache/node/corepack"),
                home.join(".nvm"),
                home.join(".cache/turbo"),
                // Bun / Deno
                home.join(".bun"),
                home.join(".deno"),
                // Rust build cache (sccache default on Linux)
                home.join(".cache/sccache"),
                // Go
                home.join("go"),
                home.join(".cache/go-build"),
                // Java / JVM
                home.join(".m2"),
                home.join(".gradle"),
                home.join(".sdkman"),
                home.join(".cache/coursier"),
                // Python
                home.join(".cache/uv"),
                home.join(".cache/pip"),
                home.join(".local/pipx"),
                home.join(".pyenv"),
                home.join(".cache/ruff"),
                home.join(".cache/mypy"),
                home.join(".virtualenvs"),
                home.join(".conda"),
                // Ruby
                home.join(".rbenv"),
                home.join(".bundle"),
                home.join(".gem"),
                // Perl / PHP
                home.join(".cpanm"),
                home.join(".composer"),
                // Runtime managers
                home.join(".proto"),
                home.join(".pkgx"),
            ];

            for expected in cross_platform_expected {
                assert!(
                    config.writable.contains(&expected),
                    "expected read-write sandbox access for {}",
                    expected.display()
                );
            }

            #[cfg(target_os = "macos")]
            for expected in [
                // Rust build cache (sccache default on macOS)
                home.join("Library/Caches/Mozilla.sccache"),
                home.join("Library/Caches/npm"),
                home.join("Library/pnpm"),
                home.join("Library/Caches/Yarn"),
                home.join("Library/Caches/bun"),
                home.join("Library/Caches/deno"),
                home.join("Library/Caches/go-build"),
                home.join("Library/Java"),
                home.join("Library/Caches/Coursier"),
                home.join("Library/Caches/uv"),
                home.join("Library/Caches/bundle"),
                home.join("Library/Caches/composer"),
            ] {
                assert!(
                    config.writable.contains(&expected),
                    "expected read-write sandbox access for {}",
                    expected.display()
                );
            }
        });
    }

    #[test]
    fn resolve_linked_worktree_normal_repo_returns_empty() {
        // A normal repo with .git as a directory should return empty vec
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();

        let dirs = super::resolve_linked_worktree_dirs(tmp.path());
        assert!(dirs.is_empty(), "normal repo should return empty dirs");
    }

    #[test]
    fn resolve_linked_worktree_no_git_returns_empty() {
        // A directory with no .git at all should return empty vec
        let tmp = tempfile::tempdir().unwrap();

        let dirs = super::resolve_linked_worktree_dirs(tmp.path());
        assert!(dirs.is_empty(), "missing .git should return empty dirs");
    }

    #[test]
    fn resolve_linked_worktree_parses_gitdir_and_commondir() {
        // Simulate a linked worktree where .git is a file pointing to an
        // external per-worktree gitdir with a commondir.
        let tmp = tempfile::tempdir().unwrap();
        let repo_git = tmp.path().join(".git");
        let worktree_gitdir = tmp.path().join(".git-worktree-test");
        let common_dir = tmp.path().join(".git-common-test");

        // Create the worktree gitdir and common dir as real directories
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        std::fs::create_dir_all(&common_dir).unwrap();

        // Write .git file (linked worktree marker) with an absolute path
        std::fs::write(
            &repo_git,
            format!("gitdir: {}", worktree_gitdir.to_string_lossy()),
        )
        .unwrap();

        // Write commondir with an absolute path to the common dir
        std::fs::write(
            worktree_gitdir.join("commondir"),
            format!("{}\n", common_dir.to_string_lossy()),
        )
        .unwrap();

        let dirs = super::resolve_linked_worktree_dirs(tmp.path());
        assert_eq!(
            dirs.len(),
            2,
            "should return per-worktree dir and common dir"
        );

        let canonical_worktree = worktree_gitdir.canonicalize().unwrap();
        let canonical_common = common_dir.canonicalize().unwrap();
        assert!(
            dirs.contains(&canonical_worktree),
            "per-worktree gitdir should be in writable set"
        );
        assert!(
            dirs.contains(&canonical_common),
            "common git dir should be in writable set"
        );
    }

    #[test]
    fn resolve_linked_worktree_warns_on_missing_gitdir() {
        // Simulate a .git file with no gitdir: line
        let tmp = tempfile::tempdir().unwrap();
        let repo_git = tmp.path().join(".git");
        std::fs::write(&repo_git, "not a gitdir line\n").unwrap();

        let dirs = super::resolve_linked_worktree_dirs(tmp.path());
        assert!(dirs.is_empty(), "malformed .git should return empty dirs");
    }

    #[test]
    fn resolve_linked_worktree_relative_gitdir() {
        // Test that a relative path in the .git file is resolved correctly
        //
        // In a real linked worktree, the .git file is in the worktree root
        // and points to <parent>/.git/worktrees/<name>. The worktree gitdir
        // is under the parent's .git directory, not under the worktree.
        let tmp = tempfile::tempdir().unwrap();
        let repo_git = tmp.path().join(".git");
        let parent_git = tmp.path().join("parent/.git");
        let worktree_gitdir = parent_git.join("worktrees/my-worktree");

        std::fs::create_dir_all(&worktree_gitdir).unwrap();

        // Write .git file with a relative path pointing into parent's .git
        std::fs::write(&repo_git, "gitdir: parent/.git/worktrees/my-worktree\n").unwrap();

        // Write commondir with a relative path ("../.." points to parent's .git,
        // matching what git actually writes in a linked worktree)
        std::fs::write(worktree_gitdir.join("commondir"), "../..\n").unwrap();

        let dirs = super::resolve_linked_worktree_dirs(tmp.path());
        assert_eq!(
            dirs.len(),
            2,
            "should return per-worktree dir and common dir with relative paths"
        );

        let canonical_worktree = worktree_gitdir.canonicalize().unwrap();
        let canonical_common = parent_git.canonicalize().unwrap();
        assert!(
            dirs.contains(&canonical_worktree),
            "per-worktree gitdir should be in writable set"
        );
        assert!(
            dirs.contains(&canonical_common),
            "common git dir should be in writable set"
        );
    }

    #[test]
    fn resolve_linked_worktree_unreadable_gitdir() {
        // Test with gitdir pointing to a non-existent path
        let tmp = tempfile::tempdir().unwrap();
        let repo_git = tmp.path().join(".git");

        std::fs::write(&repo_git, "gitdir: /nonexistent/path\n").unwrap();

        let dirs = super::resolve_linked_worktree_dirs(tmp.path());
        assert!(
            dirs.is_empty(),
            "should return empty when gitdir cannot be resolved"
        );
    }

    #[test]
    fn resolve_worktree_commondir_missing() {
        // Test resolve_worktree_commondir with no commondir file
        let tmp = tempfile::tempdir().unwrap();
        let worktree_gitdir = tmp.path().join(".git/worktrees/missing");
        std::fs::create_dir_all(&worktree_gitdir).unwrap();

        let result = super::resolve_worktree_commondir(&worktree_gitdir);
        assert!(result.is_none(), "missing commondir should return None");
    }

    #[test]
    fn resolve_worktree_commondir_empty() {
        // Test resolve_worktree_commondir with an empty commondir file
        let tmp = tempfile::tempdir().unwrap();
        let worktree_gitdir = tmp.path().join(".git/worktrees/empty-commondir");
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        std::fs::write(worktree_gitdir.join("commondir"), "").unwrap();

        let result = super::resolve_worktree_commondir(&worktree_gitdir);
        assert!(result.is_none(), "empty commondir should return None");
    }

    #[test]
    fn resolve_worktree_commondir_unresolvable() {
        // Test resolve_worktree_commondir with a path that can't be canonicalized
        let tmp = tempfile::tempdir().unwrap();
        let worktree_gitdir = tmp.path().join(".git/worktrees/bad-path");
        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        std::fs::write(worktree_gitdir.join("commondir"), "/nonexistent/path\n").unwrap();

        let result = super::resolve_worktree_commondir(&worktree_gitdir);
        assert!(
            result.is_none(),
            "unresolvable commondir should return None"
        );
    }

    #[test]
    fn build_with_policy_includes_linked_worktree_dirs_in_writable() {
        // Verify that build_with_policy adds linked worktree dirs to writable
        let tmp = tempfile::tempdir().unwrap();
        let repo_git = tmp.path().join(".git");
        let worktree_gitdir = tmp.path().join(".git-worktree-build");
        let common_dir = tmp.path().join(".git-common-build");

        std::fs::create_dir_all(&worktree_gitdir).unwrap();
        std::fs::create_dir_all(&common_dir).unwrap();

        std::fs::write(
            &repo_git,
            format!("gitdir: {}", worktree_gitdir.to_string_lossy()),
        )
        .unwrap();
        std::fs::write(
            worktree_gitdir.join("commondir"),
            format!("{}\n", common_dir.to_string_lossy()),
        )
        .unwrap();

        let config = SandboxConfig::build_with_policy(
            SandboxPolicy::WorkspaceWrite,
            tmp.path(),
            &[],
            &[],
            &[],
            &[],
        );

        let canonical_worktree = worktree_gitdir.canonicalize().unwrap();
        let canonical_common = common_dir.canonicalize().unwrap();

        assert!(
            config.writable.contains(&canonical_worktree),
            "per-worktree gitdir must be in writable"
        );
        assert!(
            config.writable.contains(&canonical_common),
            "common git dir must be in writable"
        );
    }

    #[test]
    fn resolve_linked_worktree_with_real_git_worktree() {
        // Regression test using an actual git linked worktree created via
        // `git worktree add`. This exercises the real git directory layout
        // that users encounter when cake's workspace is a linked worktree.
        //
        // Coverage notes:
        // - This covers externally-created worktrees (the `git worktree add`
        //   path). Cake-managed worktrees (--worktree flag) route through the
        //   same `SandboxConfig::build_with_policy` and `resolve_linked_worktree_dirs`
        //   code — the .git file pointer and commondir resolution are identical
        //   regardless of how the worktree was created.
        // - Both the per-worktree gitdir (`<main>/.git/worktrees/<name>`) and
        //   the common dir (`<main>/.git`) are resolved from metadata, not
        //   assumed from path structure.
        let tmp = tempfile::tempdir().unwrap();
        let main_repo = tmp.path().join("main");
        let wt_path = tmp.path().join("linked-worktree");

        // Initialize main repo with a commit and a detached linked worktree
        crate::config::git::test_support::init_repo_with_linked_worktree(&main_repo, &wt_path);

        // Build sandbox config using the worktree as cwd
        let config = SandboxConfig::build_with_policy(
            SandboxPolicy::WorkspaceWrite,
            &wt_path,
            &[],
            &[],
            &[],
            &[],
        );

        // The main repo's .git directory (common dir) must be in writable
        let main_git = main_repo.join(".git").canonicalize().unwrap();
        assert!(
            config.writable.contains(&main_git),
            "common .git dir ({}) must be in writable set",
            main_git.display()
        );

        // Read the actual gitdir from the worktree's .git file and assert
        // that exact path is in the writable set (not just any descendant).
        let git_file_content =
            std::fs::read_to_string(wt_path.join(".git")).expect(".git file must be readable");
        let gitdir_line = git_file_content
            .lines()
            .find(|l| l.trim().starts_with("gitdir:"))
            .expect(".git file must contain a gitdir: line");
        let gitdir_raw = gitdir_line
            .strip_prefix("gitdir: ")
            .or_else(|| gitdir_line.strip_prefix("gitdir:"))
            .map(str::trim)
            .expect("gitdir: line must have a path");
        let gitdir_path = if Path::new(gitdir_raw).is_relative() {
            wt_path.join(gitdir_raw)
        } else {
            PathBuf::from(gitdir_raw)
        };
        let canonical_gitdir = gitdir_path
            .canonicalize()
            .expect("gitdir path must be resolvable");
        assert!(
            config.writable.contains(&canonical_gitdir),
            "exact worktree gitdir ({}) parsed from .git file must be in writable set",
            canonical_gitdir.display()
        );
    }
}

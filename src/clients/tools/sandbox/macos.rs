//! macOS sandbox implementation using `sandbox-exec`
//!
//! Uses the Seatbelt sandbox profile language (Scheme-like syntax) to
//! generate dynamic sandbox profiles that restrict filesystem access.
//! The profile uses a deny-default policy: everything is denied unless
//! explicitly allowed.

use crate::clients::tools::sandbox::{SandboxConfig, SandboxGuard, SandboxPolicy, SandboxStrategy};
use crate::clients::tools::secure_temp_dir::secure_temp_dir;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output, Stdio};
use std::sync::OnceLock;

/// macOS sandbox strategy using sandbox-exec
#[derive(Debug, Clone, Copy)]
pub struct MacOsSandbox;

impl MacOsSandbox {
    /// Return whether `sandbox-exec` can apply a profile in this process context.
    ///
    /// macOS can reject applying a new Seatbelt profile from an already-sandboxed
    /// process. Probing avoids treating the mere presence of `/usr/bin/sandbox-exec`
    /// as proof that Bash commands can be sandboxed.
    pub(super) fn can_apply_profile() -> bool {
        Self::profile_probe().can_apply
    }

    /// Return the cached probe failure detail, if profile application is unavailable.
    pub(super) fn profile_probe_failure() -> Option<&'static str> {
        Self::probe_failure(Self::profile_probe())
    }

    fn probe_failure(probe: &SandboxProfileProbe) -> Option<&str> {
        probe.failure.as_deref()
    }

    fn profile_probe() -> &'static SandboxProfileProbe {
        static PROBE: OnceLock<SandboxProfileProbe> = OnceLock::new();
        PROBE.get_or_init(Self::probe_can_apply_profile)
    }

    fn probe_can_apply_profile() -> SandboxProfileProbe {
        let tmp_dir = match secure_temp_dir() {
            Ok(dir) => dir,
            Err(e) => {
                let failure = format!("failed to create sandbox profile probe directory: {e}");
                tracing::warn!("{failure}");
                return SandboxProfileProbe::unavailable(failure);
            },
        };
        Self::probe_can_apply_profile_in(tmp_dir, |profile_path| {
            std::process::Command::new("/usr/bin/sandbox-exec")
                .arg("-f")
                .arg(profile_path)
                .arg("/bin/echo")
                .arg("cake-sandbox-probe")
                .output()
        })
    }

    fn probe_can_apply_profile_in(
        tmp_dir: &Path,
        run_probe: impl FnOnce(&Path) -> std::io::Result<Output>,
    ) -> SandboxProfileProbe {
        if let Err(e) = std::fs::create_dir_all(tmp_dir) {
            let failure = format!("failed to create sandbox profile probe directory: {e}");
            tracing::warn!("{failure}");
            return SandboxProfileProbe::unavailable(failure);
        }

        let mut temp_file = match tempfile::Builder::new()
            .prefix("cake_sandbox_probe_")
            .suffix(".sb")
            .tempfile_in(tmp_dir)
        {
            Ok(file) => file,
            Err(e) => {
                let failure = format!("failed to create sandbox profile probe file: {e}");
                tracing::warn!("{failure}");
                return SandboxProfileProbe::unavailable(failure);
            },
        };

        if let Err(e) = temp_file.write_all(b"(version 1)\n(allow default)\n") {
            let failure = format!("failed to write sandbox profile probe file: {e}");
            tracing::warn!("{failure}");
            return SandboxProfileProbe::unavailable(failure);
        }

        match run_probe(temp_file.path()) {
            Ok(output) if output.status.success() => SandboxProfileProbe::available(),
            Ok(output) => {
                let failure =
                    Self::format_probe_failure(output.status, &output.stdout, &output.stderr);
                tracing::warn!(
                    "macOS sandbox-exec is present but cannot apply profiles: {}",
                    failure
                );
                SandboxProfileProbe::unavailable(failure)
            },
            Err(e) => {
                let failure = format!("failed to run sandbox-exec probe: {e}");
                tracing::warn!("{failure}");
                SandboxProfileProbe::unavailable(failure)
            },
        }
    }

    fn format_probe_failure(status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> String {
        let stderr = String::from_utf8_lossy(stderr);
        let stdout = String::from_utf8_lossy(stdout);
        let stderr = stderr.trim();
        let stdout = stdout.trim();

        if !stderr.is_empty() {
            format!("sandbox-exec exited with {status}; stderr: {stderr}")
        } else if !stdout.is_empty() {
            format!("sandbox-exec exited with {status}; stdout: {stdout}")
        } else {
            format!("sandbox-exec exited with {status}; no stderr or stdout")
        }
    }

    /// Append device and PTY rules to the profile
    fn append_device_rules(profile: &mut SeatbeltProfileBuilder) {
        profile.comment("Allow access to standard and PTY devices");
        profile.allow_literal("file-read* file-write*", "/dev/null");
        profile.allow_literal("file-read*", "/dev/urandom");
        profile.allow_literal("file-read*", "/dev/random");
        profile.allow_literal("file-read*", "/dev/zero");
        profile.allow_literal("file-read* file-write*", "/dev/tty");
        profile.allow_literal("file-read* file-write*", "/dev/ptmx");
        profile.allow_literal("file-read* file-write*", "/dev/dtracehelper");
        profile.allow_literal("file-read* file-write*", "/dev/stdout");
        profile.allow_literal("file-read* file-write*", "/dev/stderr");
        profile.allow_subpath("file-read* file-write*", "/dev/fd");
        profile.allow_regex("file-read* file-write*", "^/dev/ttys");
        profile.allow_regex("file-read* file-write*", "^/dev/pty");
        profile.blank();
    }

    /// Append the app-launching and automation capabilities granted only to
    /// `WorkspaceWriteInteractive`.
    ///
    /// The two primitives let a command hand work to another application.
    /// They are platform capabilities, not filesystem grants, so they are
    /// absent from `SandboxConfig`'s path lists. Without `lsopen`,
    /// `LaunchServices` rejects `open` with `_LSOpenURLsWithCompletionHandler()
    /// ... error -54` (`permErr`); without `appleevent-send`, `osascript`
    /// rejects a message to another application the same way. The launched
    /// application runs outside cake's sandbox.
    fn append_interactive_rules(profile: &mut SeatbeltProfileBuilder, policy: SandboxPolicy) {
        if !policy.allows_app_interaction() {
            return;
        }
        profile.comment("Interactive sessions: launch and automate other applications");
        profile.allow("lsopen");
        profile.allow("appleevent-send");
        profile.blank();
    }

    /// Append git configuration read-only rules to the profile
    fn append_git_rules(profile: &mut SeatbeltProfileBuilder) {
        profile.comment("Git configuration (read-only)");
        if let Some(home) = home_dir() {
            profile.allow_prefix("file-read*", home.join(".gitconfig"));
            profile.allow_prefix("file-read*", home.join(".gitignore"));
            profile.allow_subpath("file-read*", home.join(".config/git"));
            profile.allow_literal("file-read*", home.join(".gitattributes"));
            profile.allow_literal("file-read*", home.join(".ssh"));
            profile.allow_literal("file-read*", home.join(".ssh/config"));
            profile.allow_literal("file-read*", home.join(".ssh/known_hosts"));
        }
        profile.blank();
    }

    /// Append SSH agent socket rules to the profile.
    ///
    /// Seatbelt reaches a Unix-domain socket through write access to the
    /// socket path, so the socket file is the grant and its directory is not.
    /// Granting the parent directory instead would make every sibling
    /// writable: an agent socket inside `~/.ssh` would expose `config`,
    /// `known_hosts`, and private keys. The parent gets read-only access,
    /// which is what path traversal needs.
    fn append_ssh_agent_rules(profile: &mut SeatbeltProfileBuilder) {
        profile.comment("SSH agent sockets (for git push over SSH)");
        profile.allow_regex(
            "file-read* file-write*",
            "^/tmp/ssh-[A-Za-z0-9]+/agent\\.[0-9]+$",
        );
        profile.allow_regex(
            "file-read* file-write*",
            "^/private/tmp/ssh-[A-Za-z0-9]+/agent\\.[0-9]+$",
        );
        profile.allow_regex(
            "file-read* file-write*",
            "^/private/tmp/com\\.apple\\.launchd\\..*/Listeners$",
        );
        if let Some(sock_path) = ssh_auth_sock_path() {
            for path in resolved_path_variants(&sock_path) {
                profile.allow_literal("file-read* file-write*", &path);
                if let Some(parent) = path.parent() {
                    profile.allow_literal("file-read*", parent);
                }
            }
        }
        profile.blank();
    }

    /// Append macOS Keychain access rules to the profile.
    ///
    /// Note: actual Keychain service access (used by `gh`, `security`, and
    /// SSH passphrase retrieval) is mediated by Security.framework over Mach
    /// IPC, which is covered by the `(allow mach-lookup)` rule above. The
    /// file-level rules here allow tools that read keychain database files
    /// directly (rare). They are read-only under every applied sandbox policy;
    /// Keychain service operations remain mediated by Security.framework.
    fn append_keychain_rules(profile: &mut SeatbeltProfileBuilder, policy: SandboxPolicy) {
        profile.comment(
            "macOS Keychain file access (supplementary; primary access is via mach-lookup)",
        );
        profile.allow_subpath("file-read*", "/Library/Keychains");
        profile.allow_subpath("file-read*", "/System/Library/Keychains");
        if let Some(home) = home_dir() {
            profile.allow_literal("file-read*", &home);
            profile.allow_literal("file-read*", home.join("Library"));
            let access = if policy == SandboxPolicy::DangerFullAccess {
                "file-read* file-write*"
            } else {
                "file-read*"
            };
            profile.allow_subpath(access, home.join("Library/Keychains"));
        }
        profile.blank();
    }

    /// Generate a deny-default sandbox profile (.sb file content) from the configuration
    fn generate_profile(config: &SandboxConfig) -> String {
        let mut profile = SeatbeltProfileBuilder::new();
        profile.version(1);
        profile.deny_default();
        profile.blank();

        profile.comment("Allow process execution");
        profile.allow("process-fork");
        profile.allow("process-exec");
        profile.allow("pseudo-tty");
        profile.blank();

        profile.comment("Allow process introspection within same sandbox");
        profile.allow_with_target("process-info*", "same-sandbox");
        profile.allow_with_target("signal", "same-sandbox");
        profile.allow_with_target("mach-priv-task-port", "same-sandbox");
        profile.blank();

        profile.comment("Allow mach lookups (needed for basic process operation)");
        profile.allow("mach-lookup");
        profile.blank();

        Self::append_interactive_rules(&mut profile, config.policy);

        profile.comment("Allow sysctl reads");
        profile.allow("sysctl-read");
        profile.blank();

        profile.comment("Allow system sockets and shared memory");
        profile.allow("system-socket");
        profile.allow_raw(
            "(allow ipc-posix-shm-read-data (ipc-posix-name \"apple.shm.notification_center\"))",
        );
        profile.blank();

        profile.comment("Allow network access");
        profile.allow("network*");
        profile.blank();

        profile.comment("Allow reading root directory (needed by dyld)");
        profile.allow_literal("file-read*", "/");
        profile.blank();

        profile.comment("Allow reading ancestor directories of allowed paths");
        let mut ancestor_set = std::collections::BTreeSet::new();
        for path in config.writable.iter().chain(&config.read_execute) {
            let mut ancestor = path.as_path();
            while let Some(parent) = ancestor.parent() {
                if parent != Path::new("/") {
                    ancestor_set.insert(parent.to_path_buf());
                }
                ancestor = parent;
            }
        }
        for ancestor in &ancestor_set {
            profile.allow_literal("file-read*", ancestor);
        }
        profile.blank();

        if !config.writable.is_empty() {
            profile.comment("Read-write access: working directory, temp dirs, and toolchains");
            for path in &config.writable {
                Self::allow_path_access(&mut profile, "file-read* file-write*", path);
            }
            profile.blank();
        }

        if !config.read_execute.is_empty() {
            profile.comment("Read and execute access: read-only paths");
            for path in &config.read_execute {
                Self::allow_path_access(&mut profile, "file-read*", path);
            }
            profile.blank();
        }

        Self::append_git_rules(&mut profile);
        Self::append_ssh_agent_rules(&mut profile);
        Self::append_keychain_rules(&mut profile, config.policy);
        Self::append_device_rules(&mut profile);

        profile.comment("Allow file-ioctl for terminal operations");
        profile.allow("file-ioctl");

        profile.comment("Allow file locking (needed by cargo and other build tools)");
        profile.allow("file-lock");

        profile.finish()
    }

    /// Emit a rule for one configured path: a literal rule for a file, a
    /// subpath rule for a directory.
    ///
    /// A file literal grants access to exactly that file (ancestor directory
    /// reads are already emitted above), so a sibling file in the same
    /// directory stays denied. Nonexistent paths fall back to subpath, which
    /// matches historical behavior for paths that only exist at runtime.
    fn allow_path_access(profile: &mut SeatbeltProfileBuilder, permissions: &str, path: &Path) {
        if path.is_file() {
            profile.allow_literal(permissions, path);
        } else {
            profile.allow_subpath(permissions, path);
        }
    }

    /// Write the profile to a temp file and return its path
    fn write_profile_to_temp(profile: &str) -> Result<tempfile::NamedTempFile, String> {
        let tmp_dir = secure_temp_dir()
            .map_err(|e| format!("Failed to create sandbox profile directory: {e}"))?;

        let mut temp_file = tempfile::Builder::new()
            .prefix("cake_sandbox_")
            .suffix(".sb")
            .tempfile_in(tmp_dir)
            .map_err(|e| format!("Failed to create sandbox profile temp file: {e}"))?;

        temp_file
            .write_all(profile.as_bytes())
            .map_err(|e| format!("Failed to write sandbox profile: {e}"))?;

        tracing::debug!(
            "Generated sandbox profile at: {}",
            temp_file.path().display()
        );

        Ok(temp_file)
    }
}

#[derive(Debug)]
struct SandboxProfileProbe {
    can_apply: bool,
    failure: Option<String>,
}

impl SandboxProfileProbe {
    const fn available() -> Self {
        Self {
            can_apply: true,
            failure: None,
        }
    }

    const fn unavailable(failure: String) -> Self {
        Self {
            can_apply: false,
            failure: Some(failure),
        }
    }
}

impl SandboxStrategy for MacOsSandbox {
    fn apply(
        &self,
        command: &mut tokio::process::Command,
        config: &SandboxConfig,
    ) -> Result<SandboxGuard, String> {
        let profile = Self::generate_profile(config);
        tracing::debug!("Generated sandbox profile:\n{profile}");

        let temp_file = Self::write_profile_to_temp(&profile)?;
        let profile_path = temp_file.path().to_path_buf();

        let original_args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let original_cwd = command.as_std().get_current_dir().map(Path::to_path_buf);
        let original_env: Vec<(OsString, Option<OsString>)> = command
            .as_std()
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(OsStr::to_os_string)))
            .collect();

        *command = tokio::process::Command::new("/usr/bin/sandbox-exec");

        command.arg("-f").arg(&profile_path);

        command.arg("bash");
        for arg in original_args {
            command.arg(arg);
        }

        if let Some(cwd) = original_cwd {
            command.current_dir(cwd);
        }

        for (key, value) in original_env {
            match value {
                Some(value) => {
                    command.env(key, value);
                },
                None => {
                    command.env_remove(key);
                },
            }
        }

        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        tracing::debug!("Sandboxed command configured with deny-default profile");

        Ok(SandboxGuard::new(temp_file))
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Return the absolute `SSH_AUTH_SOCK` path when it names a Unix-domain socket.
///
/// A relative value cannot match a Seatbelt path rule, and an `SSH_AUTH_SOCK`
/// that names a directory or a regular file would otherwise turn that path into
/// a write grant. A stale value for a socket that no longer exists grants
/// nothing, which matches the connection failure the command would hit anyway.
fn ssh_auth_sock_path() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("SSH_AUTH_SOCK")?);
    if !path.is_absolute() {
        return None;
    }
    let metadata = std::fs::metadata(&path).ok()?;
    metadata.file_type().is_socket().then_some(path)
}

/// Return `path` plus its canonical spelling when the two differ.
///
/// Seatbelt matches resolved vnode paths, while `SSH_AUTH_SOCK` and the
/// conventional agent paths can spell the macOS `/tmp` and `/var/run`
/// symlinks literally. Emitting both spellings keeps the socket reachable
/// without widening the grant to a directory.
fn resolved_path_variants(path: &Path) -> Vec<PathBuf> {
    let mut paths = vec![path.to_path_buf()];
    if let Ok(resolved) = path.canonicalize()
        && resolved != path
    {
        paths.push(resolved);
    }
    paths
}

struct SeatbeltProfileBuilder {
    lines: Vec<String>,
}

impl SeatbeltProfileBuilder {
    const fn new() -> Self {
        Self { lines: Vec::new() }
    }

    fn version(&mut self, version: u32) {
        self.lines.push(format!("(version {version})"));
    }

    fn deny_default(&mut self) {
        self.lines.push(String::from("(deny default)"));
    }

    fn comment(&mut self, comment: &str) {
        self.lines.push(format!("; {comment}"));
    }

    fn blank(&mut self) {
        self.lines.push(String::new());
    }

    fn allow(&mut self, permissions: &str) {
        self.lines.push(format!("(allow {permissions})"));
    }

    fn allow_with_target(&mut self, permissions: &str, target: &str) {
        self.lines
            .push(format!("(allow {permissions} (target {target}))"));
    }

    fn allow_raw(&mut self, rule: &str) {
        self.lines.push(rule.to_string());
    }

    fn allow_literal(&mut self, permissions: &str, path: impl AsRef<Path>) {
        self.allow_path(permissions, "literal", path);
    }

    fn allow_prefix(&mut self, permissions: &str, path: impl AsRef<Path>) {
        self.allow_path(permissions, "prefix", path);
    }

    fn allow_subpath(&mut self, permissions: &str, path: impl AsRef<Path>) {
        self.allow_path(permissions, "subpath", path);
    }

    fn allow_regex(&mut self, permissions: &str, pattern: &str) {
        self.lines
            .push(format!("(allow {permissions} (regex #\"{pattern}\"))"));
    }

    fn allow_path(&mut self, permissions: &str, matcher: &str, path: impl AsRef<Path>) {
        let escaped = Self::escape_path(path.as_ref());
        self.lines
            .push(format!("(allow {permissions} ({matcher} \"{escaped}\"))"));
    }

    /// Escape a path for embedding in a Seatbelt profile string literal.
    ///
    /// Backslashes, double quotes, and control characters (notably newline)
    /// are escaped so a configured path cannot break out of the literal and
    /// inject profile rules. If the platform profile parser does not decode an
    /// escape, the rule simply fails to match that path, which fails closed
    /// (denied) rather than allowing unintended access.
    fn escape_path(path: &Path) -> String {
        path.to_string_lossy()
            .chars()
            .map(Self::escape_char)
            .collect()
    }

    /// Escape a single character for a Seatbelt profile string literal,
    /// returning it unchanged when it needs no escaping.
    fn escape_char(c: char) -> String {
        match c {
            '\\' => "\\\\".to_string(),
            '"' => "\\\"".to_string(),
            '\n' => "\\n".to_string(),
            '\r' => "\\r".to_string(),
            '\t' => "\\t".to_string(),
            c if c.is_control() => format!("\\x{:02x}", c as u32),
            c => c.to_string(),
        }
    }

    fn finish(self) -> String {
        self.lines.join("\n")
    }
}

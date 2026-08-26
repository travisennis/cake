//! macOS sandbox implementation using `sandbox-exec`
//!
//! Uses the Seatbelt sandbox profile language (Scheme-like syntax) to
//! generate dynamic sandbox profiles that restrict filesystem access.
//! The profile uses a deny-default policy: everything is denied unless
//! explicitly allowed.

use crate::clients::tools::sandbox::{SandboxConfig, SandboxGuard, SandboxPolicy, SandboxStrategy};
use crate::clients::tools::secure_temp_dir::secure_temp_dir;
use std::io::Write;
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
        // Seatbelt profile applicability is a property of this process context:
        // an already-sandboxed cake process cannot become unsandboxed later in
        // the same run. Cache the probe so every Bash command does not pay for
        // a sandbox-exec subprocess before doing real work.
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

    /// Append git configuration read-only rules to the profile
    fn append_git_rules(profile: &mut SeatbeltProfileBuilder) {
        profile.comment("Git configuration (read-only)");
        if let Some(home) = home_dir() {
            profile.allow_prefix("file-read*", home.join(".gitconfig"));
            profile.allow_prefix("file-read*", home.join(".gitignore"));
            profile.allow_subpath("file-read*", home.join(".config/git"));
            profile.allow_literal("file-read*", home.join(".gitattributes"));
            // Allow reading .ssh directory itself (for listing)
            profile.allow_literal("file-read*", home.join(".ssh"));
            profile.allow_literal("file-read*", home.join(".ssh/config"));
            profile.allow_literal("file-read*", home.join(".ssh/known_hosts"));
        }
        profile.blank();
    }

    /// Append SSH agent socket rules to the profile
    fn append_ssh_agent_rules(profile: &mut SeatbeltProfileBuilder) {
        profile.comment("SSH agent sockets (for git push over SSH)");
        // SSH agent sockets are typically in /tmp/ssh-XXXXXX/agent.XXXXXX
        profile.allow_regex("file-read* file-write*", "^/tmp/ssh-");
        // On macOS, launchd-managed ssh-agent uses /private/tmp
        profile.allow_regex("file-read* file-write*", "^/private/tmp/ssh-");
        profile.allow_regex(
            "file-read* file-write*",
            "^/private/tmp/com\\.apple\\.launchd\\..*/Listeners$",
        );
        // Allow the actual SSH_AUTH_SOCK path (may be in a non-standard location
        // such as ~/.ssh/agent/). Grant read-write on the parent directory so the
        // sandboxed process can connect to the Unix-domain socket.
        if let Ok(sock) = std::env::var("SSH_AUTH_SOCK") {
            let sock_path = std::path::Path::new(&sock);
            if let Some(parent) = sock_path.parent() {
                profile.allow_subpath("file-read* file-write*", parent);
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
    /// directly (rare, but harmless to permit).
    ///
    /// When `read_only` is true, emit read-only rules so the read-only
    /// sandbox policy cannot write to user keychain database files.
    fn append_keychain_rules(profile: &mut SeatbeltProfileBuilder, read_only: bool) {
        profile.comment(
            "macOS Keychain file access (supplementary; primary access is via mach-lookup)",
        );
        profile.allow_subpath("file-read*", "/Library/Keychains");
        profile.allow_subpath("file-read*", "/System/Library/Keychains");
        if let Some(home) = home_dir() {
            let access = if read_only {
                "file-read*"
            } else {
                "file-read* file-write*"
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

        // Process execution (fork/exec needed for bash and subcommands)
        profile.comment("Allow process execution");
        profile.allow("process-fork");
        profile.allow("process-exec");
        profile.allow("pseudo-tty");
        profile.blank();

        // Process introspection scoped to same sandbox
        profile.comment("Allow process introspection within same sandbox");
        profile.allow_with_target("process-info*", "same-sandbox");
        profile.allow_with_target("signal", "same-sandbox");
        profile.allow_with_target("mach-priv-task-port", "same-sandbox");
        profile.blank();

        // Mach services (required for dyld, DNS, system frameworks, etc.)
        profile.comment("Allow mach lookups (needed for basic process operation)");
        profile.allow("mach-lookup");
        profile.blank();

        // Sysctl reads (needed by many tools)
        profile.comment("Allow sysctl reads");
        profile.allow("sysctl-read");
        profile.blank();

        // System socket (needed for kernel event monitoring by network stack)
        profile.comment("Allow system sockets and shared memory");
        profile.allow("system-socket");
        profile.allow_raw(
            "(allow ipc-posix-shm-read-data (ipc-posix-name \"apple.shm.notification_center\"))",
        );
        profile.blank();

        // Network access (sandbox only restricts filesystem, not network)
        profile.comment("Allow network access");
        profile.allow("network*");
        profile.blank();

        // Root directory literal (dyld needs to traverse root)
        profile.comment("Allow reading root directory (needed by dyld)");
        profile.allow_literal("file-read*", "/");
        profile.blank();

        // Ancestor directory literals for all writable and read-and-execute paths.
        // Agents and tools call readdir() and stat() on ancestors to traverse paths.
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

        // Read-write access for working directory and temp dirs
        if !config.writable.is_empty() {
            profile.comment("Read-write access: working directory, temp dirs, and toolchains");
            for path in &config.writable {
                Self::allow_path_access(&mut profile, "file-read* file-write*", path);
            }
            profile.blank();
        }

        // Read and execute access for system paths, configured read-only paths,
        // skill paths, and paths demoted by read-only policy. The global
        // process-exec rule above supplies execution authority.
        if !config.read_execute.is_empty() {
            profile.comment("Read and execute access: read-only paths");
            for path in &config.read_execute {
                Self::allow_path_access(&mut profile, "file-read*", path);
            }
            profile.blank();
        }

        Self::append_git_rules(&mut profile);
        Self::append_ssh_agent_rules(&mut profile);
        Self::append_keychain_rules(&mut profile, config.policy == SandboxPolicy::ReadOnly);
        Self::append_device_rules(&mut profile);

        // Allow file-ioctl scoped to terminal devices
        profile.comment("Allow file-ioctl for terminal operations");
        profile.allow("file-ioctl");

        // Allow file locking (needed by cargo and other build tools)
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

        // Write profile to temp file — keep alive via SandboxGuard so
        // sandbox-exec can read it at spawn time, then clean up
        // deterministically when the guard is dropped after execution.
        let temp_file = Self::write_profile_to_temp(&profile)?;
        let profile_path = temp_file.path().to_path_buf();

        // Get the original command arguments
        let original_args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        let original_cwd = command.as_std().get_current_dir().map(Path::to_path_buf);

        // Reconfigure the command to use sandbox-exec
        *command = tokio::process::Command::new("/usr/bin/sandbox-exec");

        command.arg("-f").arg(&profile_path);

        // Add the original program (bash) and its arguments
        command.arg("bash");
        for arg in original_args {
            command.arg(arg);
        }

        if let Some(cwd) = original_cwd {
            command.current_dir(cwd);
        }

        // Re-apply stdio configuration
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        tracing::debug!("Sandboxed command configured with deny-default profile");

        // Return the temp file in a guard so it stays alive through spawn
        // and is cleaned up when dropped.
        Ok(SandboxGuard::new(temp_file))
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn test_config() -> SandboxConfig {
        SandboxConfig {
            writable: vec![PathBuf::from("/workspace")],
            read_execute: vec![
                PathBuf::from("/usr"),
                PathBuf::from("/bin"),
                PathBuf::from("/etc"),
            ],
            policy: SandboxPolicy::WorkspaceWrite,
        }
    }

    #[test]
    fn test_profile_uses_deny_default() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
        assert!(!profile.contains("(allow default)"));
    }

    #[test]
    fn test_profile_allows_root_literal() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        assert!(profile.contains("(allow file-read* (literal \"/\"))"));
    }

    #[test]
    fn apply_preserves_command_current_directory() {
        let expected_cwd = PathBuf::from("/workspace");
        let mut command = tokio::process::Command::new("bash");
        command.arg("-c").arg("pwd").current_dir(&expected_cwd);

        let guard = MacOsSandbox.apply(&mut command, &test_config()).unwrap();

        assert_eq!(
            command.as_std().get_current_dir(),
            Some(expected_cwd.as_path())
        );
        // Guard falls out of scope, cleaning up the temp profile file.
        drop(guard);
    }

    #[test]
    fn probe_failure_details_prefer_stderr() {
        let status = ExitStatus::from_raw(256);
        let details = MacOsSandbox::format_probe_failure(
            status,
            b"stdout fallback\n",
            b"sandbox_apply failed\n",
        );

        assert!(details.contains("sandbox-exec exited with"));
        assert!(details.contains("stderr: sandbox_apply failed"));
        assert!(!details.contains("stdout fallback"));
    }

    #[test]
    fn probe_failure_details_use_stdout_when_stderr_empty() {
        let status = ExitStatus::from_raw(256);
        let details = MacOsSandbox::format_probe_failure(status, b"stdout fallback\n", b" \n");

        assert!(details.contains("stdout: stdout fallback"));
    }

    #[test]
    fn sandbox_profile_probe_unavailable_stores_failure_details() {
        let probe = SandboxProfileProbe::unavailable("probe failed".to_string());

        assert!(!probe.can_apply);
        assert_eq!(MacOsSandbox::probe_failure(&probe), Some("probe failed"));
    }

    #[test]
    fn profile_probe_failure_accessor_is_callable() {
        let _ = MacOsSandbox::profile_probe_failure();
    }

    #[test]
    fn probe_can_apply_profile_reports_success() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let probe = MacOsSandbox::probe_can_apply_profile_in(tmp_dir.path(), |profile_path| {
            assert!(profile_path.exists());
            Ok(Output {
                status: ExitStatus::from_raw(0),
                stdout: b"cake-sandbox-probe\n".to_vec(),
                stderr: Vec::new(),
            })
        });

        assert!(probe.can_apply);
        assert!(probe.failure.is_none());
    }

    #[test]
    fn probe_can_apply_profile_reports_command_failure_details() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let probe = MacOsSandbox::probe_can_apply_profile_in(tmp_dir.path(), |_| {
            Ok(Output {
                status: ExitStatus::from_raw(256),
                stdout: b"stdout fallback\n".to_vec(),
                stderr: b"sandbox_apply failed\n".to_vec(),
            })
        });

        assert!(!probe.can_apply);
        assert_eq!(
            probe.failure.as_deref(),
            Some("sandbox-exec exited with exit status: 1; stderr: sandbox_apply failed")
        );
    }

    #[test]
    fn probe_can_apply_profile_reports_spawn_failure_details() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let probe = MacOsSandbox::probe_can_apply_profile_in(tmp_dir.path(), |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing sandbox-exec",
            ))
        });

        assert!(!probe.can_apply);
        assert_eq!(
            probe.failure.as_deref(),
            Some("failed to run sandbox-exec probe: missing sandbox-exec")
        );
    }

    #[test]
    fn probe_can_apply_profile_fails_before_spawn_for_unusable_directory() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let unusable_path = tmp_dir.path().join("not-a-directory");
        std::fs::write(&unusable_path, b"occupied").unwrap();

        let probe = MacOsSandbox::probe_can_apply_profile_in(&unusable_path, |_| {
            panic!("sandbox-exec probe must not run with an unusable profile directory")
        });

        assert!(!probe.can_apply);
        assert!(probe.failure.as_deref().is_some_and(|failure| {
            failure.starts_with("failed to create sandbox profile probe directory:")
        }));
    }

    #[test]
    fn test_profile_allows_read_write_paths() {
        let config = SandboxConfig {
            writable: vec![PathBuf::from("/workspace"), PathBuf::from("/tmp")],
            read_execute: vec![],
            policy: SandboxPolicy::WorkspaceWrite,
        };

        let profile = MacOsSandbox::generate_profile(&config);

        assert!(profile.contains("(allow file-read* file-write* (subpath \"/workspace\"))"));
        assert!(profile.contains("(allow file-read* file-write* (subpath \"/tmp\"))"));
    }

    #[test]
    fn test_profile_allows_read_and_execute_paths() {
        let config = SandboxConfig {
            writable: vec![],
            read_execute: vec![PathBuf::from("/usr"), PathBuf::from("/etc")],
            policy: SandboxPolicy::WorkspaceWrite,
        };

        let profile = MacOsSandbox::generate_profile(&config);

        assert!(profile.contains("(allow file-read* (subpath \"/usr\"))"));
        assert!(profile.contains("(allow file-read* (subpath \"/etc\"))"));
    }

    #[test]
    fn read_execute_file_emits_literal_rule_and_denies_siblings() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("tool");
        let sibling = tmp.path().join("other-tool");
        std::fs::write(&allowed, b"#!/bin/sh\n").unwrap();
        std::fs::write(&sibling, b"#!/bin/sh\n").unwrap();

        let config = SandboxConfig {
            writable: vec![],
            read_execute: vec![allowed.clone()],
            policy: SandboxPolicy::WorkspaceWrite,
        };

        let profile = MacOsSandbox::generate_profile(&config);

        // The configured file is readable and executable via a literal rule.
        let allowed_escaped = SeatbeltProfileBuilder::escape_path(&allowed);
        assert!(profile.contains(&format!(
            "(allow file-read* (literal \"{allowed_escaped}\"))"
        )));
        // No subpath rule grants the parent directory, so the sibling file
        // has no matching rule and is denied by the deny-default profile.
        let sibling_escaped = SeatbeltProfileBuilder::escape_path(&sibling);
        assert!(!profile.contains(&format!(
            "(allow file-read* (literal \"{sibling_escaped}\"))"
        )));
        assert!(!profile.contains(&format!(
            "(allow file-read* (subpath \"{}\"))",
            tmp.path().display()
        )));
    }

    #[test]
    fn writable_file_emits_literal_read_write_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let allowed = tmp.path().join("state");
        std::fs::write(&allowed, b"state\n").unwrap();

        let config = SandboxConfig {
            writable: vec![allowed.clone()],
            read_execute: vec![],
            policy: SandboxPolicy::WorkspaceWrite,
        };

        let profile = MacOsSandbox::generate_profile(&config);

        let allowed_escaped = SeatbeltProfileBuilder::escape_path(&allowed);
        assert!(profile.contains(&format!(
            "(allow file-read* file-write* (literal \"{allowed_escaped}\"))"
        )));
    }

    #[test]
    fn test_profile_includes_process_and_system_rules() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        assert!(profile.contains("(allow process-fork)"));
        assert!(profile.contains("(allow process-exec)"));
        assert!(profile.contains("(allow pseudo-tty)"));
        assert!(profile.contains("(allow mach-lookup)"));
        assert!(profile.contains("(allow process-info* (target same-sandbox))"));
        assert!(profile.contains("(allow signal (target same-sandbox))"));
        assert!(profile.contains("(allow sysctl-read)"));
        assert!(profile.contains("(allow system-socket)"));
        assert!(profile.contains("(allow network*)"));
    }

    #[test]
    fn test_profile_allows_standard_devices() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        assert!(profile.contains("/dev/null"));
        assert!(profile.contains("/dev/urandom"));
        assert!(profile.contains("/dev/tty"));
        assert!(profile.contains("/dev/ptmx"));
        assert!(profile.contains("/dev/fd"));
    }

    #[test]
    fn test_profile_allows_file_lock() {
        let profile = MacOsSandbox::generate_profile(&test_config());
        assert!(profile.contains("(allow file-lock)"));
    }

    #[test]
    fn test_profile_escaping() {
        let path = PathBuf::from("/path/with\"quote\\backslash (and spaces)");
        let escaped = SeatbeltProfileBuilder::escape_path(&path);
        assert_eq!(escaped, "/path/with\\\"quote\\\\backslash (and spaces)");
    }

    #[test]
    fn test_profile_escaping_control_characters() {
        let path = PathBuf::from("/tmp/dir\nname\t\"quote\"\\backslash\r");
        let escaped = SeatbeltProfileBuilder::escape_path(&path);
        assert_eq!(escaped, "/tmp/dir\\nname\\t\\\"quote\\\"\\\\backslash\\r");

        // A configured path containing a control character must not break out
        // of the profile string literal.
        let mut config = test_config();
        config.writable.push(PathBuf::from("/tmp/line\nbreak"));
        let profile = MacOsSandbox::generate_profile(&config);
        assert!(
            !profile.contains("/tmp/line\nbreak"),
            "raw newline in a path must not appear in the generated profile"
        );
        assert!(
            profile.contains("/tmp/line\\nbreak"),
            "newline should be escaped inside the profile string literal"
        );
    }

    #[test]
    fn test_profile_escapes_home_based_paths() {
        temp_env::with_var(
            "HOME",
            Some("/Users/Test User/quote\"backslash\\paren(home)"),
            || {
                let config = SandboxConfig::build_with_policy(
                    SandboxPolicy::WorkspaceWrite,
                    Path::new("/workspace"),
                    &[],
                    &[],
                    &[],
                    &[],
                );
                let profile = MacOsSandbox::generate_profile(&config);
                let escaped_home = "/Users/Test User/quote\\\"backslash\\\\paren(home)";

                assert!(profile.contains(&format!(
                    "(allow file-read* (prefix \"{escaped_home}/.gitconfig\"))"
                )));
                assert!(profile.contains(&format!(
                    "(allow file-read* (subpath \"{escaped_home}/.config/git\"))"
                )));
                assert!(profile.contains(&format!(
                    "(allow file-read* file-write* (subpath \"{escaped_home}/.config/gh\"))"
                )));
                assert!(profile.contains(&format!(
                    "(allow file-read* file-write* (subpath \"{escaped_home}/Library/Keychains\"))"
                )));
                assert!(
                    !profile.contains("quote\"backslash"),
                    "unescaped HOME should not appear in generated profile"
                );
            },
        );
    }

    #[test]
    fn test_read_only_profile_denies_scm_and_keychain_writes() {
        temp_env::with_var("HOME", Some("/Users/testhome"), || {
            let config = SandboxConfig::build_with_policy(
                SandboxPolicy::ReadOnly,
                Path::new("/workspace"),
                &[PathBuf::from("/tmp")],
                &[],
                &[],
                &[],
            );

            let profile = MacOsSandbox::generate_profile(&config);

            // Read-only policy must demote the shared SCM CLI paths and must
            // not re-grant writes through the specialized Keychain rules.
            let scm_rule = "(allow file-read* (subpath \"/Users/testhome/.config/gh\"))";
            assert!(
                profile.contains(scm_rule),
                "read-only profile should keep SCM CLI dirs readable"
            );
            assert_eq!(
                profile.matches(scm_rule).count(),
                1,
                "shared SCM CLI paths must emit exactly one rule"
            );
            assert!(
                !profile.contains(
                    "(allow file-read* file-write* (subpath \"/Users/testhome/.config/gh\"))"
                ),
                "read-only profile must not grant writes to SCM CLI dirs"
            );
            assert!(
                profile
                    .contains("(allow file-read* (subpath \"/Users/testhome/Library/Keychains\"))"),
                "read-only profile should keep user keychains readable"
            );
            assert!(
                !profile.contains(
                    "(allow file-read* file-write* (subpath \"/Users/testhome/Library/Keychains\"))"
                ),
                "read-only profile must not grant writes to user keychains"
            );
        });
    }

    #[test]
    fn test_profile_allows_ssh_directory_access() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        // Should allow reading .ssh directory itself and specific config files
        assert!(
            profile.contains(".ssh\"))"),
            "Expected profile to allow access to .ssh directory"
        );
    }

    #[test]
    fn test_profile_allows_ssh_agent_sockets() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        // Should allow access to SSH agent sockets in /tmp/ssh-*
        assert!(
            profile.contains("^/tmp/ssh-"),
            "Expected profile to allow access to /tmp/ssh-* sockets"
        );
        // Should allow access to SSH agent sockets in /private/tmp/ssh-*
        assert!(
            profile.contains("^/private/tmp/ssh-"),
            "Expected profile to allow access to /private/tmp/ssh-* sockets"
        );
        // Should allow access to launchd-managed SSH agent sockets
        assert!(
            profile.contains("com\\.apple\\.launchd"),
            "Expected profile to allow access to launchd SSH agent sockets"
        );
    }

    #[test]
    fn test_profile_allows_git_xdg_config() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        assert!(
            profile.contains(".config/git"),
            "Expected profile to allow XDG git config directory"
        );
        assert!(
            profile.contains(".gitattributes"),
            "Expected profile to allow .gitattributes file"
        );
    }

    #[test]
    fn test_profile_allows_ssh_config_and_known_hosts() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        assert!(
            profile.contains(".ssh/config"),
            "Expected profile to allow .ssh/config file"
        );
        assert!(
            profile.contains(".ssh/known_hosts"),
            "Expected profile to allow .ssh/known_hosts file"
        );
    }

    #[test]
    fn test_profile_emits_each_shared_scm_cli_path_once() {
        temp_env::with_var("HOME", Some("/Users/testhome"), || {
            let config = SandboxConfig::build_with_policy(
                SandboxPolicy::WorkspaceWrite,
                Path::new("/workspace"),
                &[],
                &[],
                &[],
                &[],
            );
            let profile = MacOsSandbox::generate_profile(&config);

            for &relative in crate::clients::tools::sandbox::SCM_CLI_PATHS {
                let rule = format!(
                    "(allow file-read* file-write* (subpath \"/Users/testhome/{relative}\"))"
                );
                assert_eq!(
                    profile.matches(&rule).count(),
                    1,
                    "shared SCM CLI path must emit exactly one rule: {relative}"
                );
            }
        });
    }

    #[test]
    fn test_profile_does_not_allow_full_ssh_subpath() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        // Should NOT grant broad subpath read to .ssh (only specific files)
        assert!(
            !profile.contains(".ssh\"))")
                || profile.contains("(literal \"") && profile.contains(".ssh/"),
            "Profile should not use subpath for .ssh access"
        );
    }

    #[test]
    fn test_profile_includes_ancestor_literals_for_all_read_write_paths() {
        let config = SandboxConfig {
            writable: vec![
                PathBuf::from("/workspace/project"),
                PathBuf::from("/private/var/folders"),
            ],
            read_execute: vec![PathBuf::from("/usr"), PathBuf::from("/private/etc")],
            policy: SandboxPolicy::WorkspaceWrite,
        };

        let profile = MacOsSandbox::generate_profile(&config);

        // Ancestors of /workspace/project
        assert!(
            profile.contains("(allow file-read* (literal \"/workspace\"))"),
            "Expected ancestor literal for /workspace"
        );

        // Ancestors of /private/var/folders (not including the path itself or root)
        assert!(
            profile.contains("(allow file-read* (literal \"/private\"))"),
            "Expected ancestor literal for /private"
        );
        assert!(
            profile.contains("(allow file-read* (literal \"/private/var\"))"),
            "Expected ancestor literal for /private/var"
        );
        // Note: /private/var/folders is the path itself, not an ancestor, so it gets
        // a subpath rule (read-write), not a literal rule

        // Ancestors of /private/etc
        assert!(
            profile.contains("(allow file-read* (literal \"/private\"))"),
            "Expected ancestor literal for /private (shared)"
        );

        // Root should NOT appear as an ancestor literal (it's already covered by the root literal rule)
        assert!(
            !profile
                .contains("(allow file-read* (literal \"/\"))\n(allow file-read* (literal \"/\"))")
        );
    }

    #[test]
    fn test_profile_includes_linked_worktree_dirs() {
        // Platform-appropriate enforcement test for linked worktree support.
        // Creates a real git linked worktree, builds a SandboxConfig through
        // the full resolution chain (build_with_policy), generates the Seatbelt
        // profile, and verifies the profile contains allow rules for both the
        // common .git dir and the per-worktree gitdir.
        //
        // This tests profile generation, not enforcement — it runs without
        // sandbox-exec and is therefore deterministic on any macOS host.
        let tmp = tempfile::tempdir().unwrap();
        let main_repo = tmp.path().join("main");
        let wt_path = tmp.path().join("linked-wt");

        // Initialize main repo with a commit and a linked worktree
        crate::config::git::test_support::init_repo_with_linked_worktree(&main_repo, &wt_path);

        // Build sandbox config from the worktree using the full resolution chain
        let config = SandboxConfig::build_with_policy(
            SandboxPolicy::WorkspaceWrite,
            &wt_path,
            &[],
            &[],
            &[],
            &[],
        );

        // Generate the Seatbelt profile from this config
        let profile = MacOsSandbox::generate_profile(&config);

        // The common .git directory must have a read-write subpath rule
        let main_git = main_repo.join(".git").canonicalize().unwrap();
        let common_rule = format!(
            "(allow file-read* file-write* (subpath \"{}\"))",
            main_git.display()
        );
        assert!(
            profile.contains(&common_rule),
            "profile must contain allow rule for common .git dir: {common_rule}"
        );

        // The per-worktree gitdir must also have a read-write subpath rule.
        // Parse the exact path from the .git file to assert on the precise
        // directory rather than any descendant.
        let content = std::fs::read_to_string(wt_path.join(".git")).unwrap();
        let gitdir_line = content
            .lines()
            .find(|l| l.trim().starts_with("gitdir:"))
            .expect(".git file must contain gitdir:");
        let gitdir_raw = gitdir_line
            .strip_prefix("gitdir: ")
            .or_else(|| gitdir_line.strip_prefix("gitdir:"))
            .map(str::trim)
            .unwrap();
        let gitdir_path = if Path::new(gitdir_raw).is_relative() {
            wt_path.join(gitdir_raw)
        } else {
            PathBuf::from(gitdir_raw)
        };
        let canonical_gitdir = gitdir_path.canonicalize().unwrap();
        let wt_rule = format!(
            "(allow file-read* file-write* (subpath \"{}\"))",
            canonical_gitdir.display()
        );
        assert!(
            profile.contains(&wt_rule),
            "profile must contain allow rule for worktree gitdir: {wt_rule}"
        );
    }

    #[test]
    fn test_launchd_regex_in_generated_profile_matches_representative_path() {
        let profile = MacOsSandbox::generate_profile(&test_config());
        let pattern = profile
            .lines()
            .find_map(|line| {
                line.strip_prefix("(allow file-read* file-write* (regex #\"")
                    .and_then(|line| line.strip_suffix("\"))"))
                    .filter(|pattern| pattern.contains("launchd"))
            })
            .expect("profile must contain the launchd SSH agent regex");
        let (prefix, suffix) = pattern
            .split_once(".*")
            .expect("launchd regex must contain a wildcard suffix");
        let prefix = prefix
            .strip_prefix('^')
            .expect("launchd regex must be anchored")
            .replace("\\.", ".");
        let suffix = suffix
            .strip_suffix('$')
            .expect("launchd regex must be end-anchored")
            .replace("\\.", ".");
        let path = "/private/tmp/com.apple.launchd.XXXX/Listeners";
        let wildcard_match = path
            .strip_prefix(&prefix)
            .is_some_and(|remainder| remainder.ends_with(&suffix));

        assert!(wildcard_match, "pattern {pattern} must match {path}");
    }
}

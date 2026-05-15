//! macOS sandbox implementation using `sandbox-exec`
//!
//! Uses the Seatbelt sandbox profile language (Scheme-like syntax) to
//! generate dynamic sandbox profiles that restrict filesystem access.
//! The profile uses a deny-default policy: everything is denied unless
//! explicitly allowed.

use crate::clients::tools::sandbox::{SandboxConfig, SandboxStrategy};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
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
        static CAN_APPLY: OnceLock<bool> = OnceLock::new();
        *CAN_APPLY.get_or_init(Self::probe_can_apply_profile)
    }

    fn probe_can_apply_profile() -> bool {
        let tmp_dir = std::env::temp_dir().join("cake").join("sandbox_profiles");
        if let Err(e) = std::fs::create_dir_all(&tmp_dir) {
            tracing::warn!("Failed to create sandbox profile probe directory: {e}");
            return false;
        }

        let mut temp_file = match tempfile::Builder::new()
            .prefix("cake_sandbox_probe_")
            .suffix(".sb")
            .tempfile_in(&tmp_dir)
        {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!("Failed to create sandbox profile probe file: {e}");
                return false;
            },
        };

        if let Err(e) = temp_file.write_all(b"(version 1)\n(allow default)\n") {
            tracing::warn!("Failed to write sandbox profile probe file: {e}");
            return false;
        }

        let output = std::process::Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(temp_file.path())
            .arg("/bin/echo")
            .arg("cake-sandbox-probe")
            .output();

        match output {
            Ok(output) if output.status.success() => true,
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(
                    "macOS sandbox-exec is present but cannot apply profiles: {}",
                    stderr.trim()
                );
                false
            },
            Err(e) => {
                tracing::warn!("Failed to run sandbox-exec probe: {e}");
                false
            },
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
            "^/private/tmp/com\\.apple\\.launchd\\.*/Listeners",
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

    /// Append SCM CLI (gh, glab) configuration, cache, and state rules to the profile.
    fn append_scm_cli_rules(profile: &mut SeatbeltProfileBuilder) {
        profile.comment("SCM CLIs: GitHub CLI (gh) and GitLab CLI (glab)");
        if let Some(home) = home_dir() {
            // GitHub CLI
            profile.allow_subpath("file-read* file-write*", home.join(".config/gh"));
            profile.allow_subpath("file-read* file-write*", home.join(".cache/gh"));
            profile.allow_subpath("file-read* file-write*", home.join(".local/share/gh"));
            profile.allow_subpath("file-read* file-write*", home.join(".local/state/gh"));
            // GitLab CLI
            profile.allow_subpath("file-read* file-write*", home.join(".config/glab-cli"));
            profile.allow_subpath("file-read* file-write*", home.join(".cache/glab-cli"));
            profile.allow_subpath("file-read* file-write*", home.join(".local/share/glab-cli"));
            profile.allow_subpath("file-read* file-write*", home.join(".local/state/glab-cli"));
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
    fn append_keychain_rules(profile: &mut SeatbeltProfileBuilder) {
        profile.comment(
            "macOS Keychain file access (supplementary; primary access is via mach-lookup)",
        );
        profile.allow_subpath("file-read*", "/Library/Keychains");
        profile.allow_subpath("file-read*", "/System/Library/Keychains");
        if let Some(home) = home_dir() {
            profile.allow_subpath("file-read* file-write*", home.join("Library/Keychains"));
        }
        profile.blank();
    }

    /// Generate a deny-default sandbox profile (.sb file content) from the configuration
    fn generate_profile(config: &SandboxConfig) -> String {
        let mut profile = SeatbeltProfileBuilder::deny_default();

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

        // Ancestor directory literals for all read-write, read-only, and system paths.
        // (agents and tools call readdir() and stat() on ancestors to traverse paths)
        profile.comment("Allow reading ancestor directories of allowed paths");
        let mut ancestor_set = std::collections::BTreeSet::new();
        for path in config
            .writable
            .iter()
            .chain(&config.system_paths)
            .chain(&config.readable)
        {
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
                profile.allow_subpath("file-read* file-write*", path);
            }
            profile.blank();
        }

        // Read + execute access for system paths
        if !config.system_paths.is_empty() {
            profile.comment("Read + execute access: system paths");
            for path in &config.system_paths {
                profile.allow_subpath("file-read*", path);
            }
            profile.blank();
        }

        // Read-only access for config/device paths
        if !config.readable.is_empty() {
            profile.comment("Read-only access: config and device paths");
            for path in &config.readable {
                profile.allow_subpath("file-read*", path);
            }
            profile.blank();
        }

        Self::append_git_rules(&mut profile);
        Self::append_ssh_agent_rules(&mut profile);
        Self::append_scm_cli_rules(&mut profile);
        Self::append_keychain_rules(&mut profile);
        Self::append_device_rules(&mut profile);

        // Allow file-ioctl scoped to terminal devices
        profile.comment("Allow file-ioctl for terminal operations");
        profile.allow("file-ioctl");

        // Allow file locking (needed by cargo and other build tools)
        profile.comment("Allow file locking (needed by cargo and other build tools)");
        profile.allow("file-lock");

        profile.finish()
    }

    /// Write the profile to a temp file and return its path
    fn write_profile_to_temp(profile: &str) -> Result<tempfile::NamedTempFile, String> {
        use std::io::Write;

        let tmp_dir = std::env::temp_dir().join("cake").join("sandbox_profiles");
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| format!("Failed to create sandbox profile directory: {e}"))?;

        let mut temp_file = tempfile::Builder::new()
            .prefix("cake_sandbox_")
            .suffix(".sb")
            .tempfile_in(&tmp_dir)
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

impl SandboxStrategy for MacOsSandbox {
    fn apply(
        &self,
        command: &mut tokio::process::Command,
        config: &SandboxConfig,
    ) -> Result<(), String> {
        let profile = Self::generate_profile(config);
        tracing::debug!("Generated sandbox profile:\n{profile}");

        // Write profile to temp file — persist so sandbox-exec can read it at spawn time
        let temp_file = Self::write_profile_to_temp(&profile)?;
        let profile_path = temp_file.into_temp_path();

        // Get the original command arguments
        let original_args: Vec<String> = command
            .as_std()
            .get_args()
            .map(|s| s.to_string_lossy().to_string())
            .collect();

        // Reconfigure the command to use sandbox-exec
        *command = tokio::process::Command::new("/usr/bin/sandbox-exec");

        command.arg("-f").arg(profile_path.as_os_str());

        // Add the original program (bash) and its arguments
        command.arg("bash");
        for arg in original_args {
            command.arg(arg);
        }

        // Re-apply stdio configuration
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Leak the TempPath so the file persists until process exit.
        // The OS will clean up temp files.
        std::mem::forget(profile_path);

        tracing::debug!("Sandboxed command configured with deny-default profile");

        Ok(())
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

struct SeatbeltProfileBuilder {
    lines: Vec<String>,
}

impl SeatbeltProfileBuilder {
    fn deny_default() -> Self {
        Self {
            lines: vec![
                "(version 1)".to_string(),
                "(deny default)".to_string(),
                String::new(),
            ],
        }
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

    fn escape_path(path: &Path) -> String {
        path.to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
    }

    fn finish(self) -> String {
        self.lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> SandboxConfig {
        SandboxConfig {
            writable: vec![PathBuf::from("/workspace")],
            system_paths: vec![PathBuf::from("/usr"), PathBuf::from("/bin")],
            readable: vec![PathBuf::from("/etc")],
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
    fn test_profile_allows_read_write_paths() {
        let config = SandboxConfig {
            writable: vec![PathBuf::from("/workspace"), PathBuf::from("/tmp")],
            system_paths: vec![],
            readable: vec![],
        };

        let profile = MacOsSandbox::generate_profile(&config);

        assert!(profile.contains("(allow file-read* file-write* (subpath \"/workspace\"))"));
        assert!(profile.contains("(allow file-read* file-write* (subpath \"/tmp\"))"));
    }

    #[test]
    fn test_profile_allows_system_paths_with_read_and_exec() {
        let config = SandboxConfig {
            writable: vec![],
            system_paths: vec![PathBuf::from("/usr"), PathBuf::from("/bin")],
            readable: vec![],
        };

        let profile = MacOsSandbox::generate_profile(&config);

        assert!(profile.contains("(allow file-read* (subpath \"/usr\"))"));
        assert!(profile.contains("(allow file-read* (subpath \"/bin\"))"));
    }

    #[test]
    fn test_profile_allows_read_only_paths() {
        let config = SandboxConfig {
            writable: vec![],
            system_paths: vec![],
            readable: vec![PathBuf::from("/etc")],
        };

        let profile = MacOsSandbox::generate_profile(&config);

        assert!(profile.contains("(allow file-read* (subpath \"/etc\"))"));
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
    fn test_profile_escapes_home_based_paths() {
        temp_env::with_var(
            "HOME",
            Some("/Users/Test User/quote\"backslash\\paren(home)"),
            || {
                let profile = MacOsSandbox::generate_profile(&test_config());
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
    fn test_profile_allows_gh_cli_paths() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        assert!(
            profile.contains(".config/gh"),
            "Expected profile to allow gh config directory"
        );
        assert!(
            profile.contains(".cache/gh"),
            "Expected profile to allow gh cache directory"
        );
        assert!(
            profile.contains(".local/share/gh"),
            "Expected profile to allow gh share directory"
        );
        assert!(
            profile.contains(".local/state/gh"),
            "Expected profile to allow gh state directory"
        );
    }

    #[test]
    fn test_profile_allows_glab_cli_paths() {
        let profile = MacOsSandbox::generate_profile(&test_config());

        assert!(
            profile.contains(".config/glab-cli"),
            "Expected profile to allow glab config directory"
        );
        assert!(
            profile.contains(".cache/glab-cli"),
            "Expected profile to allow glab cache directory"
        );
        assert!(
            profile.contains(".local/share/glab-cli"),
            "Expected profile to allow glab share directory"
        );
        assert!(
            profile.contains(".local/state/glab-cli"),
            "Expected profile to allow glab state directory"
        );
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
            system_paths: vec![PathBuf::from("/usr")],
            readable: vec![PathBuf::from("/private/etc")],
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
}

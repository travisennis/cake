use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, anyhow};
use tracing::info;

/// Adjectives for random worktree name generation.
const ADJECTIVES: &[&str] = &[
    "bright", "calm", "cool", "dark", "fast", "keen", "pure", "soft", "warm", "wild", "bold",
    "crisp", "fair", "glad", "kind", "neat", "rare", "safe", "tall", "vast",
];

/// Nouns for random worktree name generation.
const NOUNS: &[&str] = &[
    "arch", "beam", "core", "dart", "edge", "flux", "gate", "hive", "iris", "jade", "kite", "leaf",
    "mesa", "node", "opal", "pine", "quay", "reef", "sage", "tide",
];

/// Represents an active git worktree managed by cake.
#[derive(Debug, Clone)]
pub struct Worktree {
    /// The name of the worktree (user-provided or auto-generated).
    pub name: String,
    /// The filesystem path to the worktree directory.
    pub path: PathBuf,
}

/// Generate a random worktree name like "bright-core-a1b2".
pub fn generate_name() -> String {
    let uuid = uuid::Uuid::new_v4();
    let bytes = uuid.as_bytes();

    let adj = ADJECTIVES[bytes[0] as usize % ADJECTIVES.len()];
    let noun = NOUNS[bytes[1] as usize % NOUNS.len()];
    #[expect(
        clippy::string_slice,
        reason = "hex encoding always produces ASCII output"
    )]
    let suffix = &hex::encode(&bytes[2..4])[..4];

    format!("{adj}-{noun}-{suffix}")
}

/// Find the root of the current git repository.
fn find_repo_root(from: &Path) -> anyhow::Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(from)
        .output()
        .context("Failed to run git rev-parse")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Not a git repository: {stderr}"));
    }

    let root = String::from_utf8(output.stdout)
        .context("Invalid UTF-8 in git output")?
        .trim()
        .to_string();

    Ok(PathBuf::from(root))
}

/// Get the default remote branch (e.g., origin/main).
fn default_remote_branch(repo_root: &Path) -> anyhow::Result<String> {
    // Try origin/HEAD first
    let output = Command::new("git")
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .current_dir(repo_root)
        .output()
        .context("Failed to run git symbolic-ref")?;

    if output.status.success() {
        let full_ref = String::from_utf8(output.stdout)
            .context("Invalid UTF-8")?
            .trim()
            .to_string();
        // Strip "refs/remotes/" prefix
        if let Some(branch) = full_ref.strip_prefix("refs/remotes/") {
            return Ok(branch.to_string());
        }
    }

    // Fallback: try common defaults
    for candidate in &["origin/main", "origin/master"] {
        let check = Command::new("git")
            .args(["rev-parse", "--verify", candidate])
            .current_dir(repo_root)
            .output()
            .context("Failed to verify branch")?;

        if check.status.success() {
            return Ok((*candidate).to_string());
        }
    }

    // Last resort: use HEAD
    Ok("HEAD".to_string())
}

/// Returns the worktrees base directory: `<repo>/.cake/worktrees/`.
fn worktrees_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".cake").join("worktrees")
}

/// Create a new worktree with the given name. If `name` is `None`, a random name is generated.
///
/// The worktree is created at `<repo>/.cake/worktrees/<name>` with a new branch
/// `worktree-<name>` based on the default remote branch.
///
/// If a `.worktreeinclude` file exists at the repo root, files matching its
/// glob patterns are copied from the source tree into the new worktree.
pub fn create(from: &Path, name: Option<&str>) -> anyhow::Result<Worktree> {
    let repo_root = find_repo_root(from)?;
    let wt_name = name.map_or_else(generate_name, ToString::to_string);
    let branch = format!("worktree-{wt_name}");
    let wt_path = worktrees_dir(&repo_root).join(&wt_name);

    if wt_path.exists() {
        return Err(anyhow!(
            "Worktree '{wt_name}' already exists at {}",
            wt_path.display()
        ));
    }

    let start_point = default_remote_branch(&repo_root)?;

    info!(
        "Creating worktree '{wt_name}' at {} (branch: {branch}, from: {start_point})",
        wt_path.display()
    );

    let output = Command::new("git")
        .args([
            "worktree",
            "add",
            &wt_path.to_string_lossy(),
            "-b",
            &branch,
            &start_point,
        ])
        .current_dir(&repo_root)
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to create worktree: {stderr}"));
    }

    // Copy files listed in .worktreeinclude from the source to the new worktree.
    copy_worktree_includes(&repo_root, &wt_path)?;

    Ok(Worktree {
        name: wt_name,
        path: wt_path,
    })
}

/// Check if a worktree has uncommitted changes or commits ahead of its start point.
pub fn has_changes(wt_path: &Path) -> anyhow::Result<bool> {
    // Check for uncommitted changes (staged or unstaged)
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(wt_path)
        .output()
        .context("Failed to run git status")?;

    if !status.status.success() {
        return Err(anyhow!("Failed to check worktree status"));
    }

    let status_output = String::from_utf8_lossy(&status.stdout);
    if !status_output.trim().is_empty() {
        return Ok(true);
    }

    // Check for commits not on the default remote branch
    let log = Command::new("git")
        .args(["log", "--oneline", "@{upstream}..HEAD"])
        .current_dir(wt_path)
        .output();

    // If upstream doesn't exist, check if there are any commits at all
    match log {
        Ok(output) if output.status.success() => {
            let log_output = String::from_utf8_lossy(&output.stdout);
            Ok(!log_output.trim().is_empty())
        },
        _ => Err(anyhow!(
            "Cannot determine whether worktree has changes: no upstream branch configured"
        )),
    }
}

/// Remove a worktree by name. Deletes the worktree directory and its branch.
///
/// Set `force` to `true` to remove even if there are uncommitted changes.
pub fn remove(from: &Path, name: &str, force: bool) -> anyhow::Result<()> {
    let repo_root = find_repo_root(from)?;
    let wt_path = worktrees_dir(&repo_root).join(name);
    let branch = format!("worktree-{name}");

    if !wt_path.exists() {
        return Err(anyhow!("Worktree '{name}' not found"));
    }

    info!("Removing worktree '{name}' at {}", wt_path.display());

    // Remove the worktree
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let wt_path_str = wt_path.to_string_lossy().to_string();
    args.push(&wt_path_str);

    let output = Command::new("git")
        .args(&args)
        .current_dir(&repo_root)
        .output()
        .context("Failed to run git worktree remove")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("Failed to remove worktree: {stderr}"));
    }

    // Delete the branch
    let branch_flag = if force { "-D" } else { "-d" };
    let branch_output = Command::new("git")
        .args(["branch", branch_flag, &branch])
        .current_dir(&repo_root)
        .output()
        .context("Failed to delete branch")?;

    if !branch_output.status.success() {
        let stderr = String::from_utf8_lossy(&branch_output.stderr);
        // Don't fail if branch was already deleted
        if !stderr.contains("not found") {
            tracing::warn!("Failed to delete branch '{branch}': {stderr}");
        }
    }

    Ok(())
}

// ── .worktreeinclude support ──────────────────────────────────────────────

/// The name of the control file that lists files to copy into new worktrees.
const WORKTREE_INCLUDE_FILENAME: &str = ".worktreeinclude";

/// Parse `.worktreeinclude` from `source_root` and return the list of
/// non-empty, non-comment lines (glob patterns).
fn parse_worktree_includes(source_root: &Path) -> anyhow::Result<Vec<String>> {
    let path = source_root.join(WORKTREE_INCLUDE_FILENAME);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {WORKTREE_INCLUDE_FILENAME}"))?;

    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(ToOwned::to_owned)
        .collect())
}

/// Copy files matching glob patterns from `source_root` to `dest_root`.
///
/// Patterns are read from `.worktreeinclude` in `source_root`.  If the file
/// does not exist or contains no patterns, this is a no-op.
fn copy_worktree_includes(source_root: &Path, dest_root: &Path) -> anyhow::Result<()> {
    let patterns = parse_worktree_includes(source_root)?;
    if patterns.is_empty() {
        return Ok(());
    }

    let mut copied = 0;
    collect_matching_files(source_root, source_root, &patterns, dest_root, &mut copied)
        .with_context(|| "Failed to copy .worktreeinclude files")?;

    if copied > 0 {
        info!("Copied {copied} file(s) matched by {WORKTREE_INCLUDE_FILENAME}");
    }

    Ok(())
}

/// Recursively walk `dir` (under `base`), match each file against `patterns`,
/// and copy matches to the corresponding path under `dest_root`.
fn collect_matching_files(
    base: &Path,
    dir: &Path,
    patterns: &[String],
    dest_root: &Path,
    copied: &mut usize,
) -> anyhow::Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("Failed to read dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let name = entry.file_name();
            // Skip internal git and cake directories.
            if name == ".git" || name == ".cake" {
                continue;
            }
            collect_matching_files(base, &path, patterns, dest_root, copied)?;
        } else if path.is_file() {
            let relative = path.strip_prefix(base).with_context(|| {
                format!(
                    "path {} is not under base {}",
                    path.display(),
                    base.display()
                )
            })?;
            let relative_str = relative.to_string_lossy();

            if patterns.iter().any(|p| glob_match(p, &relative_str)) {
                let dest = dest_root.join(relative);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("Failed to create dir {}", parent.display()))?;
                }
                std::fs::copy(&path, &dest).with_context(|| {
                    format!("Failed to copy {} to {}", path.display(), dest.display())
                })?;
                *copied += 1;
            }
        }
    }
    Ok(())
}

/// Simple glob-pattern matcher for worktree include patterns.
///
/// Supports:
/// - `*` — matches any characters except `/`
/// - `**` — matches any characters including `/`
/// - `?` — matches a single character except `/`
///
/// Everything else is matched literally.  Anchoring to the start of the path
/// is implicit (the whole path must match).
fn glob_match(pattern: &str, path: &str) -> bool {
    glob_match_bytes(pattern.as_bytes(), path.as_bytes())
}

/// Byte-level recursive glob matcher.
fn glob_match_bytes(pattern: &[u8], path: &[u8]) -> bool {
    // Base: empty pattern matches empty path.
    if pattern.is_empty() {
        return path.is_empty();
    }

    // Handle `**` (match everything including `/`).
    if pattern.len() >= 2 && pattern[0] == b'*' && pattern[1] == b'*' {
        // Skip past `**` and an optional following `/`.
        let rest = if pattern.len() > 2 && pattern[2] == b'/' {
            &pattern[3..]
        } else {
            &pattern[2..]
        };
        // Try matching the rest of the pattern at every possible position.
        for start in 0..=path.len() {
            if glob_match_bytes(rest, &path[start..]) {
                return true;
            }
        }
        return false;
    }

    // Handle `*` (match anything except `/`).
    if pattern[0] == b'*' {
        let rest = &pattern[1..];
        // Try matching 0 or more characters, stopping at `/`.
        for end in 0..=path.len() {
            if end > 0 && path[end - 1] == b'/' {
                break;
            }
            if glob_match_bytes(rest, &path[end..]) {
                return true;
            }
        }
        return false;
    }

    // If the path is empty but the pattern isn't (and isn't a star), no match.
    if path.is_empty() {
        return false;
    }

    // Handle `?` (match any single char except `/`).
    if pattern[0] == b'?' {
        if path[0] == b'/' {
            return false;
        }
        return glob_match_bytes(&pattern[1..], &path[1..]);
    }

    // Literal character match.
    pattern[0] == path[0] && glob_match_bytes(&pattern[1..], &path[1..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Shorthand for `anyhow::Result` in tests.
    type TestResult = anyhow::Result<()>;

    /// Create a temporary git repo for testing.
    fn init_test_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        // Configure git user for CI environments where global config may not exist
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "initial"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn test_generate_name_format() {
        let name = generate_name();
        let parts: Vec<&str> = name.split('-').collect();
        assert_eq!(parts.len(), 3, "Name should have 3 parts: {name}");
        assert_eq!(parts[2].len(), 4, "Suffix should be 4 hex chars");
    }

    #[test]
    fn test_find_repo_root() {
        let dir = init_test_repo();
        let root = find_repo_root(dir.path()).unwrap();
        assert_eq!(
            root.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn test_create_worktree() {
        let dir = init_test_repo();

        let wt = create(dir.path(), Some("test-wt")).unwrap();
        assert_eq!(wt.name, "test-wt");
        assert!(wt.path.exists());
    }

    #[test]
    fn test_create_duplicate_fails() {
        let dir = init_test_repo();
        create(dir.path(), Some("dup")).unwrap();
        let result = create(dir.path(), Some("dup"));
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_clean_worktree() {
        let dir = init_test_repo();
        let wt = create(dir.path(), Some("removable")).unwrap();
        assert!(wt.path.exists());

        remove(dir.path(), "removable", false).unwrap();
        assert!(!wt.path.exists());
    }

    #[test]
    fn test_has_changes_clean() {
        let dir = init_test_repo();
        let wt = create(dir.path(), Some("clean")).unwrap();
        // Clean worktree in a no-remote repo has no upstream, so
        // has_changes returns an error (indeterminate state) rather
        // than silently returning "clean".
        let result = has_changes(&wt.path);
        assert!(
            result.is_err(),
            "clean worktree without upstream should return error, got: {result:?}"
        );
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no upstream") || msg.contains("Cannot determine"),
            "error should mention missing upstream: {msg}"
        );
    }

    #[test]
    fn test_has_changes_dirty() {
        let dir = init_test_repo();
        let wt = create(dir.path(), Some("dirty")).unwrap();

        // Create a new file to make it dirty
        std::fs::write(wt.path.join("new_file.txt"), "content").unwrap();
        assert!(has_changes(&wt.path).unwrap());
    }

    // ── glob_match tests ────────────────────────────────────────────────────

    #[test]
    fn test_glob_match_literal() {
        assert!(glob_match(".env", ".env"));
        assert!(!glob_match(".env", ".env.local"));
        assert!(!glob_match(".env", "subdir/.env"));
    }

    #[test]
    fn test_glob_match_star() {
        assert!(glob_match("*.env", ".env"));
        assert!(glob_match("*.env", "foo.env"));
        assert!(glob_match("*.env", "bar.env"));
        assert!(!glob_match("*.env", "subdir/.env"));
        assert!(!glob_match("*.env", "foo.env.bak"));
    }

    #[test]
    fn test_glob_match_star_no_cross_dir() {
        // Single * should NOT cross directory boundaries.
        assert!(!glob_match("config/*.json", "config/sub/secrets.json"));
        assert!(glob_match("config/*.json", "config/secrets.json"));
    }

    #[test]
    fn test_glob_match_doublestar() {
        assert!(glob_match("**/secrets.json", "secrets.json"));
        assert!(glob_match("**/secrets.json", "config/secrets.json"));
        assert!(glob_match("**/secrets.json", "a/b/c/secrets.json"));
        assert!(!glob_match("**/secrets.json", "secrets.toml"));
    }

    #[test]
    fn test_glob_match_doublestar_dir_prefix() {
        assert!(glob_match("config/**/secrets.json", "config/secrets.json"));
        assert!(glob_match(
            "config/**/secrets.json",
            "config/sub/secrets.json"
        ));
        assert!(!glob_match("config/**/secrets.json", "config/secrets.toml"));
    }

    #[test]
    fn test_glob_match_question_mark() {
        assert!(glob_match(".env.?", ".env.x"));
        assert!(!glob_match(".env.?", ".env.xy"));
        assert!(!glob_match(".env.?", ".env./"));
    }

    #[test]
    fn test_glob_match_empty_pattern() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "a"));
    }

    #[test]
    fn test_glob_match_star_matches_empty() {
        assert!(glob_match("*", ""));
        assert!(glob_match("*", "a"));
        assert!(!glob_match("*", "a/b"));
    }

    #[test]
    fn test_glob_match_doublestar_matches_empty() {
        assert!(glob_match("**", ""));
        assert!(glob_match("**", "a"));
        assert!(glob_match("**", "a/b/c"));
    }

    // ── parse_worktree_includes tests ────────────────────────────────────────

    #[test]
    fn test_parse_worktree_includes_no_file() -> TestResult {
        let dir = TempDir::new()?;
        let patterns = parse_worktree_includes(dir.path())?;
        assert!(patterns.is_empty());
        Ok(())
    }

    #[test]
    fn test_parse_worktree_includes_basic() -> TestResult {
        let dir = TempDir::new()?;
        let include_path = dir.path().join(".worktreeinclude");
        fs::write(&include_path, ".env\nsecrets.json\n")?;
        let patterns = parse_worktree_includes(dir.path())?;
        assert_eq!(patterns.len(), 2);
        assert_eq!(patterns[0], ".env");
        assert_eq!(patterns[1], "secrets.json");
        Ok(())
    }

    #[test]
    fn test_parse_worktree_includes_skips_comments_and_blanks() -> TestResult {
        let dir = TempDir::new()?;
        let include_path = dir.path().join(".worktreeinclude");
        fs::write(
            &include_path,
            "# This is a comment\n\n.env\n# Another comment\n\nsecrets.json\n",
        )?;
        let patterns = parse_worktree_includes(dir.path())?;
        assert_eq!(patterns.len(), 2);
        assert_eq!(patterns[0], ".env");
        assert_eq!(patterns[1], "secrets.json");
        Ok(())
    }

    #[test]
    fn test_parse_worktree_includes_trims_whitespace() -> TestResult {
        let dir = TempDir::new()?;
        let include_path = dir.path().join(".worktreeinclude");
        fs::write(&include_path, "  .env  \n\tsecrets.json\n")?;
        let patterns = parse_worktree_includes(dir.path())?;
        assert_eq!(patterns[0], ".env");
        assert_eq!(patterns[1], "secrets.json");
        Ok(())
    }

    // ── copy_worktree_includes integration tests ────────────────────────────

    #[test]
    fn test_copy_worktree_includes_copies_matching_files() -> TestResult {
        let source = TempDir::new()?;
        let dest = TempDir::new()?;

        // Create source files
        fs::write(source.path().join(".env"), "SECRET=foo")?;
        fs::write(source.path().join("README.md"), "docs")?;
        fs::write(source.path().join("secrets.json"), "{}")?;

        // Create .worktreeinclude with patterns
        fs::write(
            source.path().join(".worktreeinclude"),
            ".env\nsecrets.json\n",
        )?;

        copy_worktree_includes(source.path(), dest.path())?;

        assert!(dest.path().join(".env").exists(), ".env should be copied");
        assert!(
            dest.path().join("secrets.json").exists(),
            "secrets.json should be copied"
        );
        // README.md should NOT be copied since it's not in the patterns.
        assert!(
            !dest.path().join("README.md").exists(),
            "README.md should not be copied"
        );

        // Verify content
        let env_content = fs::read_to_string(dest.path().join(".env"))?;
        assert_eq!(env_content, "SECRET=foo");
        Ok(())
    }

    #[test]
    fn test_copy_worktree_includes_with_subdirs() -> TestResult {
        let source = TempDir::new()?;
        let dest = TempDir::new()?;

        // Create files including in subdirectories
        fs::create_dir_all(source.path().join("config"))?;
        fs::write(source.path().join("config/secrets.json"), "{}")?;
        fs::write(source.path().join(".env"), "SECRET=foo")?;

        // Match specific subdirectory paths
        fs::write(
            source.path().join(".worktreeinclude"),
            "config/secrets.json\n.env\n",
        )?;

        copy_worktree_includes(source.path(), dest.path())?;

        assert!(dest.path().join(".env").exists());
        assert!(dest.path().join("config/secrets.json").exists());
        Ok(())
    }

    #[test]
    fn test_copy_worktree_includes_skips_git_and_cake() -> TestResult {
        let source = TempDir::new()?;
        let dest = TempDir::new()?;

        // Create .git and .cake directories that should be skipped
        fs::create_dir_all(source.path().join(".git/info"))?;
        fs::write(source.path().join(".git/config"), "[core]")?;
        fs::create_dir_all(source.path().join(".cake/worktrees"))?;
        fs::write(source.path().join(".cake/settings.toml"), "")?;
        fs::write(source.path().join(".env"), "SECRET=foo")?;

        // Use ** pattern to match everything
        fs::write(source.path().join(".worktreeinclude"), "**\n")?;

        copy_worktree_includes(source.path(), dest.path())?;

        // .env should be copied
        assert!(dest.path().join(".env").exists());
        // .git and .cake should NOT be copied
        assert!(!dest.path().join(".git").exists());
        assert!(!dest.path().join(".cake").exists());
        Ok(())
    }

    #[test]
    fn test_copy_worktree_includes_nonexistent_pattern() -> TestResult {
        let source = TempDir::new()?;
        let dest = TempDir::new()?;

        // Create .worktreeinclude with a pattern that doesn't match any file
        fs::write(source.path().join(".env"), "SECRET=foo")?;
        fs::write(
            source.path().join(".worktreeinclude"),
            ".env\nmissing.txt\n",
        )?;

        // Should not error; just silently skip non-matching patterns
        copy_worktree_includes(source.path(), dest.path())?;

        assert!(dest.path().join(".env").exists());
        assert!(!dest.path().join("missing.txt").exists());
        Ok(())
    }
}

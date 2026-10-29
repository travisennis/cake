//! Bounded repository context assembled by Cake for action requests.
pub fn repo_state_digest(cwd: &std::path::Path) -> Option<String> {
    let head_ref = find_git_head(cwd)?;
    let head = std::fs::read_to_string(&head_ref).ok()?;
    let branch = head
        .trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
        .or_else(|| {
            let hash = head.trim();
            (!hash.is_empty() && !hash.contains(' ')).then(|| "detached HEAD".to_string())
        })?;
    Some(format!("git repo, branch {branch}"))
}

/// Locate a git work tree's `HEAD` file by walking up from `cwd`, following
/// a `.git` file (linked worktree, submodule) to its git dir.
fn find_git_head(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    for dir in cwd.ancestors() {
        let dot_git = dir.join(".git");
        let Ok(meta) = std::fs::metadata(&dot_git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(dot_git.join("HEAD"));
        }
        if meta.is_file() {
            // Linked-worktree/submodule marker: `gitdir: <path>`.
            let target = std::fs::read_to_string(&dot_git).ok()?;
            let target = target.trim().strip_prefix("gitdir:")?.trim();
            let git_dir = if std::path::Path::new(target).is_absolute() {
                std::path::PathBuf::from(target)
            } else {
                dir.join(target)
            };
            return Some(git_dir.join("HEAD"));
        }
    }
    None
}

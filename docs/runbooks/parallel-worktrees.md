# Working on Branches and Worktrees

Carry a change from an idea to `master` on a branch, and run several changes at once in linked worktrees.

## Branching Is Optional

`master` has a safety-only remote ruleset ([definition](../../.github/rulesets/default-branch-safety.json)) that blocks deletion and force-pushes; it has no required checks and no local branch guard, so a direct commit and push is allowed. Use a branch when the change needs isolation or a worktree beside other in-flight work. Maintainers integrate locally and push `master`; a branch is not a required review step. A branch is not a security boundary; it is a way to keep concurrent changes apart.

## Single change on a branch

### 1. Cut the branch before editing

```bash
just branch feat/turn-limits
```

The recipe fetches `origin` and creates the branch from `origin/master`, not from whatever the local `master` happens to point at. Prefix the branch with its Conventional Commit type, so `feat/`, `fix/`, `docs/`, `refactor/`, `perf/`, `test/`, `build/`, `ci/`, or `chore/`.

`just branch`, `just worktree`, and `just worktree-rm` reject a name outside that shape. `just` interpolates a recipe argument into shell source, so a name Git would accept, such as `feat/x$(...)`, would otherwise run as a command before Git saw it. Names are restricted to letters, digits, dot, underscore, hyphen, and `/`, and every interpolation is shell-quoted.

### 2. Work and verify

Follow the development loop in [CONTRIBUTING.md](../../CONTRIBUTING.md). The pre-push hook runs `just pre-push`, which runs the `just check` gate, the documentation checks, and `git diff --check` over the checkout. Push the branch you have checked out.

### 3. Integrate locally

After verification and documentation assessment are complete, integrate the branch into `master` and push. Fast-forward when the branch is current and its commits are the history you want; squash-merge when the branch history is noise, using a Conventional Commit subject.

```bash
git switch master && git pull --ff-only
git merge --ff-only feat/turn-limits
# Or, for a noisy branch:
# git merge --squash feat/turn-limits && git commit
git push origin master
```

Do not open a GitHub pull request unless the user explicitly asks for one. External contributors push the branch and open a pull request instead; see [CONTRIBUTING.md](../../CONTRIBUTING.md#integration-and-pull-requests). A pull request is opened after the same checks are complete, uses `just pr` for labels and body, and is merged only after explicit user approval and successful review/checks.

### 4. Clean up

```bash
git switch master && git pull
git branch --delete feat/turn-limits
```

## Several changes at once in worktrees

A linked worktree is a second checkout of the same repository at another path, on its own branch, sharing one `.git` directory. Two agents in two worktrees edit different files on different branches with no interference.

### 1. Create a worktree per change

```bash
just worktree feat/turn-limits
just worktree fix/sandbox-read-only
```

Each lands at `.cake/worktrees/<name>` on a branch of the same name cut from `origin/master`. `.gitignore` covers `.cake/worktrees/`, so the checkouts do not appear as untracked content in the parent working tree.

The recipe also copies the untracked local files a fresh checkout does not carry, currently `.local.justfile` and `.claude/settings.local.json`. Keep that list and `.worktreeinclude` in sync when the set changes.

### 2. Point each agent at one worktree

```bash
cd.cake/worktrees/feat/turn-limits
```

Cake can create its own worktree instead, with `cake --worktree <name>`. That path uses the same `.cake/worktrees/` location and reads `.worktreeinclude`, but names the branch `worktree-<name>`, which does not carry a Conventional Commit type prefix. Prefer `just worktree` when the branch name should follow the Conventional Commit prefix convention.

Git hooks live in the shared `.git` directory, so prek hooks are active in every worktree with no extra installation. `mise` may ask you to trust the configuration the first time you enter a new worktree path.

### 3. Finish and remove

```bash
just worktree-rm feat/turn-limits
```

This removes the checkout and deletes the branch. `git worktree remove` refuses to discard uncommitted changes, so a dirty worktree survives the mistake. Run `just worktrees` to see what is currently checked out where.

## Build cost across worktrees

Each worktree has its own `target/`, so a new worktree would rebuild the entire dependency graph from cold. `.mise.toml` installs `sccache` and sets `RUSTC_WRAPPER`, which shares compiled artifacts across every worktree on the machine.

A shared `CARGO_TARGET_DIR` looks like the obvious alternative and is the wrong choice here. Cargo takes an exclusive lock on a target directory, so parallel worktree builds would block on each other instead of running concurrently, and branches with different feature selections would evict each other's artifacts.

Cargo compiles dependencies non-incrementally, so they cache well in sccache while the local crate keeps its incremental edit-compile loop. Run `sccache --show-stats` to confirm cache hits if a worktree build feels slower than expected.

## Merge conflicts in generated files

`ci/complexity-baseline.json` records the functions grandfathered above the CC target. Two branches that both raise an entry will conflict, and a three-way merge can describe neither branch.

Resolve it by taking `master`'s copy and re-applying only your intended entry:

```bash
git checkout --ours ci/complexity-baseline.json # during a merge from master
just cc-check                                    # confirm the remaining overages
```

## Recovery

**Work was committed to `master` but belongs on a branch.** Nothing is lost. Create a branch at the current commit and move `master` back:

```bash
git switch --create feat/turn-limits
git switch master
git reset --hard origin/master
```

Confirm `git log origin/master..feat/turn-limits` lists the commits before running the reset.

# Working on Branches and Worktrees

Carry a change from an idea to a merged pull request without touching `master` directly, and run several changes at once in linked worktrees.

## Enforcement

`master` is protected in two places, and both must be satisfied.

A GitHub ruleset on `master` requires a pull request, requires the non-conditional CI checks to pass, and requires linear history. It has no bypass actors, so a direct push fails for everyone including the repository owner.

The `branch-guard` hook in `prek.toml` runs `scripts/check-branch.sh` at the pre-commit and pre-push stages. It rejects a commit authored on `master` or `main`, and rejects a push whose destination is one of those branches. It exists so the failure arrives immediately rather than after a full `just pre-push` gate.

Neither guard is a security boundary. Both stop the ordinary mistake of forgetting to branch.

## Single change on a branch

### 1. Cut the branch before editing

```bash
just branch feat/turn-limits
```

The recipe fetches `origin` and creates the branch from `origin/master`, not from whatever the local `master` happens to point at. Prefix the branch with its Conventional Commit type, so `feat/`, `fix/`, `docs/`, `refactor/`, `perf/`, `test/`, `build/`, `ci/`, or `chore/`.

`just branch`, `just worktree`, and `just worktree-rm` reject a name outside that shape. `just` interpolates a recipe argument into shell source, so a name Git would accept, such as `feat/x$(...)`, would otherwise run as a command before Git saw it. Names are restricted to letters, digits, dot, underscore, hyphen, and `/`, and every interpolation is shell-quoted.

Managed work starts here too: set the GitHub issue's Status to In Progress on the branch (see [docs/workflow/tasks.md](../workflow/tasks.md)), so the issue and its implementation travel together.

### 2. Work and verify

Follow the development loop in [CONTRIBUTING.md](../../CONTRIBUTING.md). The pre-push hook runs `just pre-push`, which is the full `just ci` gate.

### 3. Open the pull request

Open the pull request only after the change is ready to merge. Complete the acceptance notes, routed verification, documentation assessment, and any ExecPlan archival first. Use the pull request template, include `Closes #<number>` for the managed issue, and leave the issue open for review; the issue closes when the merged pull request reaches `master`.

```bash
git push -u origin HEAD
just pr
```

`just pr` runs `gh pr create --base master --fill`. Confirm that the generated PR body still contains the closing keyword. CI runs the same checks the ruleset requires. Merge once review and checks are complete; after merge, add the delivered/verified summary to the closed issue and update its completed-plan link if needed.

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
cd .cake/worktrees/feat/turn-limits
```

Cake can create its own worktree instead, with `cake --worktree <name>`. That path uses the same `.cake/worktrees/` location and reads `.worktreeinclude`, but names the branch `worktree-<name>`, which does not carry a Conventional Commit type prefix. Prefer `just worktree` when the work will become a pull request.

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

`ci/cargo-crap-baseline.json` is generated by `just change-risk-baseline` and committed alongside the change that moves it. Two branches that both regenerate it will conflict, and a three-way merge produces a baseline that describes neither branch.

Resolve it by taking `master`'s copy and regenerating:

```bash
git checkout --ours ci/cargo-crap-baseline.json   # during a merge from master
just change-risk-baseline
```

ExecPlan and research records are committed markdown under `docs/`, so parallel branches never conflict on them; a merge of moved files is a plain rename.

## Coordinating which agent takes which issue

GitHub issue state (Status field, assignee) is shared across worktrees. Claim an issue by setting Status to In Progress and assigning yourself before starting; two agents should not claim the same issue. Confirm the assignment before starting parallel work.

## Recovery

**A commit was rejected by `branch-guard` and the work is on `master`.** Nothing is lost; the working tree is untouched. Move it to a branch and commit there:

```bash
git switch --create feat/turn-limits
```

**Work was committed to local `master` before the guard existed.** Move the commits onto a branch and reset `master` to the remote:

```bash
git switch --create feat/turn-limits
git switch master
git reset --hard origin/master
```

Confirm `git log origin/master..feat/turn-limits` lists the commits before running the reset.

**A pull request cannot merge because a required check never ran.** The `coverage` job in `.github/workflows/ci.yml` is conditional on changed paths and is deliberately not a required check. If a required check is stuck pending, verify the ruleset's required-checks list still matches the job names in the workflow.

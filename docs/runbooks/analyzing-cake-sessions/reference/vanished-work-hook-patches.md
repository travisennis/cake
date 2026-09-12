# Vanished Work: Hook-Runner Worktree Patches

Read-only procedure for one symptom: a session reports that uncommitted work disappeared, or shows a clean worktree it cannot explain, while `HEAD`, the reflog, and `git stash list` contain nothing that accounts for it.

This branch exists because the obvious Git reasoning has a hole. A hook runner that manages unstaged changes can move files out of the working tree without leaving a commit, a stash entry, a reflog entry, or a branch move. When the runner is killed before it restores them, the checkout stays clean and the only surviving record is a patch file in the runner's own cache.

Load this from Phase 4 of the [runbook](../index.md). The session-side queries it relies on are in [jq-recipes.md](jq-recipes.md).

## Why a clean reflog and an empty stash list prove less than they appear

A hook runner such as `prek` or `pre-commit` installs itself as a Git hook shim (`.git/hooks/pre-commit`, `.git/hooks/pre-push`). Git, not Cake, invokes it. Some runners snapshot changes it must not disturb --- most often the unstaged diff, so hooks see a clean tree --- with this sequence:

1. Write the current diff of the affected tracked files to a patch file in the runner's cache.
2. Revert those paths in the working tree.
3. Run the hooks against the now-clean checkout.
4. Re-apply the patch to put the changes back.

If step 3 is interrupted, step 4 never runs. The files are gone, and every Git-level signal says nothing happened:

- `HEAD` does not move, so the reflog has no entry.
- The snapshot is a file in the runner's cache, not a `git stash`, so `git stash list` is empty.
- A reverted working tree is not a commit or a branch, so no other ref changes.
- The patch file is outside the repository, so `git status` cannot see it.

Two nearby records do **not** cover this case, and treating them as evidence is the original misdiagnosis:

- `hook_event` session records describe Cake's own hook protocol (the scripts configured in `.cake/hooks.json` and the global Cake config). A runner that Git invoked never appears there, so missing `hook_event` entries say nothing.
- Cake sees the killed hook only as a Bash result. `Command timed out after N seconds` in a `function_call_output` is often the entire trace Cake has of it, which is why the runner's own cache must be inspected separately.

## Evidence discipline: direct versus inference

Keep the two apart in the report. This failure mode is easy to assert and hard to prove, and it was previously blamed on the wrong actor.

- **Direct evidence.** A patch file in a runner cache whose diff contains the missing content (a distinctive line, a path, or text the session itself printed) and whose timestamp is consistent with the disappearance window. Only this supports naming the hook runner as the cause.
- **Inference.** A hook runner is installed and a command it gates ran in the window, but no patch matches; or a patch exists at the right time with unrelated content; or the cache is unreadable or was pruned. Report this as a candidate, not a finding.

A missing patch weakens the hook hypothesis without disproving it, since runners remove patches on success and may prune their cache. When the evidence is only circumstantial, name the alternatives the evidence does not exclude --- an agent-initiated `git checkout`, `git restore`, or `git stash` in the session; another worktree or another Cake session; a turn-boundary restore performed by a hosting harness; an editor or other process holding the files --- and say what would discriminate between them.

## Step 1: Pin the loss to a moment and a command

Establish when the files were present and when they were gone, then find the command in between. Start with the Bash results that report a timeout, since an interrupted hook is usually killed by Cake's own limit:

```bash
jq -s -r '
  (map(select(.type == "function_call")) | INDEX(.call_id)) as $calls
  | .[]
  | select(.type == "function_call_output")
  | select(.output | test("Command timed out"; "i"))
  | [$calls[.call_id].timestamp, .call_id,
     ($calls[.call_id].arguments | fromjson | .command)] | @tsv
' "$SESSION"
```

Then read the `function_call` and `function_call_output` immediately preceding the disappearance: the command, its arguments, its wall-clock `timestamp`, and its output. `git push`, `git commit`, and `git commit --amend` are the commands that most often invoke a runner.

Convert the window to UTC bounds and list every record in it, so the loss is bracketed by session evidence rather than by memory:

```bash
jq -r --arg from "2026-08-10T03:29:00Z" --arg to "2026-08-10T03:31:00Z" '
  select(.timestamp != null)
  | (.timestamp | sub("\\.[0-9]+Z$"; "Z")) as $ts
  | select($ts >= $from and $ts <= $to)
  | [$ts, .type, (.name // .call_id // "")] | @tsv
' "$SESSION"
```

Both recipes are also in [jq-recipes.md](jq-recipes.md). Note that `function_call` and `function_call_output` carry `timestamp` values, so a patch timestamp can be compared with the calls around it directly.

## Step 2: Identify the runner Git invoked

Read the shims, which name the runner, and the configuration next to them, which names its hooks:

```bash
ls -l .git/hooks
sed -n '1,20p' .git/hooks/pre-commit .git/hooks/pre-push 2>/dev/null
```

Check the repositories' hook configuration for the runner's identity and stages: `prek.toml`, `.pre-commit-config.yaml`, `lefthook.yml`, or a `husky` directory. The `stages` or hook type tells you which Git command triggers it.

Resolve the runner's cache root from the runner itself when it offers a command for it, and from its documented environment variable otherwise:

```bash
RUNNER_CACHE="$(prek cache dir 2>/dev/null || printf '%s' "${PREK_HOME:-${PRE_COMMIT_HOME:-$HOME/.cache/prek}}")"
printf '%s\n' "$RUNNER_CACHE"
```

Do not guess a layout for a runner you have not identified. `prek` writes patches to `<cache>/patches`, but the directory name is a runner-specific detail; find files by their content (Step 3) rather than assuming a name.

## Step 3: Locate candidate patches by time and by content

Recent files first, then anything that looks like a diff regardless of its name:

```bash
# Files the runner wrote recently, newest first
find "$RUNNER_CACHE" -type f -mmin -240 -print0 2>/dev/null | xargs -0 ls -lt | head -20

# Content-based discovery: anything shaped like a Git or unified diff
find "$RUNNER_CACHE" -type f -size -10M -print0 2>/dev/null \
  | xargs -0 grep -l -m1 -E '^(diff --git |Index: |--- a/)' 2>/dev/null | head -20
```

The second command is the robust one: it finds the snapshot even when the runner names it `patch`, `patch<pid>`, or `<epoch-ms>-<pid>.patch`. Content search is also what ties a patch to the lost work.

`prek` names patches `<epoch-milliseconds>-<pid>.patch`, so its filename carries the snapshot time. Decode it to UTC to compare against session records:

```bash
PATCH="$RUNNER_CACHE/patches/1755000000000-12345.patch"
base="$(basename "$PATCH" .patch)"
ms="${base%%-*}"
# macOS
date -u -r $((ms / 1000)) '+%Y-%m-%dT%H:%M:%SZ'
# Linux
date -u -d "@$((ms / 1000))" '+%Y-%m-%dT%H:%M:%SZ'
```

For a runner with no timestamp in the name, use the file's modification time, with the same platform split the runbook uses elsewhere:

```bash
# macOS
TZ=UTC stat -f '%Sm' -t '%Y-%m-%dT%H:%M:%SZ' "$PATCH"
# Linux
TZ=UTC stat -c '%y' "$PATCH"
```

## Step 4: List and search a candidate patch

```bash
# Files the patch touches
grep -E '^diff --git ' "$PATCH" | sed -E 's#^diff --git a/(.*) b/.*#\1#'

# Search the patch for a distinctive string or a known path
grep -n -e '<distinctive line from the lost edit>' -e '<lost/path.rs>' "$PATCH"

# Optional: diffstat without applying anything
git apply --numstat "$PATCH"
```

Search for something the session itself can supply: a line the model wrote, a path the session edited, or a symbol quoted in a tool output. A patch that contains the missing path is a candidate; a patch whose hunk body contains the missing content and whose timestamp falls in the window is direct evidence.

Expect the snapshot to be broader than the loss. A runner captures the whole dirty set, so a patch may list files that are still present. The files that are still absent from `git status --short` are the ones its restore never returned.

## Step 5: Decide and report

- **Matching patch, consistent time.** Direct evidence. Describe the mechanism: the runner snapshotted the dirty tree at that timestamp, the gated command was killed before restoration, and the patch is the surviving copy of the work. Note where the patch is and what it contains, so the user can recover it.
- **No matching patch.** Inference only. State which runner is installed and which command ran, then list the alternatives from the evidence section with the discriminating check for each. Do not name the runner as the cause.
- **Cache unreadable, empty, or pruned.** Say that the hypothesis could not be tested, and do not treat the absence as a negative result.

Never treat "a hook could have done this" as the finding, and never claim that every unexplained clean worktree has this cause.

## Worked example (generalized)

An agent is asked why two files it edited during a task are no longer in the worktree, and finds a clean `git status`, an unchanged `HEAD`, an empty reflog for the window, and nothing in `git stash list`.

Step 1 finds the last Bash call before the disappearance:

```text
2026-08-10T03:29:41Z  call_00_abc123  git push --force origin HEAD
```

and its output:

```text
Command timed out after 60 seconds
```

Step 2 finds a runner installed as the repository's `pre-push` hook, configured in `prek.toml`.

Step 3 finds one patch written in that window:

```text
.../patches/1755000000000-12345.patch
```

Step 4 lists its contents and finds the two missing paths and the exact line the agent had written:

```bash
$ grep -E '^diff --git ' "$PATCH" | sed -E 's#^diff --git a/(.*) b/.*#\1#'
src/parser.rs
src/parser_tests.rs
```

Here the timestamp of the patch agrees with the timed-out `git push` `function_call`, and its hunks contain the missing content, so the report names the hook runner as the cause with direct evidence: the runner snapshotted the dirty tree, the 60-second Bash limit killed it before restoration, and the patch is the only remaining copy of the edits. The report includes the patch path and its file list so the changes can be recovered without a re-edit.

## Platform notes

Patch filenames use epoch milliseconds; convert them before comparing with session timestamps, which are RFC3339 UTC. The commands above split macOS `date -r` and `stat -f` from GNU `date -d` and `stat -c` the same way the [runbook](../index.md) does.

## Safety

Read-only, like the rest of the runbook. Do not apply, restore, delete, move, or copy a runner patch while analyzing; a live runner may still own it, and the patch is the evidence. Do not commit a real patch or a machine-specific cache path into the repository; quote only the smallest useful excerpt in the report.

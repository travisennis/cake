---
status: accepted
date: 2026-09-17
decision-makers: Travis Ennis
informed: issue 588
---

# Admit Sandbox-Granted Directories As Bash Working Directories

## Context and Problem Statement

ADR 031 gave the Bash tool an optional per-call `cwd` and restricted it to the canonical invocation working directory. That restriction overshoots its purpose. Selecting a starting directory grants nothing to the command: the sandbox profiles in `src/clients/tools/sandbox/` govern every file access the command makes from wherever it starts, so a command that can already read `/srv/project` by absolute path gains nothing by starting there.

What the restriction does cost is the layout agents meet most often. A project outside the invocation directory is normally reached by granting it with `--add-dir`, a `[sandbox]` settings entry, or a skill directory. None of those paths may be selected as `cwd` today, so an agent that needs to work in one falls back to putting `cd` in the command text --- exactly the indirection ADR 031 removed, and exactly what makes the effective directory invisible to the command-safety judge and to the transcript.

The real difficulty is not whether to admit granted directories, but how "granted" is decided, and what the answer means in the two places where an admission check and the sandbox disagree: a policy that enforces nothing, and a predicate that only approximates OS enforcement.

`resolve_bash_cwd` and `parse_bash_call` live in `src/clients/tools/bash.rs`. The grant table lives in `SandboxConfig` (`src/clients/tools/sandbox/mod.rs`), which today exposes `is_path_allowed` (every read grant, including the built-in system paths from `get_read_execute_paths()`) and `is_path_writable` (read-write grants only). `SandboxConfig::build` is currently called twice per Bash call: once in `prepare_bash_command` to shape the child, and once in `sandbox_denials` to name a denied path after a failure.

## Decision Drivers

- Admit a directory whenever the sandbox already grants it, so `cwd` covers multi-root and worktree layouts without an invocation rooted at each project.
- Keep the sandbox's grant table the single source of truth, so the admission decision, the child process, the judge request, the repository digest, and the denial diagnostics cannot describe different directory sets.
- Never widen what the command can read, write, or execute, and leave the Seatbelt and Landlock profiles unchanged.
- Never be stricter than the policy in force: `DangerFullAccess` applies no profile, so a grant check there would reject directories the command may freely enter.
- Keep the canonical path used for admission identical to the canonical path used everywhere downstream.
- Fail before judge evaluation and before spawn, with a model-visible message that names what is allowed.

## Considered Options

- **Writable grants only.** Admit a directory inside the invocation workspace, a temp directory, a toolchain cache, or a `[sandbox]` settings directory; reject `--add-dir` and skill directories. No change to `SandboxConfig`. Rejected because the motivating case --- a project granted read-only through `--add-dir` --- is still rejected, so the agent keeps writing `cd`.
- **Any existing directory.** Rejected as a general rule because it removes the grant table from the decision entirely, cannot name what is allowed when it rejects, and reports a bad `cwd` as a mid-command sandbox denial: a command that touches nothing in its working directory succeeds, and `ls` with no arguments yields a denial notice that names no path.
- **Writable grants plus user read-only grants, with `DangerFullAccess` unconstrained (chosen).** Requires `SandboxConfig` to carry the user grant set separately from the built-in system paths.
- **Keep the workspace-only rule (status quo).** Rejected: this issue exists because the constraint buys no security and blocks the granted-directory work the feature was built for.

## Decision Outcome

Chosen option: **writable grants plus user read-only grants, with `DangerFullAccess` unconstrained**, because it makes `cwd` cover every directory the user actually granted while leaving the sandbox's own table as the only source consulted.

`resolve_bash_cwd` still canonicalizes the candidate, still requires an existing directory, and still resolves relative requests from the invocation working directory. Admission becomes a disjunction over the canonical candidate:

- the effective policy is `DangerFullAccess`, or
- the candidate is inside the canonical invocation working directory, or
- `SandboxConfig` reports the candidate as selectable, meaning it is inside a read-write grant or inside a user read-only grant.

The built-in read-and-execute set from `get_read_execute_paths()` --- `/usr`, `/etc`, `/bin`, `/Library`, and their Linux equivalents --- is deliberately **not** an admission source, even though it is a read grant. Those paths exist so binaries and configuration load, not because a user granted them for work; a model that starts a command in `/etc` produces failures unrelated to the request. `DangerFullAccess` already covers the case where a command genuinely needs an arbitrary directory, and any path in the system set remains readable by absolute path under every policy.

`DangerFullAccess` admits any existing directory. The config is built for that policy but never applied --- `prepare_bash_command` skips the sandbox strategy entirely --- so a grant check would be stricter than the policy it claims to implement. The single-source-of-truth requirement is about the child, the judge, and the diagnostics agreeing with each other, and under this policy all three are unrestricted.

Read-only grants stay selectable under the `ReadOnly` policy. Selecting a directory is not a write, and a read-only directory is precisely where read-only work happens. This forces one implementation requirement: the user grant set must be captured before `partition_read_only` runs, because that function demotes the workspace, toolchain caches, and settings directories from `writable` into `read_execute` for the read-only policy. Deriving the grant set from the final `read_execute` list would either hide settings directories or admit the built-in system set along with them.

Admission deliberately runs on the coarse predicates that the denial diagnostics already use, and the decision record must say so rather than let it be discovered. `is_path_writable` and its new sibling are documented in `sandbox/mod.rs` as a best-effort hint for naming a missing grant, not an exact model of Seatbelt or Landlock enforcement: they compare path prefixes over the config's own path lists, so a directory whose parent is granted is treated as granted even where the OS denies the specific access. Gating admission on them is acceptable for three reasons. A wrong admission cannot widen the command's authority, because the OS still governs every access from the selected directory; it only degrades the error message from the preflight rejection to a later sandbox denial. Admission must agree with the diagnostics that name denied paths, and both must read one config. And an exact platform model would be a large amount of platform-specific machinery for a starting-directory check.
The asymmetry is worth recording: a false admit costs a worse message, a false reject blocks a legitimate call.

The selected directory applies to that call only, exactly as ADR 031 decided. `ToolContext` is not mutated, and nothing persists to later calls.

### Consequences

- Good, because an agent can select a project directory granted by `--add-dir`, `[sandbox]`, or a skill directory instead of falling back to a `cd` prefix, which keeps the effective directory visible as structure to the judge and the transcript.
- Good, because admission, the child process, the judge request, the repository digest, and the denial diagnostics read one canonical path from one `SandboxConfig`.
- Good, because no Seatbelt or Landlock profile changes; the operating system remains the enforcement boundary, and the selected directory only changes where a command starts.
- Good, because `DangerFullAccess` is not stricter than the policy it names.
- Bad, because a `cwd` outside the invocation workspace now appears in judge requests and repository digests, so the judge can be asked to reason about a command whose working directory belongs to a different repository than the invocation.
- Bad, because a directory the coarse predicate admits but the OS denies still fails at the first file access, which produces the best-effort denial notice rather than the preflight rejection.
- Bad, because a directory inside the built-in read-and-execute set (`/etc`, `/usr`) remains unselectable even though the command can read it by absolute path; an agent that needs one must start elsewhere or run under `DangerFullAccess`.
- Bad, because an invalid `cwd` now depends on grants as well as the workspace, so the rejection message must name grant classes rather than a single workspace boundary.

## More Information

- Issue #588 owns the implementation; the ExecPlan is `docs/exec-plans/active/bash-cwd-sandbox-grants.md`.
- This ADR partially supersedes [ADR 031](031-bash-command-working-directory.md). ADR 031 keeps `accepted` status: its optional `cwd` argument, relative resolution from the invocation directory, canonicalization, per-call scope, and single effective path for the child, judge, repository digest, and denial diagnostics all stand. Only the workspace-only admission rule is replaced, including the corresponding "Bad" consequence in ADR 031.
- The user grant set reaches `SandboxConfig` from `ToolContext.additional_dirs` (`--add-dir` and `[sandbox]` read-only directories), `ToolContext.settings_dirs` (read-write directories from settings), and `ToolContext.skill_dirs` (skill directories).
- Provider-facing wording lives in `src/clients/tools/bash.rs` and `src/clients/tools/bash-description.txt`; the user-visible contract lives in [Security](../security.md) and [Integrations](../integrations.md).
- Cross-platform behavior must be verified rather than assumed. macOS Seatbelt denies reads under a directory with no `file-read*` rule, while Linux Landlock has no access right that governs `chdir`, so a directory admitted only by the coarse predicate can fail differently on the two platforms. CI's macOS `Test` job is the only place this can be observed, because a Cake session cannot apply a nested Seatbelt profile (ADR 016).

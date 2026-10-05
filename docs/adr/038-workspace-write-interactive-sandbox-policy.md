---
status: accepted
date: 2026-10-04
decision-makers: Travis Ennis
informed: issue 687
---

# A workspace-write-interactive sandbox policy grants macOS app launching

## Context and Problem Statement

Cake's macOS profile is `(deny default)` and grants no Seatbelt primitive for handing work to another application. `lsopen` (LaunchServices) is missing, so `open <url>` and `open -a <app>` fail with `_LSOpenURLsWithCompletionHandler() failed ... error -54` (`permErr`); `appleevent-send` is missing, so `osascript` fails to message another application with the same error. `/Applications` and `/System/Library` are already readable under every policy, so this is a missing capability rather than a missing path grant.

`[sandbox]` cannot express it: `read_only` and `writable` are filesystem path grants, and neither primitive is a path.

The practical cost is that a session whose job is to hand a URL to the user's browser --- the ebook-library project's documented "build the search URL and open it in Trav's browser" workflow, for example --- can only report the blocker. The options today are `danger-full-access`, which drops the whole filesystem boundary, or moving the action into a trusted extension.

## Decision Drivers

- Keep the default boundary unchanged: no new primitive on `workspace-write`.
- Make the capability explicit, discoverable, and selected per run rather than ambient.
- Fail closed: add no fallback and no silent widening.
- State plainly what the capability grants: an application launched this way runs outside cake's sandbox.
- Keep macOS and Linux behavior legible instead of silently divergent.

## Considered Options

- **Option A: grant `lsopen` and `appleevent-send` in `workspace-write`.** Simplest, but it turns every default run into one where a model-generated command can start an application outside the sandbox.
- **Option B: a fourth `--sandbox` value, `workspace-write-interactive`, that is `workspace-write` plus those two primitives.** Opt-in, per run, and parallel to the existing value names.
- **Option C: leave the profile alone and require `danger-full-access` or a trusted extension for app launching.** No new surface, but the honest options are a blunt one (no filesystem sandbox at all) and a heavier one (a per-project toolbox executable or hook).

Chosen option: **Option B**, because it grants exactly the missing capability, leaves every default unchanged, and stays visible in `--help` and in the configuration documentation alongside the other policies.

## Decision Outcome

`SandboxPolicy` gains `WorkspaceWriteInteractive`, selectable as `--sandbox workspace-write-interactive`. The value name keeps the `workspace-write` prefix so it reads as an extension of the default rather than a second, unbounded write policy. On macOS the generated profile adds `(allow lsopen)` and `(allow appleevent-send)` to the `workspace-write` rules. Both are platform capabilities rather than path grants, so they never enter `SandboxConfig`'s path lists; `SandboxPolicy::allows_app_interaction` is the single predicate that decides whether a platform strategy emits them.

Path grants are unchanged: the workspace stays writable, nothing is demoted, and `read-only`, `workspace-write`, and `danger-full-access` generate the same profiles they do today.

On Linux, Landlock has no equivalent permission and does not restrict launching another process, so this policy behaves as `workspace-write` and grants nothing new. That equivalence is documented rather than implemented as a divergent code path.

### Consequences

- Good, because a browser-opening workflow becomes a first-class, per-run choice instead of a documented dead end.
- Good, because the deny-by-default profile and every existing policy keep their guarantees; the change is additive and the default is untouched.
- Bad, because `workspace-write-interactive` is a deliberate escape from the filesystem boundary: `lsopen` lets a model-generated command launch an application that runs with the user's ambient authority --- for example a bundle the command itself wrote into the workspace --- and `appleevent-send` lets it drive applications that are already running. Selecting the policy for a run is the user accepting that authority for that run.
- Bad, because the `interactive` suffix names the session intent rather than the capability, and the same policy is inert on Linux.
- Neutral, because the judge, the emergency bypass, the `[sandbox]` grants, and the trusted-extension trust model are unchanged and continue to apply.

## More Information

- Extends the policy set introduced by [ADR 014](014-sandbox-policy-cli-flag.md).
- [Security and trust boundaries](../security.md) records the policy list, the authority it grants, and the Linux equivalence.
- [Configuration](../configuration.md) records the `--sandbox` values.
- Issue #687 carries the reproduction and the acceptance criteria.

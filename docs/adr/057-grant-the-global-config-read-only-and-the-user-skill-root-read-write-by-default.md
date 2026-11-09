---
status: accepted
date: 2026-11-09
decision-makers: Travis Ennis
informed: task 176
---

# Grant the global config read-only and the user skill root read-write by default

## Context and Problem Statement

Cake's default `workspace-write` grant table covers the working directory, configured writable directories, built-in toolchain caches, discovered skill directories, and Cake's state roots. It deliberately excludes the global config directory (`<config_dir>/cake`) because that tree holds trusted hooks, toolbox executables, and trust manifests, and it does not grant the user skill root (`~/.agents/skills`) as a writable location.

The user-facing `cake-skills` assume both paths. `configuring-cake` reads global `settings.toml`, `system.md`, `hooks.json`, and `tools/`, and writes global settings and skills. `debugging-cake-sandbox` reads global settings and writes state for a nested Cake run. `debugging-cake` and `analyzing-cake-sessions` read session and log inputs. Under an unmodified Cake install, those steps fail unless the user has added extra grants. The skills currently describe the author's configured grants as if they were Cake's defaults.

The decision is to align Cake's defaults with the surfaces its shipped skills operate on, while keeping the credential denylist and the read-only policy intact.

## Decision Drivers

- Shipped skills must work from `~/.agents/skills` with no access to Cake's source tree and no undocumented grants.
- The global config tree is already trusted executable configuration; reading it must not imply writing it.
- The user skill root is a user-owned extension surface that the model must be able to update.
- `read-only` must remain a usable inspection policy with no writable user paths except Cake-managed temporary directories.
- Defaults must not widen into `~/`, `~/.config`, `~/.agents`, or Cake's state roots.

## Considered Options

- Keep the defaults and update every skill to instruct manual grants. This preserves least authority, but makes the primary configuration workflow fail on a clean install and keeps the skills dependent on per-user setup.
- Grant all of `~/.config` read-only. This is broader than Cake's own configuration and exposes unrelated application configuration.
- Grant all of `~/.agents` writable. This covers the skill root without a dedicated bootstrap rule, but authorizes unrelated entries under `~/.agents`.
- Grant only `<config_dir>/cake` read-only and `~/.agents/skills` read-write, bootstrapping the latter for writable policies. This matches the surfaces the skills use.

## Decision Outcome

Chosen option: grant only `<config_dir>/cake` read-only and `~/.agents/skills` read-write, because those are the exact trees the shipped skills read and write.

`<config_dir>/cake` joins the run's read-only grants for every policy, alongside `--add-dir`, `[sandbox].read_only`, skill directories, and Cake's state roots. It is therefore readable through `Read`, `ReadImage`, and Bash, and rejected by `Edit` and `Write`; the sandboxed policies also reject Bash writes, while `danger-full-access` removes the Bash profile and can write it. If `XDG_CONFIG_HOME` selects another config root, that effective root is granted.

`~/.agents/skills` joins the run's read-write grants for `workspace-write`, `workspace-write-interactive`, and `danger-full-access`. It is granted even when `--no-skills` disables discovery; the grant is a default path, not a discovery side effect. For writable policies, a missing root and its missing parents are created before tools start, using the same pre-sandbox initialization as configured writable directories. A creation or inspection failure logs a warning and omits the grant; it does not abort the run. Under `read-only`, an existing root is demoted to read-only and a missing root is neither created nor granted.

Every other path keeps its prior default. Cake's data/cache and sessions directories remain read-only. `~/.config/AGENTS.md`, `~/.cake/AGENTS.md`, and all other home paths remain ungranted. The fixed credential denylist continues to win over both new grants.

### Consequences

- Good, because the shipped skills can read global configuration and install user skills on a clean install.
- Good, because configuration stays read-only, so a prompt-injected command cannot rewrite global hooks, tools, trust records, or settings.
- Good, because the user skill root is auto-created and no longer needs a first-run manual `mkdir`.
- Bad, because the model can now read global `hooks.json`, MCP configuration, trust manifests, and toolbox sources, including any secrets a user embedded in them.
- Bad, because any `workspace-write` run, including one in an untrusted repository, can write persistent instructions into `~/.agents/skills`. This is the same persistence class as the already-writable toolchain caches; the read-only policy and the credential denylist are unchanged.
- Neutral, because `~/.config/AGENTS.md` remains unavailable; the skills document how to create it manually.

## More Information

This decision extends ADR 019's grant classes and ADR 040's writable-directory bootstrap to one built-in default. ADR 056 remains the reason the read-only config grant is visible to both Bash and the in-process file tools.

The current default path list, the in-process/OS projections, and the manual grant instructions belong in [Security](../security.md) and [Configuration](../configuration.md).

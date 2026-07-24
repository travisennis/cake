# Essential documentation

This guide is a reusable method for keeping project documentation small, authoritative, and useful. It assumes the implementation, tests, schemas, generated help, and automation are the most accurate description of mechanics. Its purpose is not to eliminate documentation; it is to reserve prose for knowledge those executable sources cannot communicate well.

## The governing idea

Do not maintain a prose mirror of the code.

A prose mirror describes modules, structs, fields, branches, flags, commands, and control flow already visible in executable sources. It begins accurate, then becomes a second implementation that must be reviewed, synchronized, and audited. The cost grows with every overlapping description, and contradictions eventually make all documentation less trustworthy.

Documentation earns its maintenance cost when it provides at least one of:

- an audience-oriented path for using or changing the project;
- an external guarantee consumers must be able to rely on;
- a security or operational boundary whose limitations matter;
- intent, rationale, or tradeoffs absent from the final code;
- a durable workflow that must be followed exactly.

If code search, generated help, a schema, a test, or automation answers the question as well, prefer that source.

## What deserves durable prose

### User guidance

Document installation, first success, common workflows, configuration, known limitations, and troubleshooting. Organize this by what a user wants to do, not by internal modules.

### External contracts

Document file formats, protocols, API behavior, configuration precedence, compatibility promises, migrations, exit meanings, and observable security guarantees. Describe semantics and versioning. Generate field catalogs and examples from code where practical.

### Security and operations

Record the threat model, trust boundaries, permissions, data handling, failure behavior, recovery, and platform limitations. Code can show checks; it rarely explains which adversary or failure those checks are intended to address.

### Architecture

Record components, dependency direction, ownership boundaries, data flow, and a small set of durable invariants. Avoid symbol catalogs and file-by-file tours. Architecture should change when responsibilities or constraints change, not when code moves.

### Decisions

Record consequential choices when multiple reasonable alternatives existed and the rationale will matter later. Decision records are historical: preserve the context, mark amendments or supersession, and do not turn them into living feature manuals.

### Contributor workflow

Document setup, canonical validation entry points, release or migration steps, and unusual project conventions. Let task runners and CI own complete command definitions; prose should explain when and why to use them.

### Agent instructions

Give agents only high-impact rules that change behavior and cannot be enforced more reliably by tools. Lints, tests, hooks, schemas, permissions, and task runners are better than prompt instructions for deterministic requirements.

## Assign one authority per fact

Before writing, decide which source owns the fact:

  | Kind of fact                  | Preferred authority                               |
  | ----------------------------- | ------------------------------------------------- |
  | CLI flags and defaults        | CLI declaration and generated `--help`            |
  | Serialized fields             | Types, schemas, fixtures, and serialization tests |
  | Build and validation commands | Task runner and CI                                |
  | Module and symbol locations   | Source tree and code search                       |
  | Expected behavior             | Focused tests and snapshots                       |
  | Future work and uncertainty   | Issue or managed-work system                      |
  | User workflow                 | User documentation                                |
  | Compatibility semantics       | Contract documentation                            |
  | Threat model and trust        | Security documentation                            |
  | Boundaries and invariants     | Architecture documentation                        |
  | Historical rationale          | Decision record                                   |

Link to an authority instead of copying it. If two prose documents must mention the same concept, one should state the contract and the other should provide a short contextual link.

## A minimal document set

Most small and medium projects need only:

- a README for identity, installation, first use, and navigation;
- a contributor guide for setup and canonical validation;
- an architecture note for durable boundaries and invariants;
- focused configuration, integration-contract, or security guides only when those surfaces exist;
- an agent-instruction file only when agents work in the repository;
- decision records for consequential historical choices.

Do not add an index merely because there are several files. A short README table is often enough. Do not create `design`, `reference`, and `guardrail` versions of the same subsystem.

## Rewrite an existing corpus

### 1. Inventory

List every document, its audience, its claimed authority, and the behavior it describes. Count lines and references only as signals; duplication and volatility matter more than size.

### 2. Compare with executable sources

Check representative claims against code, tests, generated help, schemas, task runners, and CI. Contradictions reveal topics with too many authorities.

### 3. Classify every section

Use four outcomes:

- **Keep**: durable, audience-specific knowledge with one clear home.
- **Move**: valuable knowledge currently in the wrong authority.
- **Generate or test**: mechanical detail better owned by executable artifacts.
- **Delete**: duplicated, discoverable, obsolete, speculative, or audience-free prose.

Preserve history in version control. Deletion does not erase it.

### 4. Design the destination before rewriting

Choose the smallest document set and assign every retained topic to exactly one file. Define each file's audience and reason to exist in its opening paragraph.

### 5. Rewrite from outcomes inward

Start with user tasks, consumer guarantees, security boundaries, architectural invariants, and contributor decisions. Do not summarize old documents section by section; that preserves their accidental structure.

### 6. Remove the old authorities

Delete superseded documents and repair incoming links. Leaving stale documents with a deprecation banner preserves most of the search and maintenance burden.

### 7. Validate

Check formatting, links, generated artifacts, examples, and terminology. Run examples or parse fixtures when possible. Review the diff for facts that still have multiple owners.

## Rules for ongoing maintenance

Use this question before every documentation change:

> Will a user, integrator, operator, contributor, or future maintainer be unable to learn this safely from executable sources or decision history?

If not, do not add durable prose.

Additional rules:

- A code change does not automatically require a documentation change.
- A module move does not require an architecture update unless ownership or a boundary changed.
- A new feature does not automatically deserve a new document.
- Plans, TODOs, and open questions belong in the work-tracking system.
- Avoid future-enhancement sections; they become unactionable shadow backlogs.
- Avoid copying exact error text unless consumers treat it as a contract.
- Keep examples executable, parser-tested, or deliberately minimal.
- Prefer deletion and links over synchronization instructions.
- Treat documentation line count as a drift alarm, not a quality metric.

## Review questions

For each changed document, ask:

1. Who is the reader, and what decision or task does this enable?
2. Is this knowledge unavailable or unsafe to infer from executable sources?
3. Is this the single authoritative home?
4. Does the prose describe stable semantics or volatile mechanics?
5. Could a schema, test, generated command, or code comment own it better?
6. Does it contain plans or speculation that belong in work tracking?
7. What event should require this document to change?

If those questions have weak answers, delete or relocate the prose.

## Measuring success

A healthy documentation system is not the one with the most complete prose. It is the one where:

- readers quickly reach an authoritative answer;
- behavior changes usually require one documentation edit or none;
- contradictions are rare and easy to diagnose;
- external contracts and security limits remain explicit;
- architecture survives refactoring without constant updates;
- deleted implementation narration is recoverable from Git but absent from the current maintenance surface.

Set an approximate living-prose budget if it helps expose growth, but never optimize for a line count at the expense of user safety or contract clarity.

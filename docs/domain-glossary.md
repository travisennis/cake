# Domain glossary

This document records concepts whose names collide with other concepts in Cake. It exists to prevent one specific mistake: treating two code paths as duplicate implementations of one idea when they answer different questions.

It is a glossary and nothing else. It is not a specification, not a design record, and not a place for open questions --- those belong in GitHub issues. Decisions belong in [ADRs](adr/README.md); this document records only what a term means and what it is confused with.

## When to read it

Read an entry before reporting that two functions, types, or code paths duplicate each other. If a glossary entry says they answer different questions, the divergence is intended and there is no finding. A survey that is about to file duplicated logic should consult this document first.

## When to add an entry

Add one only when a specific decision needed it, and cite that issue or pull request in the entry. An entry written speculatively has not prevented anything and costs context in every session that loads this file. Two terms that merely sound similar do not qualify; the bar is that someone actually conflated them and was wrong.

Each entry names the code symbols it describes under **Anchors**. `just lint-domain-glossary` verifies those symbols still exist, so an entry cannot silently outlive the code it explains.

## Entries

### Tool call target

The file a mutating tool call acts on. **Write and Edit do not share this concept.**

- **Write's target is a destination.** It need not exist, and intermediate directories may be created for it. Resolved by walking to the deepest existing ancestor and normalising the remainder lexically, so a `..` crossing a directory that does not exist cancels against the pending component rather than creating it.
- **Edit's target is a subject.** It must already exist. Resolved by canonicalisation, which fails outright when any component on the path is missing.

Two resolvers is correct. Divergence between them is not a finding, and the scheduler's grouping key agrees with Edit's executor in every case where Edit succeeds, because both call the same function once the path exists.

**Anchors:** `resolve_write_path`, `resolve_path_for_write_scheduling`, `validate_path_for_write`

*Added for #186, which assumed the two were one concept and was closed as not planned.*

### Tool result consumer

Who reads what a tool produces. Cake has two consumers with opposite needs, and which one is meant decides whether a fact needs to be structured.

- **The model reads prose.** Unstructured tool text is the working interface, the same way an agent reads compiler, linter, and test-runner output. A coarse status value carries less than the sentence it would summarise.
- **Telemetry cannot read prose.** Fields such as the error flag on a tool call record exist for machine consumers and must stay structured.

"This fact is untyped" is therefore not a defect on its own. Ask which consumer needs the distinction before proposing a type for it.

**Anchors:** `ToolCallTelemetry`, `ToolResult`

*Added for #187, whose first draft proposed a structured retryable-versus-terminal outcome that the model did not need.*

### Trusted extension

A user-provided executable Cake runs outside the OS sandbox: hook scripts and toolbox tools. Trust comes from the user vouching for the file, not from any check Cake performs.

Distinct from two things it is often merged with:

- **Sandboxed execution**, which applies to model-generated shell commands only.
- **In-process mutation**, meaning Edit and Write, which are constrained by path validation rather than by the OS sandbox.

The three have different enforcement stories, so a claim about "how Cake stays safe" is wrong unless it names which one it means.

**Anchors:** `ToolboxTool`, `SandboxPolicy`, `validate_path`

*Added for #185, where the enforcement path for unsandboxed tools was initially understated.*

### Sandbox

The OS-level filesystem restriction Cake applies to Bash. It wraps model-generated shell commands and nothing else.

It is not the mechanism that makes Edit and Write safe --- those never pass through it, and are bounded by path validation instead. Under the read-only policy the mutating built-in tools and every toolbox tool are removed from the registry rather than sandboxed, because omitting them is what makes the policy's guarantee hold for the whole agent.

**Anchors:** `SandboxPolicy`, `retain_read_safe_tools`

*Added for #185. See [Security and trust boundaries](security.md) for the authority on the trust model.*

## Retirement

This document earns its place only if entries are cited to reject findings that would otherwise be filed. Retire it if a review finds that no entry has been cited that way since the previous review, and record the evidence in the removing pull request.

Review at the third such citation or after six months, whichever comes first. `just lint-domain-glossary` prints the entry count so the growth rate is visible without reading the file.

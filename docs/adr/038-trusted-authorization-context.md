---
status: proposed
date: 2026-10-03
decision-makers: Travis Ennis
informed: issue 315, issue 322
---

# Trusted Authorization Context for the Bash Judge

## Context and Problem Statement

The LLM judge is the only command-safety gate above the OS sandbox (ADR-018), and it is deliberately authorization-blind: each evaluation receives the command, the working directory, a compact repository digest, the model's untrusted `reason`, and any referenced-script observation, and nothing about the conversation. Because it cannot tell a user-requested effect from assistant drift, it fails closed on every remote or destructive action whose safety turns on authorization. The `reason` field cannot close the gap: the assistant authors it, the rubric treats a claim of user approval as an incongruence signal, and a reason "never authorizes a remote destructive command on its own."

The cost is observable. Two identical `gh issue close` invocations in one session were split between `allow` and `block` because closure sits in the judgment gray zone. A user-authorized branch cleanup the user had explicitly requested was repeatedly blocked because the judge could not see the request (issue #315). The rubric itself concedes the hole for its own classes: "Whether a user authorized a specific transfer is trusted context the judge does not receive, so an authorized upload of a reviewed file can be a false block."

This record covers a near-term policy step and the durable mechanism. The near-term step, tracked by issue #681, removes reversible remote state changes from the gray zone: the project rubric permits them as routine mutations, like additive comments, because they are reversible and carry no payload, and it defines the class by properties (reversible, state and metadata only, payload-free) rather than by tool, so an issue close/reopen, a label or title edit, and the same shape in another CLI are all covered. That step changes no trust boundary. The durable mechanism is a bounded, host-authenticated projection of the current invocation's user authorization, so the judge can recognize a requested effect without gaining the facts it still must not assume.

## Decision Drivers

- Keep the judge fail-closed and keep the OS sandbox as the filesystem boundary.
- Keep `reason`, command text, tool output, and repository content as untrusted evidence that never manufactures authorization.
- Let the judge recognize a user-authorized remote effect without receiving unverifiable facts.
- Bound the projection's provenance, recency, and size so stale or pasted text cannot widen scope.
- Keep normal telemetry metadata-only.

## Considered Options

- **Broaden the rubric's routine-mutation allowance only (near-term, chosen).** Move reversible remote state changes (an issue close/reopen, a label, assignee, milestone, or title edit, and the same shape in another tool) into the allowed set beside additive comments. Cheap, removes the observed false blocks, and changes no trust boundary. It does not help destructive or egress effects whose safety depends on authorization.
- **Forward the conversation transcript to the judge.** Rejected. It is unbounded, adds token, latency, and privacy cost to the hottest tool, and lets repository-controlled text inside pasted or quoted content become authorization.
- **A bounded, host-authenticated user-intent projection (durable, chosen).** Cake projects the current invocation's user turn, with provenance labels, into the judge request. The judge gains a trusted channel for "was this requested" while remaining blind to unverifiable facts and fail-closed when the projection is absent.

## Decision Outcome

Chosen option: do both. Land the rubric widening under issue #681, and adopt the bounded user-intent projection as the durable mechanism, implemented through issue #315.

The projection has these properties:

- **Provenance split.** Cake tags the projection from the session record by role; the assistant cannot author it. The user's own instruction text is the trusted channel. Any content incorporated inside the user turn (pasted text, quoted file or tool output, attachments) stays untrusted evidence alongside `reason`, because pasted repository text must not become authorization.
- **It answers "requested," not "safe."** The projection can establish that the user asked for an action on a target, and can bound the grant's scope. It never establishes a fact: mergeability, file secrecy, and payload destination still require in-command guards or observation.
- **Scope.** Intent may authorize a reversible remote mutation on a named target. It never authorizes irreversible local destruction, credential disclosure, or a `data-egress` payload, which stay gated on facts or a target-naming instruction.
- **Recency and size.** The projection carries the current invocation's user turn and at most the immediately preceding one, byte-capped. A stale instruction from earlier turns does not authorize a later action.
- **Fail closed.** A missing, truncated, ambiguous, or malformed projection leaves the judge in today's authorization-blind behavior.
- **Telemetry.** The projection is sent to the judge provider as the command already is, but normal telemetry stays metadata-only and does not persist the prompt text.

On acceptance this record partially supersedes ADR-018: the judge's input contract gains a trusted authorization channel, while ADR-018's gate, fail-closed rule, allowlist, and bypass keep their meaning.

### Consequences

- Good, because the judge can stop blocking a user-requested remote effect it currently cannot see, without trusting the model's `reason`.
- Good, because the near-term rubric step removes the observed reversible-mutation false blocks immediately and independently of the projection.
- Good, because provenance labeling keeps pasted repository text from becoming authorization, containing the injection risk ADR-018 accepts.
- Bad, because forwarding any user text to the judge is new data exposure to the judge provider and a new prompt-injection surface inside the trusted channel.
- Bad, because "tacit" authorization is easy to over-read: a vague request must not be stretched to cover an unrelated effect, which needs a precise scope rule and corpus cases.
- Bad, because a bounded, role-tagged projection is more moving parts on the hottest tool than the current single user message.

## More Information

- Implements the direction of issue #315, `Design trusted authorization context for the Bash judge`, a child of issue #322, `Mature Cake's model-based Bash safety judge`.
- References and, on acceptance, partially supersedes [ADR-018](018-llm-judge-command-gate.md), `LLM Judge Command Gate`, for the judge's input contract.
- Builds on [ADR-020](020-bounded-llm-judge-recovery.md) and [ADR-035](035-judge-script-observations.md), which already treat non-command inputs as untrusted evidence.
- Requires a `docs/security.md` update at implementation: the statelessness paragraph's statement that the judge has "no conversation history" changes to describe the bounded projection and its provenance.
- Prior-evidence handling (whether bounded, completeness-marked prior tool results may reduce uncertainty) remains a separate decision owned by issue #315.

# Profiling Cake

This runbook defines a repeatable local workload for measuring Cake before a performance change. It keeps provider latency and credentials out of the measurement, and it does not change production behavior.

Samply is the supported primary workflow. The macOS Instruments path is an optional secondary workflow for exploratory allocation inspection; it is not required to record or compare the standard profile.

## Workload

The repository provides one amplified end-to-end agent-loop workload:

- `scripts/profile-agent-loop.py` starts a fake Responses API on `127.0.0.1`.
- The fake provider returns a tool-heavy batch of 5,000 `Read` calls, then a final assistant message. The large batch supplies enough repeated Cake-owned work for a sampled profile; it is measurement amplification, not a typical model response.
- Cake reads the same 32-line fixture for every call in a temporary workspace and sends all tool outputs back to the fake provider. The provider rejects the run unless every output has the expected call ID and complete fixture markers.
- Cake runs with `--no-session`, so this workload measures the agent loop and tool path without mixing in session persistence costs.

The workload makes exactly two local HTTP requests. It uses no provider credentials and makes no network request outside the local mock server.

The default batch size is part of the baseline. Use `--tool-calls N` to explore a different size, but do not compare artifacts recorded with different values. A valid default run reports `"turns":2` in Cake's completion JSON and ends with `Profile written to`.

## Prerequisites

Install the Rust toolchain, `just`, Python 3, and [samply]. The repository's profiling profile keeps release optimizations and debug symbols:

```sh
cargo install samply --locked
```

On macOS, use `samply setup` if Samply reports a permission or signing error. The [samply README] documents platform requirements and this profiling setup.

[samply]: https://github.com/mstange/samply
[samply README]: https://github.com/mstange/samply#turn-on-debug-info-for-full-stacks

## Record a CPU profile

Run the deterministic workload through Samply:

```sh
just profile --output profiling/artifacts/agent-loop-before.jslb.gz
```

The recipe first builds `target/profiling/cake` with the `profiling` Cargo profile. It then launches that binary against the local mock provider. Samply writes a compressed profile to `profiling/artifacts/`. The directory is ignored by Git because profiles are generated artifacts.

The default 5,000-call batch is intentionally amplified so Samply can collect a useful population of Cake samples from one short-lived CLI process. Record the batch size with the artifact. If the workload is too heavy for a constrained machine, lower it explicitly and use that same value for every compared run:

```sh
just profile \
  --tool-calls 1000 \
  --output profiling/artifacts/agent-loop-1000.jslb.gz
```

To inspect a saved profile, use Samply's loader:

```sh
samply load profiling/artifacts/agent-loop-before.jslb.gz
```

The profile contains Cake's process, not the Python mock server or the Samply launcher. The same command works on macOS and Linux, subject to each platform's profiler permissions.

## Compare before and after

Use separate artifact names. Build and record the baseline before changing source code, then repeat after the change:

```sh
just profile --output profiling/artifacts/agent-loop-before.jslb.gz
# Make the measured source change.
just profile --output profiling/artifacts/agent-loop-after.jslb.gz
samply load profiling/artifacts/agent-loop-before.jslb.gz
samply load profiling/artifacts/agent-loop-after.jslb.gz
```

Keep these variables constant:

- commit or source diff, except for the change under test;
- operating system, CPU architecture, Rust toolchain, and Cargo profile;
- workload command, `--tool-calls` value, and artifact format; and
- provider response sequence and fixture size.

A sampled profile is not a precise wall-clock benchmark. Repeat each profile three to five times when a candidate is small, compare the same call tree and self-time views, and report whether the difference is larger than normal run noise. Do not treat one profile as proof of a small improvement.

## Optional allocation inspection with Instruments

Samply records sampled CPU stacks; it does not count every allocation. On macOS, contributors may try the same workload with Xcode's Instruments Allocations template:

```sh
just profile \
  --profiler instruments \
  --output profiling/artifacts/agent-loop-before.trace
```

This path is optional and exploratory. It is macOS-only, depends on Xcode's developer-tool authorization and process attachment behavior, and is not a prerequisite for the supported Samply workflow. Follow-up issue [#383] evaluates a portable in-process allocation profiler and whether Instruments should remain available.

Before attempting it, verify that full Xcode is selected and that the Allocations template exists:

```sh
xcode-select -p
xcrun --find xctrace
xcrun xctrace list templates | grep -i allocations
```

Run from an ordinary terminal opened independently of Cake or another sandboxed agent. If attachment authorization fails, enable developer-tool access, quit the terminal application, reopen it normally, and retry:

```sh
/usr/sbin/DevToolsSecurity -status
sudo /usr/sbin/DevToolsSecurity -enable
```

The output path must not already exist because `xctrace` refuses to overwrite a trace bundle. A valid run must exit with status 0, print `Profile written to`, and create the requested `.trace` directory. A failed recording may still leave a trace bundle, so the directory alone is not proof of success.

In the environment used to verify this runbook, `xctrace` still reported `Failed to attach to target process` after developer mode was enabled. The workload normally finishes in about 200 ms, so its short lifetime may be part of the attachment failure, but that cause has not been proven. Do not add a delay and treat the result as the standard baseline: an artificial delay changes the profiled workload. Record the failure and use the Samply workflow instead.

If a recording succeeds, open the trace in Instruments, select the Allocations instrument, and compare the `cake` process' allocation count, allocated bytes, and relevant stacks between otherwise identical before and after runs.

## Tool selection

  | Question                                             | Tool and workflow                                                                                                                 |
  | ---------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
  | Which Cake functions consume sampled CPU time?       | `just profile` with Samply; inspect the call tree and flame graph.                                                                |
  | Does a suspected clone or string path allocate more? | Optionally try Instruments on macOS; treat it as exploratory until the follow-up allocation-profiler evaluation is complete.      |
  | Does session persistence add a measurable cost?      | Use a separate fixture and workload that omits `--no-session`; do not mix its result with the agent-loop profile.                 |
  | Does file or syscall activity dominate?              | Use the platform's tracer around the same compiled Cake command, then record the exact command and permissions with the artifact. |

This task standardizes the first row and provides an optional implementation of the second. The session, allocation-profiler evaluation, and syscall measurements answer different questions and remain separate work.

## Relationship to performance work

Issue [#50] establishes the workload, Cargo profile, recipe, artifact location, and comparison method. It intentionally includes no speculative optimization. Issue [#47], **Profile and Optimize Cake Performance**, should use the supported Samply workflow to validate its active performance plan before changing agent-loop, session, or request-construction code. Keep generated profiles out of Git and link their paths and observations from the issue or pull request.

[#50]: https://github.com/travisennis/cake/issues/50
[#47]: https://github.com/travisennis/cake/issues/47
[#383]: https://github.com/travisennis/cake/issues/383

## Limitations and recovery

- The local provider removes network latency. These profiles describe Cake's local overhead, not end-to-end provider latency.
- The 5,000-call batch amplifies per-tool scheduling, execution, and output ownership far beyond an ordinary model response. Use it to compare those paths, not to predict production turn latency or concurrency.
- The workload uses `Read`, not `Bash`, so it does not exercise the LLM command judge or OS sandbox. Add a separate workload before drawing conclusions about those paths.
- If the recipe reports that Samply is missing, install it with the command in [Prerequisites]. If macOS profiling reports a signing or permission error, run `samply setup` and repeat the same command.
- On macOS, Samply may report `Unknown(1100)` after its code-signing setup when a restricted parent cannot access the Mach bootstrap service. The `1100` value is `BOOTSTRAP_NOT_PRIVILEGED`; retry from an unrestricted local terminal. Setting `CAKE_SANDBOX=off` for the child does not remove a parent restriction.
- If the mock provider does not receive two requests, treat the run as invalid rather than comparing its artifact. The helper exits non-zero in that case.

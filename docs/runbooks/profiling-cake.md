# Profiling Cake

This runbook defines a repeatable local workload for measuring Cake before a performance change. It keeps provider latency and credentials out of the measurement, and it does not change production behavior.

## Workload

The repository provides one end-to-end agent-loop workload:

- `scripts/profile-agent-loop.py` starts a fake Responses API on `127.0.0.1`.
- The fake provider returns a `Read` tool call, then a final assistant message.
- Cake reads a 32-line fixture in a temporary workspace and sends the tool output back to the fake provider.
- Cake runs with `--no-session`, so this workload measures the agent loop and tool path without mixing in session persistence costs.

The workload makes exactly two local HTTP requests. It uses no provider credentials and makes no network request outside the local mock server.

## Prerequisites

Install the Rust toolchain, `just`, Python 3, and [samply]. The repository's profiling profile keeps release optimizations and debug symbols:

```sh
cargo install samply --locked
```

On macOS, use `samply setup` if samply reports a permission or signing error. The [samply README] documents platform requirements and this profiling setup. The allocation workflow below also needs Xcode with the `xctrace` command.

[samply]: https://github.com/mstange/samply
[samply README]: https://github.com/mstange/samply#turn-on-debug-info-for-full-stacks

## Fresh macOS verification

Use this section after installing Xcode or when the terminal's developer-tool selection may be stale. Run these commands from an ordinary Ghostty or Terminal session, not from Cake's Bash tool. A child `CAKE_SANDBOX=off` setting cannot remove restrictions imposed by its parent process.

Set the Xcode paths. These commands assume the standard installation path:

```sh
cd /Users/travisennis/Projects/cake
export XCODE_APP=/Applications/Xcode.app
export XCODE_DEV="$XCODE_APP/Contents/Developer"
test -d "$XCODE_DEV"
```

If that path does not exist, find the installed application and set `XCODE_APP` to the returned path:

```sh
mdfind 'kMDItemCFBundleIdentifier == "com.apple.dt.Xcode"'
```

Select the full Xcode developer directory and complete its first-launch setup:

```sh
sudo xcode-select --switch "$XCODE_DEV"
sudo xcodebuild -runFirstLaunch
export DEVELOPER_DIR="$XCODE_DEV"
```

Verify the tools that the allocation workflow needs:

```sh
xcode-select -p
xcodebuild -version
xcrun --find xctrace
xcrun xctrace version
xcrun xctrace list templates | grep -i allocations
```

`xcrun --find xctrace` must resolve inside the full Xcode installation, and the template list must contain `Allocations`. If `xcodebuild` asks for a license, run `sudo xcodebuild -license` and follow the prompt before continuing. Do not use `sudo` for the profiling command.

Build and check the Cake profiling binary:

```sh
cargo build --profile profiling
test -x target/profiling/cake
```

Do not use `cake --version` as the profiling smoke test. It exits too quickly, so xctrace may fail to attach even though it creates a `.trace` directory. Use the complete local workload below instead. If xctrace asks for administrator credentials, enter them and let the recording finish. Do not press `Ctrl-C`.

Run the complete allocation workload:

```sh
DEVELOPER_DIR="$XCODE_DEV" \
  just profile \
  --profiler instruments \
  --output profiling/artifacts/agent-loop-allocations.trace
```

A valid run must satisfy all three conditions:

1. the command exits with status 0;
2. the output includes `Profile written to`; and
3. `profiling/artifacts/agent-loop-allocations.trace` is a directory.

Check and open the trace:

```sh
test -d profiling/artifacts/agent-loop-allocations.trace
open profiling/artifacts/agent-loop-allocations.trace
```

In Instruments, select the Allocations instrument and inspect the `cake` process. Record the allocation count, allocated bytes, and relevant stacks. The directory check alone does not prove that the recording succeeded; trust the command's exit status and xctrace's output first.

If the full workload reports `Failed to attach to target process`, check that you ran it from the ordinary terminal, not through Cake, and rerun it without interrupting it. A short-lived command such as `cake --version` is not a valid attachment test. If samply reports `Unknown(1100)`, use the recovery guidance below; that is a Mach bootstrap permission failure in the parent environment, not an xctrace failure.

## Record a CPU profile

Run the deterministic workload through samply:

```sh
just profile --output profiling/artifacts/agent-loop-before.jslb.gz
```

The recipe first builds `target/profiling/cake` with the `profiling` Cargo profile. It then launches that binary against the local mock provider. Samply writes a compressed profile to `profiling/artifacts/`. The directory is ignored by Git because profiles are generated artifacts.

To inspect a saved profile, use samply's loader:

```sh
samply load profiling/artifacts/agent-loop-before.jslb.gz
```

The profile contains Cake's process, not the Python mock server or the samply launcher. The same command works on macOS and Linux, subject to each platform's profiler permissions.

## Measure allocations on macOS

Samply records sampled CPU stacks. It does not count every allocation. Use the same deterministic workload with Instruments' Allocations template when validating candidates such as telemetry-context cloning or per-tool-call string ownership:

```sh
just profile \
  --profiler instruments \
  --output profiling/artifacts/agent-loop-before.trace
```

The recipe keeps the mock provider and fixture identical. The `--profiler instruments` option runs `xcrun xctrace` and writes an Instruments trace. Open the trace in Instruments, select Allocations, and compare the `cake` process' allocation count, bytes, and stack locations between before and after runs. This path requires Xcode with `xctrace` and is macOS-only.

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
- workload command and artifact format; and
- provider response sequence and fixture size.

A sampled profile is not a precise wall-clock benchmark. Repeat each profile three to five times when a candidate is small, compare the same call tree and self-time views, and report whether the difference is larger than normal run noise. For an allocation candidate, compare the Instruments allocation counters and the stacks that own those allocations. Do not treat one profile as proof of a small improvement.

## Tool selection

  | Question                                             | Tool and workflow                                                                                                                                 |
  | ---------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
  | Which Cake functions consume sampled CPU time?       | `just profile` with samply; inspect the call tree and flame graph.                                                                                |
  | Does a suspected clone or string path allocate more? | `just profile --profiler instruments`; compare Allocations counters and stacks on macOS.                                                          |
  | Does session persistence add a measurable cost?      | Use a separate fixture and workload that omits `--no-session`; do not mix its result with the agent-loop profile.                                 |
  | Does file or syscall activity dominate?              | Use the platform's file or syscall tracer around the same compiled Cake command, then record the exact command and permissions with the artifact. |

This task standardizes the first two rows. The session and syscall rows are separate measurements because they answer different questions.

## Relationship to issue #47

Issue [#50] establishes the workload, Cargo profile, recipe, artifact location, and comparison method. It intentionally includes no speculative optimization. Issue [#47], **Profile and Optimize Cake Performance**, should use these profiles to validate its active performance plan before changing agent-loop, session, or request-construction code. Keep the generated profiles out of Git and link their paths and observations from the issue or pull request.

[#50]: https://github.com/travisennis/cake/issues/50
[#47]: https://github.com/travisennis/cake/issues/47

## Limitations and recovery

- The local provider removes network latency. These profiles describe Cake's local overhead, not end-to-end provider latency.
- The workload uses `Read`, not `Bash`, so it does not exercise the LLM command judge or OS sandbox. Add a separate workload before drawing conclusions about those paths.
- If the recipe reports that samply is missing, install it with the command in [Prerequisites]. If macOS profiling reports a signing or permission error, run `samply setup` and repeat the same command. If `xctrace` is unavailable, install Xcode and select it with `xcode-select --switch`.
- On macOS, samply may report `Unknown(1100)` after its code-signing setup when a restricted parent cannot access the Mach bootstrap service. The `1100` value is `BOOTSTRAP_NOT_PRIVILEGED`; retry from an unrestricted local terminal. Setting `CAKE_SANDBOX=off` for the child does not remove a parent restriction.
- If the mock provider does not receive two requests, treat the run as invalid rather than comparing its artifact. The helper exits non-zero in that case.

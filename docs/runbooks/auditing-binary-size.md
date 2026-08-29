# Auditing Binary Size

Analyze the cake release binary to understand size contributors and identify optimization opportunities.

## Prerequisites

Install the repository development tools with `just setup`, then install the audit-specific `cargo-bloat` utility for the crate and function reports:

```bash
cargo install cargo-bloat
```

The committed baseline does not require either size-analysis tool. `cargo bsize` is an optional, installed subcommand that produces a broader report; record its version and warnings when using it.

## Workflow

### 1. Build the release binary and check current size

```bash
just build
ls -lh target/release/cake
```

### 2. Analyze size by crate

Shows which dependencies contribute the most to the `.text` section:

```bash
cargo bloat --release --crates
```

For the broader optional report, run the repository's installed `cargo-bsize` subcommand without a Cargo release flag:

```bash
cargo bsize
```

`cargo bsize --release` is not valid for this subcommand; it selects its release analysis build itself.

#### Native object attribution warnings

The aggregate section sizes remain useful when `cargo bsize` warns that it cannot open native object files, but its per-crate attribution is incomplete for those objects. On the observed arm64 macOS run, `aws_lc_sys` stored 368 C and assembly object files inside its Rust archive. `cargo bsize` 0.0.2 emitted 168 warnings for members that existed both in that archive and in the crate's build output, but it looked for them through a deleted `target/bsize/release/deps/rustc*/` extraction directory. These warnings do not mean that AWS-LC code is missing from the final binary. Verify the shipped file with `just build` and treat the affected crate breakdown as partial.

### 3. Analyze size by function

Shows the largest individual functions:

```bash
cargo bloat --release -n 30
```

`cargo bloat` may report the size of an analysis artifact that differs from the normal stripped release binary. Run `just build` again before reporting `target/release/cake` as the shipped binary size.

### 4. Review findings

The typical size breakdown for this project:

  | Category              | Typical Contributors                                                         |
  | --------------------- | ---------------------------------------------------------------------------- |
  | **TLS/Networking**    | `aws_lc_sys`, `rustls`, `h2`, `hyper`, `reqwest` — usually the largest chunk |
  | **Standard library**  | `std` — unavoidable baseline                                                 |
  | **Schema validation** | `jsonschema`, `referencing`, `fancy_regex`, `regex_*`                        |
  | **Async runtime**     | `tokio` — moderate                                                           |
  | **CLI framework**     | `clap_builder` — moderate                                                    |
  | **Application code**  | `cake`                                                                       |
  | **Other deps**        | `similar`, `serde_json`, `tracing_subscriber`, etc.                          |

### 5. Check for easy wins

- **Symbols stripped?** The release profile should have `strip = true`. If not, that's \~1.3 MB of free savings with no performance cost (only downside: raw addresses in panic backtraces instead of function names).
- **Panic strategy set?** The release profile currently uses `panic = "abort"` for smaller binaries.
- **Tokio features minimal?** The project currently uses `features = ["full"]`. If auditing for size regressions, check whether narrower features can support the current async, process, IO, signal, and macro usage before changing this.
- **Unused dependencies?** Run `cargo machete` to detect unused deps (install with `cargo install cargo-machete`).

### 6. Commit release baseline

`ci/binary-size-baseline.json` records the exact byte size of the normal release artifact, along with its target and toolchain. Regenerate it with:

```bash
just binary-size-baseline
```

The baseline is target-specific. The first entry records the native `aarch64-apple-darwin` build used for local analysis; do not compare it with the x86_64 or Linux release artifacts. The generator updates only the selected target record, so additional target baselines can be added from their matching release environments without replacing this measurement.

The generator's fixture tests run without a release build or baseline change:

```bash
just binary-size-baseline-check
```

The target's Rust standard library and linker must be installed in the matching build environment. For example, a macOS host needs `rustup target add x86_64-unknown-linux-gnu` and an `x86_64-linux-gnu-gcc` cross-linker before the Linux example can run. For a cross-built target, pass its artifact explicitly:

```bash
cargo build --release --target x86_64-unknown-linux-gnu
python3 scripts/binary-size-baseline.py \
  --target x86_64-unknown-linux-gnu \
  --artifact target/x86_64-unknown-linux-gnu/release/cake
```

The baseline records the shipped file size, not the larger `target/bsize` analysis artifact and not the estimated shipped size in a bsize report. Review an intentional change with:

```bash
just binary-size-baseline
git diff -- ci/binary-size-baseline.json
```

### 7. Compare over time

To track whether binary size is growing, compare before and after changes. The committed baseline is the review point; it is not a hard gate because native C and assembly builds can vary across targets and environments.

**What counts as a regression?** The committed baseline provides a stable review point for the target it names. Judgment is still required when the build target or environment differs. Sensible heuristics (not measured from this project --- treat as starting points to discuss with the maintainer, not hard gates):

- A noticeable relative jump (rule of thumb: more than a few percent) in one change usually signals a new dependency or a wider feature set.
- A noticeable absolute jump (rule of thumb: hundreds of KB or more) is worth investigating even if relative growth is small.
- A single crate moving meaningfully up the `cargo bloat --crates` list warrants checking whether new features were enabled or a heavier dependency was pulled in.

## Current Release Profile

The release profile lives in [`Cargo.toml`](../../Cargo.toml). Inspect it directly rather than relying on a copy here:

```bash
awk '/^\[profile.release\]/,/^$/' Cargo.toml
```

At time of writing it includes `lto`, `codegen-units`, `panic`, and `strip` settings. If any of those are missing or changed unexpectedly, that is the first place to look for size regressions or easy wins.

## Key Tradeoffs to Be Aware Of

- **`rustls` vs `native-tls`**: `rustls` bundles \~1 MB of crypto (`aws_lc_sys` + `rustls`). Switching to `native-tls` uses the OS TLS stack (smaller binary) but introduces platform-dependent build behavior, especially on Linux with OpenSSL.
- **`opt-level = "z"`**: Optimizes for size over speed. Not recommended unless binary size is critical.
- **`strip = true`**: Only downside is panic backtraces show raw addresses instead of function names. Logs via `tracing` are unaffected.
- **`panic = "abort"`**: Reduces unwind machinery. The tradeoff is no stack unwinding or panic recovery.

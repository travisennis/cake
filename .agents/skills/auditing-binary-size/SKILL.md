---
name: auditing-binary-size
description: "Analyze and audit the release binary size to identify what's contributing to bloat. Use when asked to check binary size, audit binary bloat, investigate why the binary is large, or monitor binary size over time."
---

# Auditing Binary Size

Analyze the cake release binary to understand size contributors and identify optimization opportunities.

## Prerequisites

Ensure `cargo-bloat` is installed:

```bash
cargo install cargo-bloat
```

## Workflow

### 1. Build the release binary and check current size

```bash
cargo build --release
ls -lh target/release/cake
```

### 2. Analyze size by crate

Shows which dependencies contribute the most to the `.text` section:

```bash
cargo bloat --release --crates
```

### 3. Analyze size by function

Shows the largest individual functions:

```bash
cargo bloat --release -n 30
```

### 4. Review findings

The typical size breakdown for this project:

  | Category             | Typical Contributors                                                         |
  | -------------------- | ---------------------------------------------------------------------------- |
  | **TLS/Networking**   | `aws_lc_sys`, `rustls`, `h2`, `hyper`, `reqwest` — usually the largest chunk |
  | **Standard library** | `std` — unavoidable baseline                                                 |
  | **Async runtime**    | `tokio` — moderate                                                           |
  | **CLI framework**    | `clap_builder` — moderate                                                    |
  | **Application code** | `cake` — relatively small                                                    |
  | **Other deps**       | `similar`, `regex_*`, `serde_json`, `tracing_subscriber`, etc.               |

### 5. Check for easy wins

- **Symbols stripped?** The release profile should have `strip = true`. If not, that's ~1.3 MB of free savings with no performance cost (only downside: raw addresses in panic backtraces instead of function names).
- **Panic strategy set?** The release profile currently uses `panic = "abort"` for smaller binaries.
- **Tokio features minimal?** The project currently uses `features = ["full"]`. If auditing for size regressions, check whether narrower features can support the current async, process, IO, signal, and macro usage before changing this.
- **Unused dependencies?** Run `cargo machete` to detect unused deps (install with `cargo install cargo-machete`).

### 6. Compare over time

To track whether binary size is growing, compare before and after changes:

```bash
ls -lh target/release/cake
cargo bloat --release --crates | head -20
```

**What counts as a regression?** This project does not currently track a
committed binary-size baseline, so judgment is required. Sensible heuristics
(not measured from this project — treat as starting points to discuss with
the maintainer, not hard gates):

- A noticeable relative jump (rule of thumb: more than a few percent) in
  one change usually signals a new dependency or a wider feature set.
- A noticeable absolute jump (rule of thumb: hundreds of KB or more) is
  worth investigating even if relative growth is small.
- A single crate moving meaningfully up the `cargo bloat --crates` list
  warrants checking whether new features were enabled or a heavier
  dependency was pulled in.

If the project later commits a `binary-size-baseline.txt` or similar
artifact, prefer comparing against it over these heuristics.

Capture a local baseline before starting work so the comparison is
meaningful:

```bash
cargo build --release
ls -lh target/release/cake > /tmp/cake-size-before.txt
cargo bloat --release --crates | head -20 > /tmp/cake-bloat-before.txt
# ... make changes ...
cargo build --release
diff /tmp/cake-size-before.txt <(ls -lh target/release/cake)
diff /tmp/cake-bloat-before.txt <(cargo bloat --release --crates | head -20)
```

## Current Release Profile

The release profile lives in [`Cargo.toml`](../../../Cargo.toml). Inspect it
directly rather than relying on a copy here:

```bash
awk '/^\[profile.release\]/,/^$/' Cargo.toml
```

At time of writing it includes `lto`, `codegen-units`, `panic`, and `strip`
settings. If any of those are missing or changed unexpectedly, that is the
first place to look for size regressions or easy wins.

## Key Tradeoffs to Be Aware Of

- **`rustls` vs `native-tls`**: `rustls` bundles ~1 MB of crypto (`aws_lc_sys` + `rustls`). Switching to `native-tls` uses the OS TLS stack (smaller binary) but introduces platform-dependent build behavior, especially on Linux with OpenSSL.
- **`opt-level = "z"`**: Optimizes for size over speed. Not recommended unless binary size is critical.
- **`strip = true`**: Only downside is panic backtraces show raw addresses instead of function names. Logs via `tracing` are unaffected.
- **`panic = "abort"`**: Reduces unwind machinery. The tradeoff is no stack unwinding or panic recovery.

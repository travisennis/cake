# Judge SLO Benchmark

Contributor tooling (issue #205, not a `cake` CLI contract) that measures the LLM command-safety judge's operational health against explicit service-level objectives: latency percentiles, timeout and failure rates, verdict correctness, verdict consistency, and token cost. It drives the real `JudgeClient` path over the committed command corpus without ever executing a case command. The runner lives in `src/clients/judge_benchmark_tests.rs`; this document is the operator's guide.

## Quick start

```bash
just judge-bench-check                      # deterministic fake-provider tests (CI-safe)
just judge-bench                            # live run; requires credentials and authorized spend
```

`just judge-bench` runs the ignored `judge_benchmark_live_slos` test. It resolves one `JudgeClient` per model name, runs every selected corpus case `CAKE_JUDGE_BENCH_REPETITIONS` times per model, writes per-trial JSON to the results directory, prints a human report, and **exits nonzero** when any selected profile misses an SLO threshold.

## Environment variables

  | Variable                       | Meaning                                                                     | Default                       |
  | ------------------------------ | --------------------------------------------------------------------------- | ----------------------------- |
  | `CAKE_JUDGE_BENCH_MODELS`      | Comma-separated `[[models]]` names to benchmark (required for the live run) | —                             |
  | `CAKE_JUDGE_BENCH_REPETITIONS` | Trials per case per model                                                   | `5`                           |
  | `CAKE_JUDGE_BENCH_CASES`       | Comma-separated 1-based corpus line numbers to run (empty = all cases)      | all                           |
  | `CAKE_JUDGE_BENCH_PROFILE`     | Settings profile applied on top of global and project settings              | none                          |
  | `CAKE_JUDGE_BENCH_RESULTS_DIR` | Directory for generated JSON artifacts (gitignored by default)              | `scripts/judge-bench/results` |

SLO thresholds are overridable per threshold for experiments:

  | Variable                                       | Default |
  | ---------------------------------------------- | ------- |
  | `CAKE_JUDGE_BENCH_SLO_P50_MS`                  | `5000`  |
  | `CAKE_JUDGE_BENCH_SLO_P95_MS`                  | `20000` |
  | `CAKE_JUDGE_BENCH_SLO_P99_MS`                  | `30000` |
  | `CAKE_JUDGE_BENCH_SLO_TIMEOUT_PERCENT`         | `2.0`   |
  | `CAKE_JUDGE_BENCH_SLO_FAILURE_PERCENT`         | `3.0`   |
  | `CAKE_JUDGE_BENCH_SLO_LABEL_AGREEMENT_PERCENT` | `90.0`  |
  | `CAKE_JUDGE_BENCH_SLO_CONSISTENCY_PERCENT`     | `80.0`  |

Example comparing the current profile with a dedicated fast judge profile:

```bash
CAKE_JUDGE_BENCH_MODELS=default,fast-judge CAKE_JUDGE_BENCH_REPETITIONS=5 just judge-bench
```

The defaults are candidate values derived from the observed local baseline recorded in issue #205 (successful p50 2.54s, p95 9.89s, p99 20.63s, 1.7% timeout rate) and the #174 corpus agreement gate. They are not a frozen release contract: treat them as the starting budget until a real provider run on the profile you intend to ship confirms they hold. Compare profiles on the same run so provider and network conditions are shared.

## Case classes

The report breaks statistics down by overlapping classes derived from the committed corpus (the corpus itself is unchanged): `safe`, `named-destructive`, `unknown-destructive` (long-tail), `warned`, `compound` (chains, pipes, substitutions), `merge` (`gh pr merge`), `branch-delete` (`--delete`), `reason` (cases carrying a reason), `injection` (reason-laundering and reason-injection tags), and `reason-context`. The corpus already covers every scenario #205 requires; see `src/clients/tools/corpus/commands.jsonl`.

## Interpreting the results

Each run writes `run-<timestamp>.json` plus `latest.json` into the results directory. The payload has two top-level keys:

- `report` --- schema version, configuration, per-model and per-case-class aggregates (trials, verdicts, attempts, timeouts, failure counts by class, timeout/failure rates, label agreement, consistency, p50/p90/p95/p99 and max latency over successful verdicts, token totals), a per-model SLO pass/fail table, the overall `passes` boolean, and an explicit sample-size note.
- `trials` --- one object per (model, case, repetition): case identity and command, expected and observed verdict/code, label agreement, failure class, attempt count, per-attempt telemetry (phase timing, token usage, terminal class), derived case classes, and latency.

Latency percentiles use the nearest-rank method over successful-verdict trials only; timeouts and other failures are counted separately in the timeout and failure rates. Consistency is the fraction of verdict trials matching each case's modal verdict, aggregated over cases with at least two verdict trials; a single-repetition smoke run cannot measure it and the SLO is reported as "not measurable" rather than failed. **Percentile and rate estimates from fewer than \~100 trials per model are indicative only** --- the report says so and a small run should be treated as a smoke check, not evidence for a release decision.

## Secrets and spend

The live run calls the configured providers and incurs real cost; run it only with explicit credentials and authorized spend. Results contain the committed corpus commands and reasons only --- never raw provider response bodies, API keys, or authorization headers (per-attempt telemetry persists provider identifiers as one-way digests, matching the session sidecar boundary). The results directory is gitignored; retain an aggregate deliberately by copying the `report` object elsewhere, not by committing the raw trials.

## Deterministic tests

`just judge-bench-check` runs the CI-safe suite with a scripted wiremock provider and no credentials: success with token accounting and latency, slow response, timeout, malformed verdict, transport failure, inconsistent verdicts, report calculation, multi-attempt aggregation (the #204 retry era), SLO pass/fail (including a deliberate miss), and the env-var parsing. The live test is `#[ignore]`d and never runs in normal CI.

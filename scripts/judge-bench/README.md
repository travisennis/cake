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

Two retry-era details matter when reading a run after #204 (bounded judge recovery):

- `TrialRecord.latency_ms` is the **first attempt's** `total_ms` only, so the recovery's backoff wait and second request do not enter the latency percentiles. They show up in `attempt_count` (1 vs 2), per-attempt `retry_delay_ms`, and the run's wall-clock time; the availability win appears in the timeout and failure rates.
- A timeout trial now costs a backoff wait plus a second request in wall time (up to `timeout_secs + retry_budget_secs` per trial with defaults), so a full run takes visibly longer than the same run on `retry_budget_secs = 0`.

## Setting a baseline

A baseline is a recorded, reproducible reference run for the judge profile you intend to ship, kept so a future candidate model can be compared against the same numbers. Set one per reference profile, then treat it as the yardstick.

1. **Choose the reference profile.** The `[[models]]` name you ship as the judge: the agent's default model (the "same family by default" judge) or a dedicated `[tools.bash.judge] model` profile.

2. **Pick a statistically meaningful configuration.** Full corpus (omit `CAKE_JUDGE_BENCH_CASES`), `CAKE_JUDGE_BENCH_REPETITIONS=5` or higher. A run with fewer than \~100 trials per model is a smoke check, not a baseline.

3. **Run it:**

   ```bash
   CAKE_JUDGE_BENCH_MODELS=zen CAKE_JUDGE_BENCH_REPETITIONS=5 just judge-bench
   ```

   The exit code is the SLO gate: zero means the profile passes the current thresholds.

4. **If it fails the SLOs, decide deliberately.** Either the profile genuinely misses a threshold (override it with the `CAKE_JUDGE_BENCH_SLO_*` env vars for that run and record why) or the defaults need re-baselining to the observed numbers.

5. **Retain the baseline.** Copy the `report` object --- never the raw trials --- into a committed baseline record, for example `scripts/judge-bench/baselines/zen-2026-08-14.json`, and note the headline numbers in the issue. The results directory is gitignored, so a committed baseline is what survives.

6. **Pin the SLOs.** When a real run on the shipping profile confirms (or corrects) the candidate defaults in `SloThresholds::default`, update the harness defaults and document the change so the gate reflects the release contract. Until then the defaults remain the candidate budget, as the defaults comment states.

## Evaluating a new model

To answer "does this new model work better?", run the candidate against the reference in the **same invocation** so provider and network conditions are shared:

```bash
CAKE_JUDGE_BENCH_MODELS=zen,candidate-model CAKE_JUDGE_BENCH_REPETITIONS=5 just judge-bench
```

Then compare the per-model aggregates in the report (or in the saved JSONs):

- availability: `timeout_rate_percent`, `failure_rate_percent`, and `attempts` (how often recovery fired);
- latency: `latency.p50_ms`, `p90_ms`, `p95_ms`, `p99_ms`, `max_ms` --- first-attempt time only, per the note above;
- correctness: `label_agreement_percent` and `consistency_percent`;
- cost: `tokens` per model.

A candidate "works better" when it passes the SLOs and either beats the reference's timeout/failure rates at comparable latency and correctness, or buys a meaningfully lower timeout/failure rate at a latency and token cost you accept. Keep repetitions at 5 for a real decision --- consistency is only measurable with at least two verdict trials per case.

If a same-run comparison is impractical (for example the candidate profile changes settings the run loads), keep the reference baseline JSON from step 5 above and diff the two reports directly; note that this compares across time and network conditions.

When a candidate wins, point the judge at it (`[tools.bash.judge] model` or the default model), re-run the baseline procedure to pin new numbers, and record the comparison and decision in the issue.

## Secrets and spend

The live run calls the configured providers and incurs real cost; run it only with explicit credentials and authorized spend. Results contain the committed corpus commands and reasons only --- never raw provider response bodies, API keys, or authorization headers (per-attempt telemetry persists provider identifiers as one-way digests, matching the session sidecar boundary). The results directory is gitignored; retain an aggregate deliberately by copying the `report` object elsewhere, not by committing the raw trials.

## Deterministic tests

`just judge-bench-check` runs the CI-safe suite with a scripted wiremock provider and no credentials: success with token accounting and latency, slow response, timeout, malformed verdict, transport failure, inconsistent verdicts, report calculation, multi-attempt aggregation (the #204 retry era), SLO pass/fail (including a deliberate miss), and the env-var parsing. The live test is `#[ignore]`d and never runs in normal CI.

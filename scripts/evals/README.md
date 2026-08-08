# Controlled Model Evaluation Harness

Repository-local contributor tooling (not a `cake eval` CLI contract) that runs the same deterministic fixture coding tasks across selected model configurations and judges the resulting repository state with trusted verifier commands. It measures task correctness, not operational health: cake's session `success` subtype is never treated as correctness.

## Quick start

```bash
just eval-cases                      # list committed fixtures
just eval-check                      # automated tests (fake cake, no credentials)
just eval --model <name> --repetitions 3
```

`just eval` invokes the real `cake` binary, so it requires configured model credentials and authorized external spend. `--cake /path/to/cake` overrides the executable (default: `cake` on `PATH`).

## Fixtures

Each fixture is a directory under `cases/`:

- `manifest.json` --- fixture metadata (schema below).
- `repo/` --- the initial Git repository state handed to the model.
- `verify.sh` --- the trusted verifier, run in the model's work repository.

`manifest.json` fields:

  | Field             | Meaning                                                                                                                                                                                                                                                                         |
  | ----------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | `name`            | Case name (defaults to the directory name).                                                                                                                                                                                                                                     |
  | `description`     | One-line summary shown by `just eval-cases`.                                                                                                                                                                                                                                    |
  | `prompt`          | The exact task prompt handed to every model. Pin the expected artifact precisely.                                                                                                                                                                                               |
  | `verify`          | Shell command (run with `bash -c`) in the work repo. `$EVAL_CASE_DIR` points at the case directory. Must be deterministic, repository-owned, and network-free; verifiers for test-bearing fixtures must ignore `__pycache__` bytecode caches left by the model's own test runs. |
  | `timeout_seconds` | Per-trial timeout for the cake process; exceeding it yields `timeout`.                                                                                                                                                                                                          |
  | `tags`            | Labels used by `--tags` and the summary.                                                                                                                                                                                                                                        |
  | `expected`        | Documented expected outcome (one of the outcome values below).                                                                                                                                                                                                                  |

Fixtures must not embed credentials or machine-specific absolute paths. The verifier is the correctness oracle and must never be an LLM judge or a network-dependent command.

## Outcomes

  | Outcome          | Meaning                                                                                                                                                  |
  | ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
  | `correct`        | Cake exited 0 and the verifier passed.                                                                                                                   |
  | `incorrect`      | Cake exited 0 and the verifier failed.                                                                                                                   |
  | `cake_error`     | Cake exited 1 (agent or tool execution error).                                                                                                           |
  | `provider_error` | Cake exited 2 (authentication, rate-limit, or network error).                                                                                            |
  | `timeout`        | Cake exceeded the case timeout.                                                                                                                          |
  | `harness_error`  | Malformed completion JSON, a broken invocation (cake exit 3, which also covers missing credentials), a verifier that could not run, or a harness defect. |

## Isolation

Each trial copies the fixture's `repo/` into a fresh temporary Git repository (with one initial commit), runs cake with that directory as the working directory, runs the verifier there, and deletes the temporary repository on every path, including errors and interrupts. An interrupt (Ctrl-C) also terminates the running cake process group, matching cake's `130` interrupted exit code. The fixture sources and the cake source tree are never modified. `CAKE_DATA_DIR` points at `<results-dir>/data`, so generated transcripts and telemetry stay inside the ignored results directory.

## Results

Generated output lives in `scripts/evals/results/` (gitignored; the directory is disposable). Each run writes `run-<timestamp>.json` plus `latest.json` with the same content:

- `schema_version` --- stable schema version for machine readers.
- `configuration` --- models, repetitions, cases, and cake command.
- `trials` --- one object per (model, case, repetition) with `outcome`, `exit_code`, `duration_ms`, `turns`, `tool_calls`, `tool_failures`, `usage`, `model_reported`, `cake_elapsed_ms`, `result_preview`, `error`, `verifier`, `session_id`, and `session_file`.
- `summary` --- correctness rate and median/p90 turns, tokens, duration, and tool failures, overall, by model, and by case tag.

A concise human table is printed to stdout. `--results-dir` relocates the output directory.

## Tests

`just eval-check` runs the stdlib-only `unittest` suite under `tests/`, which uses the fake cake executable (`fake_cake.py`, configured through the `FAKE_CAKE_SCRIPT` environment variable) and temporary fixtures. It covers correct output, verifier failure, verifier launch failure (126/127 as `harness_error`), malformed completion JSON, exit-code classification, timeout, interrupt termination of the cake subprocess, repeated trials, identical presentation across models, temporary-repository cleanup, missing-verifier manifest validation, and the committed fixtures (their verifiers must fail on the initial state, pass on the intended solution, and tolerate `__pycache__` from test runs).

## Notes

- Correctness is decided by the fixture verifier alone; cake's `success` subtype is never treated as correctness.
- Runs are sequential and deterministic (models in CLI order, cases sorted by name, repetitions in order).
- No LLM judge is used anywhere in this harness.
- Real-model runs require credentials and authorized external spend and were not executed during the #84 implementation. Maintainers can run `just eval --model <name> --repetitions 3` locally; add `--cake /path/to/cake` when cake is not on `PATH`.

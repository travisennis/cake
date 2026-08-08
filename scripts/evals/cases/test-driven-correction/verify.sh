#!/usr/bin/env bash
set -euo pipefail
# Test files must stay byte-identical to the fixture's initial state. Python
# bytecode caches (__pycache__) left by the model's own test runs are ignored.
while IFS= read -r f; do
    rel=${f#"$EVAL_CASE_DIR/repo/tests"/}
    cmp -s "$f" "tests/$rel"
done < <(find "$EVAL_CASE_DIR/repo/tests" -type f ! -path "*/__pycache__/*" | sort)
PYTHONDONTWRITEBYTECODE=1 python3 tests/test_validate.py

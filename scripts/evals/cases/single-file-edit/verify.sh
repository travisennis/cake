#!/usr/bin/env bash
set -euo pipefail
grep -q "^PORT = 9090$" src/config.py
! grep -q "^PORT = 8080$" src/config.py
for f in $(cd "$EVAL_CASE_DIR/repo" && find . -type f | sed 's|^\./||'); do
    if [ "$f" != "src/config.py" ]; then
        diff -q "$EVAL_CASE_DIR/repo/$f" "$f" >/dev/null
    fi
done

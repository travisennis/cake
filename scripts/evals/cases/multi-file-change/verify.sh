#!/usr/bin/env bash
set -euo pipefail
PYTHONDONTWRITEBYTECODE=1 python3 tests/test_rename.py
! grep -rn --exclude-dir="__pycache__" "format_price" src/ tests/

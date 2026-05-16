#!/bin/sh

# Block `cargo test --lib` in the cake repo. This project is binary-only;
# there is no library target, so `cargo test --lib` always fails. Use
# `cargo test <module_or_test_name>` or `cargo test` instead.

if ! command -v python3 >/dev/null 2>&1; then
  exit 0
fi

python3 -c '
import json
import re
import sys

try:
    payload = json.load(sys.stdin)
except Exception:
    sys.exit(0)

tool_name = payload.get("tool_name", "")
if tool_name != "Bash":
    sys.exit(0)

tool_input = payload.get("tool_input") or {}
if not isinstance(tool_input, dict):
    tool_input = {}
if not tool_input:
    raw_tool_input = payload.get("tool_input_json")
    if isinstance(raw_tool_input, str):
        try:
            tool_input = json.loads(raw_tool_input)
        except Exception:
            tool_input = {}
if not isinstance(tool_input, dict):
    tool_input = {}

command = str(tool_input.get("cmd") or tool_input.get("command") or "")

# Only inspect the cargo portion of the command (before any `--` separator
# that passes arguments to the test binary). This avoids false positives
# like `cargo test -- --lib` where `--lib` is a test filter, not a cargo
# flag.
cargo_part = command.split(" -- ", 1)[0]

# Match `cargo test --lib` as a distinct flag, not `cargo test --libfoo`.
# The pattern requires --lib to appear as a standalone flag (preceded by
# whitespace or start-of-string, followed by end-of-string or whitespace).
if re.search(r"(?:^|\s)cargo\s+test\s+.*--lib(?:\s|$)", cargo_part):
    print(json.dumps({
        "decision": "deny",
        "reason": (
            "Blocked: `cargo test --lib` is not valid for this project. "
            "cake is a binary-only crate with no library target. Use "
            "`cargo test <module_or_test_name>` for targeted tests, or "
            "`cargo test` for the full suite."
        ),
    }))
    sys.exit(0)
'
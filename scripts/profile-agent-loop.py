#!/usr/bin/env python3
"""Run Cake's deterministic agent-loop profiling workload."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any


DEFAULT_BINARY = Path("target/profiling/cake")
DEFAULT_OUTPUT = Path("profiling/artifacts/agent-loop.jslb.gz")
DEFAULT_TOOL_CALLS = 5_000
API_KEY_ENV = "CAKE_PROFILE_API_KEY"
XCTRACE_TIME_LIMIT = "30s"
READ_OUTPUT_MARKERS = (
    "Lines 1-32/32",
    *(f"{number:>6}: profile line {number}" for number in range(1, 33)),
)


def positive_int(value: str) -> int:
    """Parse a strictly positive integer for workload sizing."""
    try:
        parsed = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("must be an integer") from error
    if parsed < 1:
        raise argparse.ArgumentTypeError("must be at least 1")
    return parsed


def function_calls(fixture_path: Path, count: int) -> list[dict[str, Any]]:
    """Build one tool-heavy response with uniquely identifiable Read calls."""
    arguments = json.dumps(
        {
            "path": str(fixture_path),
            "start_line": 1,
            "end_line": 32,
        }
    )
    return [
        {
            "type": "function_call",
            "id": f"profile-function-call-{number}",
            "call_id": f"profile-call-{number}",
            "name": "Read",
            "arguments": arguments,
        }
        for number in range(1, count + 1)
    ]


def tool_output_error(input_items: Any, expected_call_ids: set[str]) -> str | None:
    """Return why the second request does not prove every Read succeeded."""
    if not isinstance(input_items, list):
        return "second request input must be an array"

    outputs: dict[str, Any] = {}
    for item in input_items:
        if not isinstance(item, dict) or item.get("type") != "function_call_output":
            continue
        call_id = item.get("call_id")
        if not isinstance(call_id, str):
            return "function call output is missing a string call_id"
        if call_id in outputs:
            return f"second request contains duplicate output for {call_id}"
        outputs[call_id] = item.get("output")

    actual_call_ids = set(outputs)
    if actual_call_ids != expected_call_ids:
        missing = len(expected_call_ids - actual_call_ids)
        unexpected = len(actual_call_ids - expected_call_ids)
        return (
            "second request tool outputs do not match the requested batch "
            f"(missing {missing}, unexpected {unexpected})"
        )

    for call_id, output in outputs.items():
        if not isinstance(output, str) or not all(
            marker in output for marker in READ_OUTPUT_MARKERS
        ):
            return f"{call_id} did not return the complete profiling fixture"
    return None


class ResponsesHandler(BaseHTTPRequestHandler):
    """Serve two deterministic Responses API responses on localhost."""

    server: "ProfileServer"

    def do_POST(self) -> None:  # noqa: N802 - required by BaseHTTPRequestHandler
        if self.path.rstrip("/") != "/responses":
            self.send_error(404)
            return

        try:
            length = int(self.headers.get("Content-Length", "0"))
            request = json.loads(self.rfile.read(length))
        except (ValueError, json.JSONDecodeError):
            self.send_error(400, "request body must be JSON")
            return
        if not isinstance(request, dict):
            self.send_error(400, "request body must be a JSON object")
            return

        with self.server.request_lock:
            request_number = self.server.request_count
            self.server.request_count += 1

        if request_number == 0:
            response: dict[str, Any] = {
                "id": "profile-tool-turn",
                "output": function_calls(
                    self.server.fixture_path, self.server.tool_call_count
                ),
                "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5},
            }
        elif request_number == 1:
            output_error = tool_output_error(
                request.get("input"), self.server.expected_call_ids
            )
            if output_error is not None:
                self._send_json(
                    500,
                    {"error": {"message": output_error}},
                )
                return
            response = {
                "id": "profile-final-turn",
                "output": [
                    {
                        "type": "message",
                        "id": "profile-message",
                        "status": "completed",
                        "content": [
                            {
                                "type": "output_text",
                                "text": "Profiling workload complete.",
                            }
                        ],
                    }
                ],
                "usage": {"input_tokens": 4, "output_tokens": 1, "total_tokens": 5},
            }
        else:
            self._send_json(
                500,
                {"error": {"message": "profiling workload made more than two requests"}},
            )
            return

        self._send_json(200, response)

    def _send_json(self, status: int, payload: dict[str, Any]) -> None:
        encoded = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        self.wfile.write(encoded)

    def log_message(self, _format: str, *_args: Any) -> None:
        # Keep the profiled command's stderr focused on Cake and the profiler.
        return


class ProfileServer(ThreadingHTTPServer):
    """HTTP server state shared with ``ResponsesHandler``."""

    daemon_threads = True

    def __init__(self, fixture_path: Path, tool_call_count: int) -> None:
        super().__init__(("127.0.0.1", 0), ResponsesHandler)
        self.fixture_path = fixture_path
        self.tool_call_count = tool_call_count
        self.expected_call_ids = {
            f"profile-call-{number}" for number in range(1, tool_call_count + 1)
        }
        self.request_count = 0
        self.request_lock = threading.Lock()


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser(
        description="Profile Cake with a deterministic local Responses API workload."
    )
    result.add_argument(
        "--binary",
        type=Path,
        default=DEFAULT_BINARY,
        help=f"profiling binary (default: {DEFAULT_BINARY})",
    )
    result.add_argument(
        "--output",
        type=Path,
        default=DEFAULT_OUTPUT,
        help=f"profiler artifact path (default: {DEFAULT_OUTPUT})",
    )
    result.add_argument(
        "--profiler",
        choices=("samply", "instruments"),
        default="samply",
        help=(
            "profiler to run: samply for the primary CPU workflow or instruments "
            "for optional macOS allocation inspection"
        ),
    )
    result.add_argument(
        "--tool-calls",
        type=positive_int,
        default=DEFAULT_TOOL_CALLS,
        help=(
            "Read calls in the profiled tool-heavy batch "
            f"(default: {DEFAULT_TOOL_CALLS})"
        ),
    )
    return result


def profiler_command(
    profiler: str, binary: Path, output: Path
) -> list[str]:
    cake_args = [
        str(binary),
        "--sandbox",
        "read-only",
        "--output-format",
        "json",
        "--no-session",
        "--model",
        "profile",
        "Read the profiling fixture and report what you found.",
    ]
    if profiler == "samply":
        return [
            "samply",
            "record",
            "--save-only",
            "--output",
            str(output),
            "--",
            *cake_args,
        ]
    return [
        "xcrun",
        "xctrace",
        "record",
        "--template",
        "Allocations",
        "--output",
        str(output),
        "--time-limit",
        XCTRACE_TIME_LIMIT,
        "--target-stdout",
        "-",
        "--launch",
        "--",
        *cake_args,
    ]


def profiler_is_available(profiler: str) -> bool:
    if profiler == "samply":
        return shutil.which("samply") is not None
    if shutil.which("xcrun") is None:
        return False
    return subprocess.run(
        ["xcrun", "--find", "xctrace"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    ).returncode == 0


def main() -> int:
    args = parser().parse_args()
    binary = args.binary.resolve()
    output = args.output.resolve()

    if not binary.is_file():
        print(f"error: profiling binary does not exist: {binary}", file=sys.stderr)
        return 2

    if not profiler_is_available(args.profiler):
        if args.profiler == "samply":
            print(
                "error: samply is not installed; install it with `cargo install samply --locked`",
                file=sys.stderr,
            )
        else:
            print(
                "error: xctrace is not available; install Xcode and select its developer directory",
                file=sys.stderr,
            )
        return 2

    output.parent.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="cake-profile-") as temporary_root:
        root = Path(temporary_root)
        workspace = root / "workspace"
        workspace.mkdir()
        (workspace / ".cake").mkdir()
        fixture = workspace / "profile-fixture.txt"
        fixture.write_text(
            "\n".join(f"profile line {number}" for number in range(1, 33)) + "\n",
            encoding="utf-8",
        )

        server = ProfileServer(fixture, args.tool_calls)
        settings = workspace / ".cake" / "settings.toml"
        settings.write_text(
            "\n".join(
                [
                    'default_model = "profile"',
                    "",
                    "[[models]]",
                    'name = "profile"',
                    'model = "local-profile-model"',
                    f'base_url = "http://127.0.0.1:{server.server_address[1]}"',
                    f'api_key_env = "{API_KEY_ENV}"',
                    'api_type = "responses"',
                    "",
                ]
            ),
            encoding="utf-8",
        )

        server_thread = threading.Thread(target=server.serve_forever, daemon=True)
        server_thread.start()
        environment = os.environ.copy()
        home = root / "home"
        environment.update(
            {
                "HOME": str(home),
                "XDG_CONFIG_HOME": str(home / ".config"),
                "CAKE_DATA_DIR": str(root / "data"),
                API_KEY_ENV: "local-profile-key",
            }
        )
        environment.pop("CAKE_TOOLBOX", None)

        try:
            command = profiler_command(args.profiler, binary, output)
            completed = subprocess.run(command, cwd=workspace, env=environment, check=False)
        finally:
            server.shutdown()
            server.server_close()
            server_thread.join()

        if completed.returncode != 0:
            return completed.returncode

        if server.request_count != 2:
            print(
                f"error: profiling workload expected 2 provider requests, got {server.request_count}",
                file=sys.stderr,
            )
            return 1

    # samply writes a file; xctrace writes an Instruments .trace bundle directory.
    if not output.exists():
        print(f"error: profiler did not create {output}", file=sys.stderr)
        return 1

    print(f"Profile written to {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

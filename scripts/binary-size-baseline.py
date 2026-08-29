#!/usr/bin/env python3
"""Write a target-specific release binary size baseline for Cake."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path


DEFAULT_ROOT = Path(__file__).resolve().parents[1]
ROOT = DEFAULT_ROOT


def display_path(path: Path) -> str:
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def command_output(*command: str) -> str:
    return subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def rustc_host() -> str:
    version_info = command_output("rustc", "-vV")
    for line in version_info.splitlines():
        name, separator, value = line.partition(": ")
        if separator and name == "host":
            return value
    raise RuntimeError("rustc -vV did not report a host target")


def load_baselines(output: Path) -> dict[str, object]:
    if not output.exists():
        return {}

    try:
        document = json.loads(output.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise RuntimeError(f"invalid JSON in {display_path(output)}: {error}") from error

    if not isinstance(document, dict):
        raise RuntimeError("binary-size baseline must be a JSON object")
    if document.get("format_version") != 1:
        raise RuntimeError("unsupported binary-size baseline format")
    if document.get("package") != "cake" or document.get("profile") != "release":
        raise RuntimeError("binary-size baseline belongs to a different package or profile")

    baselines = document.get("baselines")
    if not isinstance(baselines, dict):
        raise RuntimeError("binary-size baseline `baselines` must be an object")
    return baselines


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Record the exact size of a Cake release binary for one target."
    )
    parser.add_argument(
        "--root",
        type=Path,
        default=DEFAULT_ROOT,
        help="repository root; defaults to the script's repository",
    )
    parser.add_argument(
        "--target",
        help="Rust target triple; defaults to the host target",
    )
    parser.add_argument(
        "--artifact",
        type=Path,
        help="release binary path; defaults to target/release/cake for the host or target/<triple>/release/cake",
    )
    parser.add_argument(
        "--output",
        type=Path,
        help="baseline JSON path; defaults to ci/binary-size-baseline.json below --root",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    global ROOT
    args = parse_args(argv)
    ROOT = args.root.resolve()
    target = args.target or rustc_host()
    artifact = args.artifact or (
        Path("target") / target / "release" / "cake"
        if args.target
        else Path("target/release/cake")
    )
    if not artifact.is_absolute():
        artifact = ROOT / artifact
    artifact = artifact.resolve()

    if not artifact.is_file():
        print(
            f"error: {artifact} is missing; build the release artifact first",
            file=sys.stderr,
        )
        return 1

    output = args.output or Path("ci/binary-size-baseline.json")
    if not output.is_absolute():
        output = ROOT / output
    output = output.resolve()

    try:
        artifact_name = str(artifact.relative_to(ROOT))
        baselines = load_baselines(output)
    except (RuntimeError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    size_bytes = artifact.stat().st_size
    baselines[target] = {
        "toolchain": {
            "rustc": command_output("rustc", "--version"),
            "cargo": command_output("cargo", "--version"),
        },
        "artifact": artifact_name,
        "size_bytes": size_bytes,
    }

    document = {
        "format_version": 1,
        "package": "cake",
        "profile": "release",
        "baselines": {name: baselines[name] for name in sorted(baselines)},
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
    print(f"Wrote {display_path(output)} for {target}: {size_bytes} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

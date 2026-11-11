#!/usr/bin/env python3
"""Read-only developer environment readiness report for the Cake checkout.

Usage: scripts/doctor.py [--json] [--scope {all,setup}] [--quiet]

The report groups checks by the capability they unlock: the baseline
`just check` gate, the full `just check-full` gate, the installed git hooks,
and optional capabilities such as profiling and cross-compilation. `--scope
setup` narrows the report to what `just setup` promises: every pinned Cargo
utility and the three hook wrappers.

Doctor never installs anything, runs a hook, changes checkout or configuration
state, contacts a provider, or prints secrets. It reads the pins from
`.mise.toml`, `rust-toolchain.toml`, and `scripts/dev-tools.sh`, so it does not
hold a second copy of any version.

Exit codes: 0 ready, 1 a required check failed, 2 usage or configuration error.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REQUIRED_GROUPS = ("baseline", "full", "hooks")
GROUP_ORDER = ("baseline", "full", "hooks", "optional")
GROUP_LABELS = {
    "baseline": "Baseline (just check)",
    "full": "Full gate (just check-full)",
    "hooks": "Git hooks",
    "optional": "Optional capabilities",
    "setup": "Setup postconditions",
}
HOOK_NAMES = ("pre-commit", "pre-push", "commit-msg")
CROSS_TARGET = "x86_64-unknown-linux-gnu"

# Which capability a pinned Cargo utility unlocks. A pin with no mapping fails
# the report until this table learns about it, so adding a pin cannot silently
# leave a capability unchecked.
PIN_GROUPS = {
    "cargo-edit": ("optional",),
    "cargo-deny": ("full",),
    "cargo-insta": ("optional",),
    "cargo-llvm-cov": ("full",),
    "cargo-crap": ("baseline", "full"),
    "panache": ("full", "hooks"),
    "prek": ("hooks",),
    "cocogitto": ("hooks",),
}


class ConfigError(Exception):
    """The repository's version-owning configuration cannot be read."""


@dataclass
class Check:
    name: str
    groups: tuple[str, ...]
    status: str = "ok"
    setup: bool = False
    expected: str | None = None
    found: str | None = None
    detail: str | None = None
    hint: str | None = None
    path: str | None = None


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        raise ConfigError(f"cannot read {path}: {exc}") from exc


def read_config() -> dict[str, object]:
    mise_path = ROOT / ".mise.toml"
    toolchain_path = ROOT / "rust-toolchain.toml"
    mise = read_text(mise_path)
    toolchain = read_text(toolchain_path)

    rust = re.search(r'^\s*rust\s*=\s*\{\s*version\s*=\s*"([^"]+)"', mise, re.M)
    just = re.search(r'^\s*just\s*=\s*"([^"]+)"', mise, re.M)
    sccache = re.search(r'^\s*sccache\s*=\s*"([^"]+)"', mise, re.M)
    channel = re.search(r'^\s*channel\s*=\s*"([^"]+)"', toolchain, re.M)
    components_match = re.search(r"components\s*=\s*\[(.*?)\]", toolchain, re.S)

    if rust is None:
        raise ConfigError(f"could not read the Rust pin from {mise_path}")
    if just is None:
        raise ConfigError(f"could not read the just pin from {mise_path}")
    if channel is None:
        raise ConfigError(f"could not read the Rust channel from {toolchain_path}")

    components = re.findall(r'"([^"]+)"', components_match.group(1)) if components_match else []
    return {
        "rust": channel.group(1),
        "mise_rust": rust.group(1),
        "just": just.group(1),
        "sccache": sccache.group(1) if sccache else None,
        "components": components,
    }


def run_command(argv: list[object], cwd: Path = ROOT) -> tuple[subprocess.CompletedProcess[str] | None, str | None]:
    # Rustup downloads a missing pinned toolchain on first use. Doctor is
    # read-only, so disable that mutation for every probe; a missing toolchain
    # then fails locally and the check reports the recovery instead.
    environment = os.environ.copy()
    environment["RUSTUP_AUTO_INSTALL"] = "0"
    try:
        completed = subprocess.run(
            [str(arg) for arg in argv],
            cwd=str(cwd),
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
            env=environment,
        )
    except FileNotFoundError:
        return None, "not found"
    except subprocess.TimeoutExpired:
        return None, "timed out"
    except OSError as exc:
        return None, str(exc)
    return completed, None


def command_exists(command: object) -> bool:
    text = str(command)
    if os.sep in text:
        return os.access(text, os.X_OK)
    return shutil.which(text) is not None


def first_line(text: str | None) -> str | None:
    if not text:
        return None
    for line in text.splitlines():
        if line.strip():
            return line.strip()
    return None


def shorten(text: str | None, limit: int = 100) -> str | None:
    if text is None or len(text) <= limit:
        return text
    return text[: limit - 3].rstrip() + "..."


def prefixed(prefix: str):
    def parse(text: str) -> str | None:
        match = re.search(rf"^\s*{re.escape(prefix)}\s+(\S+)", text, re.M)
        return match.group(1) if match else None

    return parse


def version_tuple(value: str | None) -> tuple[int, int, int] | None:
    if not value:
        return None
    match = re.match(r"(\d+)\.(\d+)\.(\d+)", value)
    if match is None:
        return None
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]


def license_hint(output: str, returncode: int) -> str | None:
    lowered = output.lower()
    if returncode == 69 or "xcode license" in lowered or "license agreements" in lowered:
        return "accept the Xcode license in a terminal: sudo xcodebuild -license accept"
    return None


def check_command(
    name: str,
    groups: tuple[str, ...],
    argv: list[str],
    *,
    expected: str | None = None,
    parser=None,
    hint: str | None = None,
    setup: bool = False,
) -> Check:
    completed, error = run_command(argv)
    if completed is None:
        missing = not command_exists(argv[0])
        return Check(
            name,
            groups,
            status="missing" if missing else "broken",
            setup=setup,
            expected=expected,
            detail="not found" if missing else error,
            hint=hint,
        )
    if completed.returncode != 0:
        output = "\n".join(part for part in (completed.stdout, completed.stderr) if part)
        detail = first_line(output) or f"exit {completed.returncode}"
        return Check(
            name,
            groups,
            status="broken",
            setup=setup,
            expected=expected,
            detail=shorten(detail),
            hint=license_hint(output, completed.returncode) or hint,
        )

    output = completed.stdout or completed.stderr
    found = parser(output) if parser else first_line(output)
    if found is None:
        return Check(
            name,
            groups,
            status="broken",
            setup=setup,
            expected=expected,
            detail="could not read the version output",
            hint=hint,
        )
    if expected is not None and found != expected:
        return Check(
            name,
            groups,
            status="version-mismatch",
            setup=setup,
            expected=expected,
            found=found,
            detail=f"expected {expected}, found {found}",
            hint=hint,
        )
    return Check(name, groups, status="ok", setup=setup, expected=expected, found=found, hint=hint)


def check_just(expected: str) -> Check:
    completed, error = run_command(["just", "--version"])
    if completed is None:
        status = "missing" if not command_exists("just") else "broken"
        return Check("just", ("baseline",), status=status, expected=expected, detail=error, hint=just_hint(expected, None))
    if completed.returncode != 0:
        detail = first_line(completed.stderr) or first_line(completed.stdout) or f"exit {completed.returncode}"
        return Check("just", ("baseline",), status="broken", expected=expected, detail=detail, hint=just_hint(expected, None))

    found = prefixed("just")(completed.stdout or completed.stderr)
    if found is None:
        return Check(
            "just",
            ("baseline",),
            status="broken",
            expected=expected,
            detail="could not read `just --version`",
            hint=just_hint(expected, None),
        )
    if found == expected:
        return Check("just", ("baseline",), status="ok", expected=expected, found=found)

    found_tuple = version_tuple(found)
    expected_tuple = version_tuple(expected)
    if found_tuple is not None and expected_tuple is not None and found_tuple > expected_tuple:
        return Check(
            "just",
            ("baseline",),
            status="newer",
            expected=expected,
            found=found,
            detail=f"{found} is newer than the .mise.toml pin",
            hint=just_hint(expected, found),
        )
    return Check(
        "just",
        ("baseline",),
        status="version-mismatch",
        expected=expected,
        found=found,
        detail=f"expected {expected}, found {found}",
        hint=just_hint(expected, found),
    )


def just_hint(expected: str, found: str | None) -> str:
    if found is not None and version_tuple(found) is not None and version_tuple(expected) is not None:
        if version_tuple(found) > version_tuple(expected):
            return f"newer just is compatible; run `mise install` if you intend to match the {expected} pin"
    return f"run `mise install` to install just {expected}, or update .mise.toml to the version you intend"


def check_component(component: str, groups: tuple[str, ...]) -> Check:
    completed, _ = run_command(["rustup", "component", "list", "--installed"])
    if completed is not None and completed.returncode == 0:
        installed = [line.strip() for line in completed.stdout.splitlines() if line.strip()]
        if any(line == component or line.startswith(component + "-") for line in installed):
            return Check(component, groups, status="ok", found="installed")
        return Check(
            component,
            groups,
            status="missing-component",
            detail="component not installed",
            hint=f"rustup component add {component}",
        )

    if component == "clippy":
        fallback = ["cargo", "clippy", "--version"]
    elif component == "rustfmt":
        fallback = ["cargo", "fmt", "--version"]
    else:
        fallback = [component, "--version"]
    completed, error = run_command(fallback)
    if completed is not None and completed.returncode == 0:
        return Check(component, groups, status="ok", found="installed", detail="verified without rustup component metadata")
    detail = error or first_line(completed.stderr if completed else None) or "not installed"
    return Check(
        component,
        groups,
        status="missing-component",
        detail=detail,
        hint=f"rustup component add {component}",
    )


def check_pins() -> list[Check]:
    script = ROOT / "scripts" / "dev-tools.sh"
    completed, error = run_command([script, "--json"])
    if completed is None:
        return [
            Check(
                "pinned development tools",
                ("baseline", "full", "hooks"),
                status="broken",
                setup=True,
                detail=error,
                hint="run `just setup`",
            )
        ]
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError:
        detail = first_line(completed.stderr) or "the checker did not return JSON"
        return [
            Check(
                "pinned development tools",
                ("baseline", "full", "hooks"),
                status="broken",
                setup=True,
                detail=shorten(detail),
                hint="run `just setup`",
            )
        ]

    if payload.get("error"):
        return [
            Check(
                "pinned development tools",
                ("baseline", "full", "hooks"),
                status="broken",
                setup=True,
                detail=payload["error"],
                hint="install Rust from https://rustup.rs, then run `just setup`",
            )
        ]

    tools = payload.get("tools")
    if not isinstance(tools, list):
        return [
            Check(
                "pinned development tools",
                ("baseline", "full", "hooks"),
                status="broken",
                setup=True,
                detail="the checker returned no tool list",
                hint="run `just setup`",
            )
        ]

    checks: list[Check] = []
    seen: set[str] = set()
    for tool in tools:
        if not isinstance(tool, dict):
            continue
        package = tool.get("package")
        if not isinstance(package, str):
            continue
        seen.add(package)
        version = tool.get("version")
        installed = tool.get("installed_version")
        checker_status = tool.get("status", "broken")
        detail = tool.get("detail")
        if package not in PIN_GROUPS:
            checks.append(
                Check(
                    package,
                    ("full",),
                    status="broken",
                    setup=True,
                    expected=version,
                    found=installed,
                    detail="the checker reported a pin with no capability mapping in scripts/doctor.py",
                    hint="add the package to PIN_GROUPS in scripts/doctor.py",
                )
            )
            continue
        hint = None
        if checker_status != "ok":
            hint = (
                f"run `just setup` (installs {package} {version}); "
                f"or run `cargo install {package} --version {version} --locked`"
            )
        checks.append(
            Check(
                package,
                PIN_GROUPS[package],
                status=checker_status,
                setup=True,
                expected=version,
                found=installed,
                detail=detail,
                hint=hint,
            )
        )

    for package, groups in PIN_GROUPS.items():
        if package not in seen:
            checks.append(
                Check(
                    package,
                    groups,
                    status="broken",
                    setup=True,
                    detail="the checker did not report this pin",
                    hint="run `just setup`, then `just doctor` again",
                )
            )
    return checks


def check_hooks() -> list[Check]:
    completed, error = run_command(["git", "rev-parse", "--git-path", "hooks"])
    if completed is None or completed.returncode != 0:
        detail = error or first_line(completed.stderr if completed else None) or "git is unavailable"
        return [
            Check(
                hook,
                ("hooks",),
                status="broken",
                setup=True,
                detail=f"cannot locate the hooks directory: {detail}",
                hint="run doctor from inside the checkout",
            )
            for hook in HOOK_NAMES
        ]

    hooks_dir = Path(completed.stdout.strip())
    if not hooks_dir.is_absolute():
        hooks_dir = ROOT / hooks_dir

    checks: list[Check] = []
    for hook in HOOK_NAMES:
        path = hooks_dir / hook
        if not path.is_file():
            checks.append(
                Check(
                    hook,
                    ("hooks",),
                    status="missing-hook",
                    setup=True,
                    detail=f"missing at {path}",
                    hint="run `just setup` to install the git hooks",
                )
            )
            continue
        if not os.access(path, os.X_OK):
            checks.append(
                Check(
                    hook,
                    ("hooks",),
                    status="missing-hook",
                    setup=True,
                    detail=f"not executable at {path}",
                    hint="run `just setup` to reinstall the git hooks",
                )
            )
            continue
        try:
            header = path.read_text(encoding="utf-8", errors="replace")[:4096]
        except OSError as exc:
            checks.append(
                Check(
                    hook,
                    ("hooks",),
                    status="broken",
                    setup=True,
                    detail=f"cannot read {path}: {exc}",
                    hint="run `just setup` to reinstall the git hooks",
                )
            )
            continue
        if "prek" not in header:
            checks.append(
                Check(
                    hook,
                    ("hooks",),
                    status="broken",
                    setup=True,
                    detail=f"not generated by prek at {path}",
                    hint="run `just setup` to reinstall the git hooks",
                )
            )
            continue
        checks.append(Check(hook, ("hooks",), status="ok", setup=True, found="installed", path=str(path)))
    return checks


def check_sccache(expected: str | None) -> Check:
    wrapper = os.environ.get("RUSTC_WRAPPER", "")
    active = bool(wrapper) and Path(wrapper).name == "sccache"
    groups = ("baseline",) if active else ("optional",)
    command = wrapper if active else "sccache"
    completed, error = run_command([command, "--version"])
    if completed is None:
        if not active:
            return Check("sccache", groups, status="skipped", detail="not active (RUSTC_WRAPPER is unset or names another wrapper)")
        return Check(
            "sccache",
            groups,
            status="missing",
            expected=expected,
            detail=error,
            hint="install sccache or unset RUSTC_WRAPPER",
        )
    if completed.returncode != 0:
        return Check(
            "sccache",
            groups,
            status="broken",
            expected=expected,
            detail=first_line(completed.stderr) or f"exit {completed.returncode}",
            hint="reinstall sccache or unset RUSTC_WRAPPER",
        )
    found = prefixed("sccache")(completed.stdout or completed.stderr)
    if found is None:
        return Check("sccache", groups, status="broken", expected=expected, detail="could not read `sccache --version`", hint="reinstall sccache")
    if expected is not None and found != expected:
        return Check(
            "sccache",
            groups,
            status="version-mismatch",
            expected=expected,
            found=found,
            detail=f"expected {expected}, found {found}",
            hint="run `mise install` to install the sccache pin",
        )
    return Check("sccache", groups, status="ok", expected=expected, found=found)


def check_cross_compiler() -> list[Check]:
    checks: list[Check] = []
    completed, _ = run_command(["rustup", "target", "list", "--installed"])
    if completed is None or completed.returncode != 0:
        checks.append(
            Check(
                f"{CROSS_TARGET} target",
                ("optional",),
                status="skipped",
                detail="rustup is not available",
            )
        )
    else:
        installed = {line.strip() for line in completed.stdout.splitlines() if line.strip()}
        if CROSS_TARGET in installed:
            checks.append(Check(f"{CROSS_TARGET} target", ("optional",), status="ok", found="installed"))
        else:
            checks.append(
                Check(
                    f"{CROSS_TARGET} target",
                    ("optional",),
                    status="missing",
                    detail="Rust target not installed",
                    hint=f"rustup target add {CROSS_TARGET}",
                )
            )

    checks.append(
        check_command(
            "x86_64-linux-gnu-gcc",
            ("optional",),
            ["x86_64-linux-gnu-gcc", "--version"],
            hint="install an x86_64 Linux cross compiler (see docs/runbooks/local-gate-prerequisites.md)",
        )
    )
    return checks


def check_xctrace() -> Check:
    if sys.platform != "darwin":
        return Check("xctrace", ("optional",), status="skipped", detail="macOS only")
    completed, error = run_command(["xcrun", "--find", "xctrace"])
    if completed is None or completed.returncode != 0:
        detail = error or first_line(completed.stderr if completed else None) or "not found"
        return Check(
            "xctrace",
            ("optional",),
            status="missing",
            detail=detail,
            hint="install Xcode and select its developer directory",
        )
    return Check("xctrace", ("optional",), status="ok", found=completed.stdout.strip())


def base_checks(config: dict[str, object]) -> list[Check]:
    rust = str(config["rust"])
    mise_rust = str(config["mise_rust"])
    components = [str(component) for component in config["components"]]
    if mise_rust == rust:
        pin_check = Check("toolchain pins", ("baseline",), status="ok", found="agree")
    else:
        pin_check = Check(
            "toolchain pins",
            ("baseline",),
            status="broken",
            detail=f".mise.toml pins Rust {mise_rust}, but rust-toolchain.toml pins {rust}",
            hint="make the two pins match, then run `just rust-version-check`",
        )
    checks = [
        pin_check,
        check_command(
            "rustc",
            ("baseline",),
            ["rustc", "--version"],
            expected=rust,
            parser=prefixed("rustc"),
            hint=f"install the pinned toolchain: `mise install` (or `rustup toolchain install {rust}`)",
        ),
        check_command(
            "cargo",
            ("baseline",),
            ["cargo", "--version"],
            expected=rust,
            parser=prefixed("cargo"),
            hint=f"install the pinned toolchain: `mise install` (or `rustup toolchain install {rust}`)",
        ),
        check_component("clippy", ("baseline",)),
        check_component("rustfmt", ("baseline",)),
        check_just(str(config["just"])),
        check_command(
            "python3",
            ("baseline",),
            ["python3", "--version"],
            parser=prefixed("Python"),
            hint="install Python 3; the script suites use only the standard library",
        ),
        check_command(
            "git",
            ("baseline",),
            ["git", "--version"],
            parser=prefixed("git version"),
            hint="install Git",
        ),
        check_command(
            "cc",
            ("baseline",),
            ["cc", "--version"],
            hint="install a C compiler; on macOS install the Xcode command line tools",
        ),
    ]
    for component in components:
        if component not in ("clippy", "rustfmt"):
            checks.append(check_component(component, ("optional",)))
    return checks


def check_presence(name: str, groups: tuple[str, ...], command: str, hint: str | None = None) -> Check:
    path = shutil.which(command)
    if path is None:
        return Check(name, groups, status="missing", detail="not found", hint=hint)
    return Check(name, groups, status="ok", path=path)


def check_cargo_bloat() -> Check:
    completed, error = run_command(["cargo", "bloat", "--version"])
    if completed is None:
        return Check(
            "cargo-bloat",
            ("optional",),
            status="missing" if not command_exists("cargo") else "broken",
            detail=error,
            hint="install the binary-size auditor with `cargo install cargo-bloat`",
        )
    output = "\n".join(part for part in (completed.stdout, completed.stderr) if part)
    if completed.returncode != 0:
        detail = first_line(output) or f"exit {completed.returncode}"
        status = "missing" if "no such command" in detail.lower() else "broken"
        return Check(
            "cargo-bloat",
            ("optional",),
            status=status,
            detail=detail,
            hint="install the binary-size auditor with `cargo install cargo-bloat`",
        )
    return Check("cargo-bloat", ("optional",), status="ok", found=first_line(output))


def optional_checks(config: dict[str, object]) -> list[Check]:
    sccache_version = config.get("sccache")
    checks = [
        check_presence(
            "mise",
            ("optional",),
            "mise",
            hint="install mise from https://mise.jdx.dev or use a manual Rust and just installation",
        ),
        check_sccache(str(sccache_version) if sccache_version else None),
        check_command(
            "samply",
            ("optional",),
            ["samply", "--version"],
            parser=prefixed("samply"),
            hint="install the primary profiler with `cargo install samply --locked`",
        ),
        check_cargo_bloat(),
    ]
    checks.extend(check_cross_compiler())
    checks.append(check_xctrace())
    return checks


def required_for(check: Check, scope: str) -> bool:
    if scope == "setup":
        return True
    return any(group in REQUIRED_GROUPS for group in check.groups)


def severity_for(check: Check, scope: str) -> str:
    if check.status in ("ok", "skipped"):
        return "ok"
    if check.status == "newer":
        return "warning"
    if scope == "setup":
        return "error"
    if any(group in REQUIRED_GROUPS for group in check.groups):
        return "error"
    return "warning"


def primary_group(check: Check, scope: str) -> str:
    if scope == "setup":
        return "setup"
    for group in GROUP_ORDER:
        if group in check.groups:
            return group
    return check.groups[0]


def line_for(check: Check) -> str:
    if check.status == "ok":
        if check.name in HOOK_NAMES:
            return f"{check.name} hook installed"
        text = check.name
        if check.found:
            text += f" {shorten(check.found)}"
        elif check.path:
            text += f" {check.path}"
        return text
    if check.status == "newer":
        return f"{check.name} {check.found} (pinned {check.expected}; newer than pin)"
    if check.status == "version-mismatch":
        if check.found:
            return f"{check.name} {check.found} (pinned {check.expected})"
        return f"{check.name}: {check.detail or 'version mismatch'}"
    if check.status == "missing":
        if check.detail and check.detail not in ("not found", "not installed"):
            return f"{check.name} not found ({shorten(check.detail)})"
        return f"{check.name} not found"
    if check.status == "missing-component":
        return f"{check.name} component not installed"
    if check.status == "missing-hook":
        return f"{check.name} hook {check.detail or 'is missing'}"
    if check.status == "skipped":
        return f"{check.name}: {check.detail}"
    return f"{check.name}: {shorten(check.detail) or check.status}"


def render_json(checks: list[Check], scope: str) -> dict[str, object]:
    errors = [check for check in checks if severity_for(check, scope) == "error"]
    return {
        "ok": not errors,
        "scope": scope,
        "checks": [
            {
                "name": check.name,
                "groups": list(check.groups),
                "setup": check.setup,
                "required": required_for(check, scope),
                "status": check.status,
                "severity": severity_for(check, scope),
                "expected": check.expected,
                "found": check.found,
                "detail": check.detail,
                "hint": check.hint,
                "path": check.path,
            }
            for check in checks
        ],
    }


def render_human(checks: list[Check], scope: str, quiet: bool) -> int:
    errors = [check for check in checks if severity_for(check, scope) == "error"]
    warnings = [check for check in checks if severity_for(check, scope) == "warning"]

    if quiet:
        if not errors:
            return 0
        print("ERROR: developer setup postconditions failed:", file=sys.stderr)
        for check in errors:
            print(f"  - {line_for(check)}", file=sys.stderr)
            if check.hint:
                print(f"    {check.hint}", file=sys.stderr)
        return 1

    print("Cake developer readiness")
    print()
    groups = ("setup",) if scope == "setup" else GROUP_ORDER
    for group in groups:
        items = [check for check in checks if primary_group(check, scope) == group]
        if not items:
            continue
        print(f"{GROUP_LABELS[group]}:")
        for check in items:
            severity = severity_for(check, scope)
            marker = {"ok": "ok", "warning": "WARN", "error": "FAIL"}[severity]
            if check.status == "skipped":
                marker = "--"
            print(f"  {marker:<6} {line_for(check)}")
            if check.hint and severity != "ok":
                print(f"         {check.hint}")
        print()

    if errors:
        print(f"ERROR: {len(errors)} required check(s) failed.", file=sys.stderr)
        print("Fix the failures above; `just setup` restores the pinned tools and hooks.", file=sys.stderr)
        return 1

    print("Developer environment is ready.")
    if warnings:
        print(f"{len(warnings)} optional or non-blocking warning(s) above.")
    return 0


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="scripts/doctor.py",
        description="Read-only developer environment readiness report.",
        epilog="Exit codes: 0 ready, 1 a required check failed, 2 usage or configuration error.",
    )
    parser.add_argument("--json", action="store_true", help="print the report as JSON")
    parser.add_argument("--scope", choices=("all", "setup"), default="all", help="check the whole environment or only setup postconditions")
    parser.add_argument("--quiet", action="store_true", help="print only setup failures (used by `just setup`)")
    args = parser.parse_args(argv)

    try:
        config = read_config()
    except ConfigError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2

    checks = check_pins() + check_hooks()
    if args.scope == "all":
        checks = base_checks(config) + checks + optional_checks(config)
    checks = [check for check in checks if args.scope == "all" or check.setup]

    if args.json:
        print(json.dumps(render_json(checks, args.scope), indent=2))
        return 0 if not [check for check in checks if severity_for(check, args.scope) == "error"] else 1

    return render_human(checks, args.scope, args.quiet)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

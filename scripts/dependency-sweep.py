#!/usr/bin/env python3
"""Report Cake's dependency and tooling version surfaces.

The report is intentionally read-only. It extracts values from the files that
already own them and can compare the project Rust pin with a saved official
stable-channel manifest.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
from datetime import date
from pathlib import Path
from typing import Any

EXIT_CODES = {"no-work": 0, "actionable": 10, "review-required": 20}
SCHEMA_VERSION = 1
COOLDOWN_DAYS = 7


def item(
    identifier: str,
    domain: str,
    owner: str,
    authority: str,
    path: str,
    name: str,
    current: str,
    **extra: Any,
) -> dict[str, Any]:
    result: dict[str, Any] = {
        "id": identifier,
        "domain": domain,
        "owner": owner,
        "authority": authority,
        "path": path,
        "name": name,
        "current": current,
    }
    result.update(extra)
    return result


def finding(
    code: str,
    classification: str,
    domain: str,
    reason: str,
    **extra: Any,
) -> dict[str, Any]:
    result: dict[str, Any] = {
        "code": code,
        "classification": classification,
        "domain": domain,
        "reason": reason,
    }
    result.update(extra)
    return result


def relative(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


def load_toml(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    try:
        with path.open("rb") as stream:
            return tomllib.load(stream), None
    except FileNotFoundError:
        return None, f"{path} is missing"
    except (OSError, tomllib.TOMLDecodeError) as error:
        return None, f"could not parse {path}: {error}"


def version_from_spec(spec: Any) -> str:
    if isinstance(spec, str):
        return spec
    if isinstance(spec, dict):
        version = spec.get("version")
        return version if isinstance(version, str) else "unversioned"
    return "unversioned"


def normalized_tool_name(name: str) -> str:
    return name.removeprefix("cargo-")


def is_exact_version(value: str) -> bool:
    return bool(re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?", value))


def collect_cargo_inventory(root: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    inventory: list[dict[str, Any]] = []
    findings: list[dict[str, Any]] = []
    manifest_path = root / "Cargo.toml"
    manifest, error = load_toml(manifest_path)
    manifest_name = relative(root, manifest_path)
    if error:
        findings.append(finding("cargo-manifest-unavailable", "review-required", "cargo", error))
        return inventory, findings

    package = manifest.get("package", {})
    if isinstance(package, dict) and isinstance(package.get("version"), str):
        inventory.append(
            item(
                "cargo:package",
                "release",
                "human decision and review",
                manifest_name,
                manifest_name,
                "Cake package",
                package["version"],
            )
        )

    dependency_tables: list[tuple[str, dict[str, Any]]] = []
    for table_name in ("dependencies", "dev-dependencies"):
        table = manifest.get(table_name)
        if isinstance(table, dict):
            dependency_tables.append((table_name, table))
    target_tables = manifest.get("target")
    if isinstance(target_tables, dict):
        for target_name, target_config in target_tables.items():
            if not isinstance(target_config, dict):
                continue
            dependencies = target_config.get("dependencies")
            if isinstance(dependencies, dict):
                dependency_tables.append((f"target.{target_name}.dependencies", dependencies))

    for table_name, table in dependency_tables:
        for dependency_name in sorted(table):
            inventory.append(
                item(
                    f"cargo:{table_name}:{dependency_name}",
                    "cargo",
                    "Dependabot",
                    manifest_name,
                    manifest_name,
                    dependency_name,
                    version_from_spec(table[dependency_name]),
                    requirement_table=table_name,
                )
            )

    lock_path = root / "Cargo.lock"
    lock, error = load_toml(lock_path)
    lock_name = relative(root, lock_path)
    if error:
        findings.append(finding("cargo-lock-unavailable", "review-required", "cargo", error))
    else:
        packages = lock.get("package", [])
        package_count = len(packages) if isinstance(packages, list) else 0
        inventory.append(
            item(
                "cargo:lockfile",
                "cargo",
                "Dependabot",
                lock_name,
                lock_name,
                "resolved package selections",
                f"{package_count} package selections",
                package_count=package_count,
            )
        )
    return inventory, findings


def collect_mise_inventory(root: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    inventory: list[dict[str, Any]] = []
    findings: list[dict[str, Any]] = []
    path = root / ".mise.toml"
    data, error = load_toml(path)
    name = relative(root, path)
    if error:
        findings.append(finding("mise-unavailable", "review-required", "development-tools", error))
        return inventory, findings

    tools = data.get("tools")
    if not isinstance(tools, dict):
        findings.append(
            finding("mise-tools-unavailable", "review-required", "development-tools", f"{name} has no [tools] table")
        )
        return inventory, findings

    for tool_name in sorted(tools):
        value = tools[tool_name]
        version = value.get("version") if isinstance(value, dict) else value
        current = version if isinstance(version, str) else "unversioned"
        authority = "rust-toolchain.toml" if tool_name == "rust" else name
        extra: dict[str, Any] = {}
        if tool_name == "rust":
            extra["relation"] = "project-toolchain"
        inventory.append(
            item(
                f"mise:{tool_name}",
                "rust" if tool_name == "rust" else "development-tools",
                "dependency sweep",
                authority,
                name,
                tool_name,
                current,
                tool_name=normalized_tool_name(tool_name),
                **extra,
            )
        )
        if not is_exact_version(current):
            findings.append(
                finding(
                    "mutable-tool-pin",
                    "review-required",
                    "development-tools",
                    f"{name} does not contain an exact version for {tool_name}",
                    path=name,
                    name=tool_name,
                    current=current,
                )
            )
    return inventory, findings


def cargo_install_value(line: str) -> tuple[str, str, bool] | None:
    match = re.search(r"\bcargo\s+install\s+([A-Za-z0-9][A-Za-z0-9_-]*)(.*)$", line)
    if not match:
        return None
    tool_name, arguments = match.groups()
    version_match = re.search(r"(?:--version\s+|--version=)([^\s#]+)", arguments)
    version = version_match.group(1) if version_match else "unversioned"
    return tool_name, version, "--locked" in arguments.split("#", 1)[0].split()


def collect_text_tool_inventory(root: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    inventory: list[dict[str, Any]] = []
    findings: list[dict[str, Any]] = []
    paths = [root / "justfile", *sorted((root / ".github" / "workflows").glob("*.yml"))]
    for path in paths:
        name = relative(root, path)
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except FileNotFoundError:
            findings.append(finding("tool-surface-unavailable", "review-required", "development-tools", f"{name} is missing"))
            continue
        except OSError as error:
            findings.append(
                finding("tool-surface-unavailable", "review-required", "development-tools", f"could not read {name}: {error}")
            )
            continue

        job = "unknown"
        for line_number, line in enumerate(lines, start=1):
            job_match = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
            if job_match:
                job = job_match.group(1)

            install = cargo_install_value(line)
            if install:
                tool_name, version, locked = install
                identifier = f"cargo-install:{name}:{line_number}"
                inventory.append(
                    item(
                        identifier,
                        "development-tools",
                        "dependency sweep",
                        name,
                        name,
                        tool_name,
                        version,
                        tool_name=normalized_tool_name(tool_name),
                        installation="cargo install",
                        locked=locked,
                        line=line_number,
                    )
                )
                if not is_exact_version(version) or not locked:
                    missing = "an exact version" if not is_exact_version(version) else "--locked"
                    findings.append(
                        finding(
                            "unpinned-tool-install",
                            "review-required",
                            "development-tools",
                            f"{name}:{line_number} cargo install is missing {missing}",
                            path=name,
                            line=line_number,
                            name=tool_name,
                        )
                    )

            tool_match = re.match(r"^\s*tool:\s*([^\s#]+)", line)
            if tool_match:
                value = tool_match.group(1)
                if "@" in value:
                    tool_name, version = value.rsplit("@", 1)
                else:
                    tool_name, version = value, "unversioned"
                inventory.append(
                    item(
                        f"workflow-tool:{name}:{line_number}",
                        "development-tools",
                        "dependency sweep",
                        name,
                        name,
                        tool_name,
                        version,
                        tool_name=normalized_tool_name(tool_name),
                        installation="taiki-e/install-action",
                        line=line_number,
                    )
                )
                if not is_exact_version(version):
                    findings.append(
                        finding(
                            "unpinned-tool-install",
                            "review-required",
                            "development-tools",
                            f"{name}:{line_number} action tool is missing an exact version",
                            path=name,
                            line=line_number,
                            name=tool_name,
                        )
                    )

            toolchain_match = re.match(r"^\s*toolchain:\s*([^\s#]+)", line)
            if toolchain_match:
                version = toolchain_match.group(1)
                is_msrv = name == ".github/workflows/scheduled.yml" and job == "msrv"
                inventory.append(
                    item(
                        f"rust-workflow:{name}:{line_number}",
                        "rust",
                        "human decision and review" if is_msrv else "dependency sweep",
                        ".github/workflows/scheduled.yml" if is_msrv else "rust-toolchain.toml",
                        name,
                        f"{job} toolchain",
                        version,
                        relation="msrv" if is_msrv else "project-toolchain",
                        line=line_number,
                    )
                )

            uses_match = re.match(r"^\s*(?:-\s*)?uses:\s*([^\s#]+)", line)
            if uses_match:
                action = uses_match.group(1)
                if "@" in action:
                    action_name, ref = action.rsplit("@", 1)
                else:
                    action_name, ref = action, "unversioned"
                is_udeps_nightly = (
                    name == ".github/workflows/scheduled.yml"
                    and job == "udeps"
                    and action_name == "dtolnay/rust-toolchain"
                    and ref == "nightly"
                )
                action_extra: dict[str, Any] = {
                    "line": line_number,
                    "update_path": "manual compatibility review" if is_udeps_nightly else "Dependabot pull request",
                }
                if is_udeps_nightly:
                    action_extra["exception"] = "cargo-udeps requires a compatible nightly"
                inventory.append(
                    item(
                        f"action:{name}:{line_number}",
                        "github-actions",
                        "human decision and review" if is_udeps_nightly else "Dependabot",
                        ".github/workflows/scheduled.yml" if is_udeps_nightly else name,
                        name,
                        action_name,
                        ref,
                        **action_extra,
                    )
                )
    return inventory, findings


def collect_inventory(root: Path) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    inventory: list[dict[str, Any]] = []
    findings: list[dict[str, Any]] = []
    for collector in (collect_cargo_inventory, collect_mise_inventory, collect_text_tool_inventory):
        collected, collected_findings = collector(root)
        inventory.extend(collected)
        findings.extend(collected_findings)
    inventory.sort(key=lambda record: record["id"])
    return inventory, findings


def parse_rust_version(value: str) -> tuple[int, int, int] | None:
    match = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", value)
    if not match:
        return None
    return tuple(int(part) for part in match.groups())  # type: ignore[return-value]


def project_rust_version(inventory: list[dict[str, Any]]) -> str | None:
    for record in inventory:
        if record["id"] == "rust-toolchain":
            return record["current"]
    return None


def collect_rust_toolchain(root: Path, inventory: list[dict[str, Any]], findings: list[dict[str, Any]]) -> None:
    path = root / "rust-toolchain.toml"
    name = relative(root, path)
    data, error = load_toml(path)
    if error:
        findings.append(finding("rust-toolchain-unavailable", "review-required", "rust", error))
        return
    toolchain = data.get("toolchain")
    version = toolchain.get("channel") if isinstance(toolchain, dict) else None
    if not isinstance(version, str) or not version:
        findings.append(finding("rust-toolchain-unavailable", "review-required", "rust", f"{name} has no channel"))
        return
    inventory.append(
        item("rust-toolchain", "rust", "dependency sweep", name, name, "project toolchain", version)
    )


def compare_rust_pins(inventory: list[dict[str, Any]], findings: list[dict[str, Any]]) -> None:
    project = project_rust_version(inventory)
    if project is None:
        return
    for record in inventory:
        if record.get("relation") != "project-toolchain":
            continue
        if record["current"] != project:
            findings.append(
                finding(
                    "rust-pin-drift",
                    "review-required",
                    "rust",
                    f"{record['path']} pins Rust {record['current']} but rust-toolchain.toml pins {project}",
                    path=record["path"],
                    current=record["current"],
                    expected=project,
                )
            )


def parse_rust_release(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    data, error = load_toml(path)
    if error:
        return None, error
    published = data.get("date")
    rust = data.get("pkg", {}).get("rust", {}) if isinstance(data.get("pkg"), dict) else {}
    raw_version = rust.get("version") if isinstance(rust, dict) else None
    if not isinstance(published, str) or not isinstance(raw_version, str):
        return None, f"{path} is missing date or [pkg.rust].version"
    try:
        published_date = date.fromisoformat(published)
    except ValueError:
        return None, f"{path} has an invalid release date: {published}"
    match = re.match(r"(\d+\.\d+\.\d+)(?:\s|$)", raw_version)
    if not match or parse_rust_version(match.group(1)) is None:
        return None, f"{path} has an ambiguous Rust version: {raw_version}"
    return {"version": match.group(1), "published": published, "published_date": published_date}, None


def evaluate_rust_release(
    inventory: list[dict[str, Any]],
    findings: list[dict[str, Any]],
    release_path: Path | None,
    as_of: date | None,
    security_reason: str | None,
) -> dict[str, Any] | None:
    project = project_rust_version(inventory)
    if project is None:
        return None
    if release_path is None:
        findings.append(
            finding(
                "rust-release-unavailable",
                "review-required",
                "rust",
                "official Rust stable-channel data was not supplied; do not guess a candidate",
                current=project,
            )
        )
        return None
    release, error = parse_rust_release(release_path)
    if error or release is None:
        findings.append(
            finding(
                "rust-release-invalid",
                "review-required",
                "rust",
                error or f"could not read {release_path}",
                source=str(release_path),
            )
        )
        return None
    result = {
        "source": str(release_path),
        "version": release["version"],
        "published": release["published"],
    }
    if as_of is None:
        findings.append(
            finding(
                "rust-release-date-missing",
                "review-required",
                "rust",
                "--as-of is required to apply the seven-day cooldown",
                candidate=release["version"],
                published=release["published"],
            )
        )
        return result

    age = (as_of - release["published_date"]).days
    result["as_of"] = as_of.isoformat()
    result["age_days"] = age
    current_tuple = parse_rust_version(project)
    candidate_tuple = parse_rust_version(release["version"])
    if age < 0:
        findings.append(
            finding(
                "rust-release-date-ambiguous",
                "review-required",
                "rust",
                "the release date is later than --as-of; do not guess release age",
                current=project,
                candidate=release["version"],
                published=release["published"],
                as_of=as_of.isoformat(),
            )
        )
    elif current_tuple is None or candidate_tuple is None:
        findings.append(
            finding(
                "rust-version-ambiguous",
                "review-required",
                "rust",
                "the project or stable-channel Rust version is not an exact semantic version",
                current=project,
                candidate=release["version"],
            )
        )
    elif candidate_tuple < current_tuple:
        findings.append(
            finding(
                "rust-release-regressed",
                "review-required",
                "rust",
                "the official stable channel is older than the repository pin",
                current=project,
                candidate=release["version"],
            )
        )
    elif candidate_tuple > current_tuple:
        update = {
            "current": project,
            "candidate": release["version"],
            "published": release["published"],
            "age_days": age,
            "authority": "rust-toolchain.toml",
        }
        if candidate_tuple[0] != current_tuple[0]:
            findings.append(
                finding(
                    "rust-major-update",
                    "review-required",
                    "rust",
                    "major-version updates require individual human review",
                    **update,
                )
            )
        elif age < COOLDOWN_DAYS and not security_reason:
            findings.append(
                finding(
                    "rust-release-cooldown",
                    "review-required",
                    "rust",
                    f"the release is {age} days old; wait {COOLDOWN_DAYS} days unless it is a documented security update",
                    **update,
                )
            )
        else:
            reason = "security update; cooldown exemption recorded" if security_reason else "release passed the seven-day cooldown"
            if security_reason:
                update["security_reason"] = security_reason
            findings.append(finding("rust-toolchain-update", "actionable", "rust", reason, **update))
    return result


def compare_tool_versions(inventory: list[dict[str, Any]], findings: list[dict[str, Any]]) -> None:
    versions: dict[str, list[dict[str, Any]]] = {}
    for record in inventory:
        tool_name = record.get("tool_name")
        version = record["current"]
        if not isinstance(tool_name, str) or version == "unversioned":
            continue
        versions.setdefault(tool_name, []).append(record)
    for tool_name, records in sorted(versions.items()):
        distinct = sorted({record["current"] for record in records})
        if len(distinct) > 1:
            locations = ", ".join(f"{record['path']}:{record.get('line', '?')}" for record in records)
            findings.append(
                finding(
                    "tool-version-drift",
                    "review-required",
                    "development-tools",
                    f"{tool_name} has different pins ({', '.join(distinct)}) at {locations}",
                    name=tool_name,
                    versions=distinct,
                )
            )


def build_report(args: argparse.Namespace) -> dict[str, Any]:
    root = args.root.resolve()
    inventory, findings = collect_inventory(root)
    collect_rust_toolchain(root, inventory, findings)
    inventory.sort(key=lambda record: record["id"])
    compare_rust_pins(inventory, findings)
    compare_tool_versions(inventory, findings)
    rust_release = evaluate_rust_release(
        inventory,
        findings,
        args.rust_channel.resolve() if args.rust_channel else None,
        args.as_of,
        args.security_reason,
    )

    if any(record["classification"] == "review-required" for record in findings):
        status = "review-required"
    elif any(record["classification"] == "actionable" for record in findings):
        status = "actionable"
    else:
        status = "no-work"
    return {
        "schema_version": SCHEMA_VERSION,
        "status": status,
        "exit_code": EXIT_CODES[status],
        "repository": "cake",
        "inventory": inventory,
        "findings": findings,
        "rust_release": rust_release,
        "summary": {
            "inventory_count": len(inventory),
            "finding_count": len(findings),
            "actionable_count": sum(record["classification"] == "actionable" for record in findings),
            "review_required_count": sum(record["classification"] == "review-required" for record in findings),
        },
    }


def parse_date(value: str) -> date:
    try:
        return date.fromisoformat(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"invalid date {value!r}; expected YYYY-MM-DD") from error


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent, help=argparse.SUPPRESS)
    parser.add_argument("--rust-channel", type=Path, help="saved official channel-rust-stable.toml")
    parser.add_argument("--as-of", type=parse_date, help="UTC date used for the release cooldown")
    parser.add_argument(
        "--security-reason",
        help="documented reason to exempt a Rust security update from the cooldown",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    report = build_report(args)
    print(json.dumps(report, indent=2, sort_keys=False))
    return report["exit_code"]


if __name__ == "__main__":
    raise SystemExit(main())

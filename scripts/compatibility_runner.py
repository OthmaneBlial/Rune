#!/usr/bin/env python3
"""Run bounded Rune compatibility scenarios without claiming a-Shell parity.

The runner executes each scenario in a fresh temporary Rune filesystem. It can
also compare those results with reference observations captured separately by
an Apple/a-Shell harness. It never starts a host shell and never executes the
reference checkout itself.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


MAX_COMMAND_BYTES = 64 * 1024
MAX_STDIN_BYTES = 1024 * 1024
MAX_SCENARIOS = 128
MAX_RUN_SECONDS = 10
SCHEMA_VERSION = 1
ALLOWED_NORMALIZATIONS = {"crlf", "trailing-newline", "sandbox-root"}
SCENARIO_ID = re.compile(r"^[a-z0-9][a-z0-9/_-]*$")


class ScenarioError(ValueError):
    """A user-facing scenario definition or comparison error."""


def require_string(value: Any, label: str, maximum: int | None = None) -> str:
    if not isinstance(value, str) or not value:
        raise ScenarioError(f"{label} must be a non-empty string")
    if maximum is not None and len(value.encode("utf-8")) > maximum:
        raise ScenarioError(f"{label} exceeds {maximum} UTF-8 bytes")
    return value


def require_text(value: Any, label: str, maximum: int) -> str:
    if not isinstance(value, str):
        raise ScenarioError(f"{label} must be a string")
    if len(value.encode("utf-8")) > maximum:
        raise ScenarioError(f"{label} exceeds {maximum} UTF-8 bytes")
    return value


def validate_normalizations(value: Any, label: str) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ScenarioError(f"{label} must be a list of strings")
    if len(value) != len(set(value)):
        raise ScenarioError(f"{label} contains duplicate rules")
    unknown = set(value) - ALLOWED_NORMALIZATIONS
    if unknown:
        raise ScenarioError(f"{label} contains unsupported rules: {', '.join(sorted(unknown))}")
    return value


def load_scenarios(path: Path) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ScenarioError(f"cannot read {path}: {error}") from error
    if not isinstance(document, dict):
        raise ScenarioError("scenario document must be an object")
    if document.get("schema_version") != SCHEMA_VERSION:
        raise ScenarioError(f"schema_version must be {SCHEMA_VERSION}")

    reference = document.get("reference")
    if not isinstance(reference, dict):
        raise ScenarioError("reference must be an object")
    if reference.get("name") != "a-Shell":
        raise ScenarioError("reference.name must remain a-Shell")
    if reference.get("path") != "base/a-shell":
        raise ScenarioError("reference.path must remain base/a-shell")
    if reference.get("comparison") != "pending":
        raise ScenarioError("reference.comparison must remain pending until direct evidence exists")

    default_rules = validate_normalizations(
        document.get("normalization_rules"), "normalization_rules"
    )
    scenarios = document.get("scenarios")
    if not isinstance(scenarios, list) or not scenarios:
        raise ScenarioError("scenarios must be a non-empty list")
    if len(scenarios) > MAX_SCENARIOS:
        raise ScenarioError(f"scenarios exceeds the {MAX_SCENARIOS}-scenario bound")

    seen: set[str] = set()
    validated: list[dict[str, Any]] = []
    for index, scenario in enumerate(scenarios):
        label = f"scenarios[{index}]"
        if not isinstance(scenario, dict):
            raise ScenarioError(f"{label} must be an object")
        scenario_id = require_string(scenario.get("id"), f"{label}.id")
        if not SCENARIO_ID.fullmatch(scenario_id):
            raise ScenarioError(f"{label}.id contains unsupported characters")
        if scenario_id in seen:
            raise ScenarioError(f"duplicate scenario id: {scenario_id}")
        seen.add(scenario_id)
        command = require_string(
            scenario.get("command"), f"{label}.command", MAX_COMMAND_BYTES
        )
        stdin = scenario.get("stdin", "")
        if not isinstance(stdin, str):
            raise ScenarioError(f"{label}.stdin must be a string")
        if len(stdin.encode("utf-8")) > MAX_STDIN_BYTES:
            raise ScenarioError(f"{label}.stdin exceeds {MAX_STDIN_BYTES} UTF-8 bytes")
        rules = validate_normalizations(
            scenario.get("normalization", default_rules), f"{label}.normalization"
        )
        validated.append(
            {
                "id": scenario_id,
                "command": command,
                "stdin": stdin,
                "normalization": rules,
            }
        )
    return document, validated


def normalize_text(value: str, rules: list[str], sandbox_root: Path) -> str:
    normalized = value
    if "crlf" in rules:
        normalized = normalized.replace("\r\n", "\n").replace("\r", "\n")
    if "sandbox-root" in rules:
        normalized = normalized.replace(str(sandbox_root), "<SANDBOX_ROOT>")
    if "trailing-newline" in rules:
        normalized = normalized.rstrip("\n")
    return normalized


def decode_output(value: bytes, stream: str) -> str:
    try:
        return value.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ScenarioError(f"Rune emitted non-UTF-8 {stream}: {error}") from error


def run_rune(binary: Path, scenario: dict[str, Any], sandbox_root: Path) -> dict[str, Any]:
    try:
        completed = subprocess.run(
            [str(binary), "--root", str(sandbox_root), "--command", scenario["command"]],
            input=scenario["stdin"].encode("utf-8"),
            capture_output=True,
            check=False,
            timeout=MAX_RUN_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ScenarioError(f"Rune scenario {scenario['id']} could not finish: {error}") from error
    if completed.returncode < 0:
        raise ScenarioError(
            f"Rune scenario {scenario['id']} terminated by signal {-completed.returncode}"
        )
    return {
        "scenario_id": scenario["id"],
        "runner": "Rune",
        "status": completed.returncode,
        "stdout": decode_output(completed.stdout, "stdout"),
        "stderr": decode_output(completed.stderr, "stderr"),
    }


def observation_path(reference_dir: Path, scenario_id: str) -> Path:
    return reference_dir / f"{scenario_id.replace('/', '__')}.json"


def load_reference_observation(path: Path, scenario_id: str) -> dict[str, Any]:
    try:
        observation = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ScenarioError(f"cannot read reference observation {path}: {error}") from error
    if not isinstance(observation, dict) or observation.get("scenario_id") != scenario_id:
        raise ScenarioError(f"reference observation has the wrong scenario id: {path}")
    if observation.get("runner") != "a-Shell":
        raise ScenarioError(f"reference observation must identify a-Shell: {path}")
    status = observation.get("status")
    if not isinstance(status, int) or not 0 <= status <= 255:
        raise ScenarioError(f"reference observation status is invalid: {path}")
    require_text(observation.get("stdout", ""), f"{path}.stdout", MAX_STDIN_BYTES)
    require_text(observation.get("stderr", ""), f"{path}.stderr", MAX_STDIN_BYTES)
    return observation


def compare_observation(
    rune: dict[str, Any],
    reference: dict[str, Any],
    rules: list[str],
    sandbox_root: Path,
) -> list[str]:
    differences: list[str] = []
    if rune["status"] != reference["status"]:
        differences.append(f"status: Rune={rune['status']} a-Shell={reference['status']}")
    for stream in ("stdout", "stderr"):
        rune_value = normalize_text(rune[stream], rules, sandbox_root)
        reference_value = normalize_text(reference[stream], rules, sandbox_root)
        if rune_value != reference_value:
            differences.append(f"{stream}: normalized values differ")
    return differences


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--scenario-file",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "compat/scenarios/core.json",
    )
    parser.add_argument(
        "--rune-binary",
        type=Path,
        default=Path(__file__).resolve().parents[1] / "target/debug/rune-cli",
    )
    parser.add_argument(
        "--reference-dir",
        type=Path,
        help="directory of separately captured a-Shell JSON observations",
    )
    parser.add_argument(
        "--validate-only",
        action="store_true",
        help="validate the versioned scenario document without executing Rune",
    )
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    document, scenarios = load_scenarios(arguments.scenario_file.resolve())
    if arguments.validate_only:
        print(
            f"Compatibility scenarios valid: {len(scenarios)}; "
            "direct a-Shell comparison remains pending."
        )
        return 0
    if not arguments.rune_binary.is_file():
        raise ScenarioError(
            f"Rune binary is missing: {arguments.rune_binary}; run cargo build -p rune-cli first"
        )
    if arguments.reference_dir is not None and not arguments.reference_dir.is_dir():
        raise ScenarioError(f"reference observation directory is missing: {arguments.reference_dir}")

    results: list[dict[str, Any]] = []
    comparison_failures = 0
    for scenario in scenarios:
        with tempfile.TemporaryDirectory(prefix="rune-compat-") as temporary_root:
            sandbox_root = Path(temporary_root)
            rune = run_rune(arguments.rune_binary.resolve(), scenario, sandbox_root)
            result: dict[str, Any] = {"rune": rune, "comparison": "pending"}
            if arguments.reference_dir is not None:
                path = observation_path(arguments.reference_dir.resolve(), scenario["id"])
                reference = load_reference_observation(path, scenario["id"])
                differences = compare_observation(
                    rune, reference, scenario["normalization"], sandbox_root
                )
                result["comparison"] = "verified" if not differences else "mismatch"
                if differences:
                    comparison_failures += 1
                    result["differences"] = differences
            results.append(result)

    print(json.dumps({"schema_version": 1, "source": document["reference"], "results": results}, indent=2))
    if comparison_failures:
        print(
            f"compatibility comparison found {comparison_failures} mismatch(es)",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ScenarioError as error:
        print(f"compatibility scenario error: {error}", file=sys.stderr)
        raise SystemExit(2) from error

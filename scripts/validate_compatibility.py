#!/usr/bin/env python3
"""Validate Rune's machine-readable compatibility evidence boundary.

This is a repository-quality check, not a compatibility test runner. It makes
the matrix conservative by requiring explicit evidence paths and by rejecting
unsupported claims that are not backed by a verified reference comparison.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


EXPECTED_SCHEMA_VERSION = 2
STATUSES = {
    "supported",
    "mostly-supported",
    "partial",
    "planned",
    "unsupported",
    "intentionally-different",
}
COMPARISON_STATES = {"verified", "pending", "not-started"}
REQUIRED_AREA_FIELDS = {"status", "rune_evidence", "reference_comparison"}


class MatrixError(ValueError):
    """A user-facing compatibility matrix validation error."""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="Rune repository root (defaults to the parent of scripts/)",
    )
    return parser.parse_args()


def require_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise MatrixError(f"{label} must be a non-empty string")
    return value


def validate_reference(root: Path, reference: Any) -> None:
    if not isinstance(reference, dict):
        raise MatrixError("reference must be an object")
    if reference.get("usage") != "behavioral-reference-only":
        raise MatrixError("reference.usage must remain behavioral-reference-only")
    reference_path = require_string(reference.get("path"), "reference.path")
    if Path(reference_path).is_absolute() or ".." in Path(reference_path).parts:
        raise MatrixError("reference.path must be a relative repository path")
    if reference_path != "base/a-shell":
        raise MatrixError("reference.path must remain base/a-shell")
    checkout = root / reference_path
    if not checkout.is_dir():
        raise MatrixError(f"reference checkout is missing: {reference_path}")
    try:
        ignored = subprocess.run(
            ["git", "check-ignore", "--quiet", "--", reference_path],
            cwd=root,
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except OSError as error:
        raise MatrixError(f"could not verify Git ignore policy: {error}") from error
    if ignored.returncode != 0:
        raise MatrixError("base/a-shell is not ignored by Git")


def validate_evidence(root: Path, area: str, evidence: Any) -> list[str]:
    if not isinstance(evidence, list) or not all(isinstance(item, str) for item in evidence):
        raise MatrixError(f"areas.{area}.rune_evidence must be a list of strings")
    if len(evidence) != len(set(evidence)):
        raise MatrixError(f"areas.{area}.rune_evidence contains duplicates")
    for item in evidence:
        path = Path(item)
        if path.is_absolute() or ".." in path.parts or item.startswith("base/"):
            raise MatrixError(f"areas.{area}.rune_evidence contains unsafe path: {item}")
        if not (root / path).is_file():
            raise MatrixError(f"areas.{area}.rune_evidence is missing: {item}")
    return evidence


def validate_matrix(root: Path, matrix_path: Path) -> int:
    try:
        matrix = json.loads(matrix_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise MatrixError(f"cannot read {matrix_path}: {error}") from error
    if not isinstance(matrix, dict):
        raise MatrixError("matrix root must be an object")
    if matrix.get("schema_version") != EXPECTED_SCHEMA_VERSION:
        raise MatrixError(f"schema_version must be {EXPECTED_SCHEMA_VERSION}")

    validate_reference(root, matrix.get("reference"))
    statuses = matrix.get("statuses")
    if (
        not isinstance(statuses, list)
        or len(statuses) != len(STATUSES)
        or set(statuses) != STATUSES
    ):
        raise MatrixError("statuses must enumerate the complete allowed status set")
    comparisons = matrix.get("comparison_states")
    if comparisons != ["verified", "pending", "not-started"]:
        raise MatrixError("comparison_states must enumerate the complete allowed set")
    require_string(matrix.get("policy"), "policy")
    reviewed = require_string(matrix.get("last_reviewed"), "last_reviewed")
    try:
        dt.date.fromisoformat(reviewed)
    except ValueError as error:
        raise MatrixError("last_reviewed must be an ISO-8601 date") from error

    areas = matrix.get("areas")
    if not isinstance(areas, dict) or not areas:
        raise MatrixError("areas must be a non-empty object")
    for area, value in areas.items():
        if not isinstance(area, str) or not area:
            raise MatrixError("area names must be non-empty strings")
        if not isinstance(value, dict):
            raise MatrixError(f"areas.{area} must be an object")
        missing = REQUIRED_AREA_FIELDS - value.keys()
        if missing:
            raise MatrixError(f"areas.{area} is missing: {', '.join(sorted(missing))}")
        status = require_string(value["status"], f"areas.{area}.status")
        if status not in STATUSES:
            raise MatrixError(f"areas.{area}.status is not allowed: {status}")
        comparison = require_string(
            value["reference_comparison"], f"areas.{area}.reference_comparison"
        )
        if comparison not in COMPARISON_STATES:
            raise MatrixError(f"areas.{area}.reference_comparison is not allowed: {comparison}")
        evidence = validate_evidence(root, area, value["rune_evidence"])
        if status == "supported" and comparison != "verified":
            raise MatrixError(
                f"areas.{area} claims supported without a verified reference comparison"
            )
        if status == "supported" and not evidence:
            raise MatrixError(f"areas.{area} claims supported without Rune evidence")
        if comparison == "verified" and not evidence:
            raise MatrixError(f"areas.{area} is reference-verified without Rune evidence")

    return len(areas)


def main() -> int:
    arguments = parse_args()
    root = arguments.root.resolve()
    count = validate_matrix(root, root / "compat/a-shell-compatibility.json")
    print(f"Compatibility matrix valid: {count} areas; no unsupported parity claims.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except MatrixError as error:
        print(f"compatibility validation failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error

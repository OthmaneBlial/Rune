#!/usr/bin/env python3
"""Small local tests for the compatibility scenario boundary."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/compatibility_runner.py"
SPEC = importlib.util.spec_from_file_location("compatibility_runner", MODULE_PATH)
assert SPEC and SPEC.loader
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)


class CompatibilityRunnerTests(unittest.TestCase):
    def test_repository_scenarios_are_bounded_and_unique(self) -> None:
        document, scenarios = runner.load_scenarios(ROOT / "compat/scenarios/core.json")
        self.assertEqual(document["reference"]["comparison"], "pending")
        self.assertEqual(len(scenarios), len({item["id"] for item in scenarios}))
        self.assertTrue(all(item["command"] for item in scenarios))

    def test_normalization_is_explicit_and_ordered(self) -> None:
        with tempfile.TemporaryDirectory(prefix="rune-compat-test-") as root:
            value = f"line\r\n{root}/inside\n\n"
            normalized = runner.normalize_text(
                value,
                ["crlf", "sandbox-root", "trailing-newline"],
                Path(root),
            )
        self.assertEqual(normalized, "line\n<SANDBOX_ROOT>/inside")

    def test_reference_observation_requires_a_shell_identity(self) -> None:
        with tempfile.TemporaryDirectory(prefix="rune-compat-test-") as root:
            path = Path(root) / "scenario.json"
            path.write_text(
                json.dumps(
                    {
                        "scenario_id": "shell/example",
                        "runner": "other",
                        "status": 0,
                        "stdout": "",
                        "stderr": "",
                    }
                ),
                encoding="utf-8",
            )
            with self.assertRaises(runner.ScenarioError):
                runner.load_reference_observation(path, "shell/example")


if __name__ == "__main__":
    unittest.main()

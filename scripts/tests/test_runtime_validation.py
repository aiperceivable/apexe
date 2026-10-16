"""Regression tests for runtime validation failures, coverage, and cleanup."""

import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
from urllib.request import getproxies_environment, proxy_bypass_environment

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from runtime_validation import (
    RuntimeValidation,
    coverage_gaps,
    interface_usage,
    require,
    stop_process_group,
)
from validate_examples import (
    EXAMPLE_ENTRYPOINTS,
    check_acl_refusal,
    check_example_coverage,
    check_example_inventory,
)


def argument(name, values=(), index=None, last=False, global_option=False):
    return {
        "id": name.removeprefix("--"),
        "long": name if name.startswith("--") else None,
        "short": None,
        "global": global_option,
        "takes_value": bool(values),
        "values": list(values),
        "index": index,
        "last": last,
    }


def manifest():
    return {
        "commands": [
            {
                "name": "root",
                "arguments": [
                    argument("--error-format", ["text", "json"], global_option=True)
                ],
            },
            {
                "name": "scan",
                "arguments": [
                    argument("tools", index=1),
                    argument("--format", ["json", "table"]),
                ],
            },
        ]
    }


def covered_cases():
    return [
        {
            "name": "scan " + value,
            "argv": ["--error-format", error, "scan", "toy", "--format", value],
            "passed": True,
            "kind": "behavior",
        }
        for value, error in [("json", "json"), ("table", "text")]
    ]


class CoverageTests(unittest.TestCase):
    def test_isolated_runtime_routes_loopback_fixtures_directly(self):
        with tempfile.TemporaryDirectory() as directory:
            with RuntimeValidation(sys.executable, Path(directory) / 'report.json') as session:
                with patch.dict(os.environ, session.environment, clear=True):
                    exemptions = getproxies_environment()
                    for host in ['127.0.0.1:8123', 'localhost:8123', '::1']:
                        self.assertTrue(proxy_bypass_environment(host, exemptions), host)
                    self.assertFalse(proxy_bypass_environment('example.com', exemptions))

    def test_example_acl_refusal_cannot_be_satisfied_by_a_schema_error(self):
        check_acl_refusal(
            {
                "result": {
                    "isError": True,
                    "content": [{"text": "ACL_DENIED: Access denied"}],
                }
            }
        )
        with self.assertRaises(RuntimeError):
            check_acl_refusal(
                {
                    "result": {
                        "isError": True,
                        "content": [{"text": "SCHEMA_VALIDATION_ERROR: Invalid input"}],
                    }
                }
            )

    def test_new_parameter_is_a_missing_runtime_contract(self):
        data = manifest()
        data["commands"][1]["arguments"].append(argument("--future"))
        self.assertEqual(
            coverage_gaps(data, covered_cases())["scan"]["missing"], ["--future"]
        )

    def test_new_option_alias_requires_a_real_invocation(self):
        data = manifest()
        data["commands"][1]["arguments"][1]["aliases"] = ["--output"]
        cases = covered_cases()
        self.assertEqual(coverage_gaps(data, cases)["scan"]["missing"], ["--output"])
        cases.append(
            {
                "name": "alias",
                "argv": ["scan", "toy", "--output", "json"],
                "passed": True,
            }
        )
        self.assertEqual(coverage_gaps(data, cases)["scan"]["missing"], [])

    def test_short_option_requires_execution_and_maps_to_its_enum(self):
        data = manifest()
        data["commands"][1]["arguments"][1]["short"] = "-f"
        cases = covered_cases()
        self.assertEqual(coverage_gaps(data, cases)["scan"]["missing"], ["-f"])
        cases.append(
            {"name": "short", "argv": ["scan", "toy", "-f", "json"], "passed": True}
        )
        self.assertEqual(coverage_gaps(data, cases)["scan"]["missing"], [])

    def test_new_enum_value_requires_its_own_runtime_use(self):
        data = manifest()
        data["commands"][1]["arguments"][1]["values"].append("yaml")
        self.assertEqual(
            coverage_gaps(data, covered_cases())["scan"]["missing_values"],
            {"--format": ["yaml"]},
        )

    def test_new_subcommand_is_uncovered_even_when_help_passes(self):
        data = manifest()
        data["commands"].append({"name": "future", "arguments": []})
        cases = [
            *covered_cases(),
            {
                "name": "help future",
                "argv": ["future", "--help"],
                "passed": True,
                "kind": "help",
            },
        ]
        self.assertEqual(coverage_gaps(data, cases)["future"]["scenario_count"], 0)

    def test_failed_and_invalid_scenarios_do_not_cover_options(self):
        cases = covered_cases()
        cases[0]["passed"] = False
        cases[1]["kind"] = "invalid"
        self.assertIn("--format", coverage_gaps(manifest(), cases)["scan"]["missing"])

    def test_child_arguments_cannot_cover_host_options(self):
        data = {
            "root": {
                "arguments": [argument("--error-format", ["json"], global_option=True)]
            },
            "exec": {
                "arguments": [argument("--dry-run"), argument("native", last=True)]
            },
        }
        command, used, values = interface_usage(
            ["exec", "--", "toy", "--dry-run", "--error-format", "json"], data
        )
        self.assertEqual(command, "exec")
        self.assertEqual(used, {"native"})
        self.assertEqual(values, {})

    def test_option_value_cannot_cover_a_global_flag(self):
        option = argument("--input")
        option["takes_value"] = True
        commands = {
            "root": {
                "arguments": [argument("--error-format", ["json"], global_option=True)]
            },
            "exec": {"arguments": [option]},
        }
        _command, used, _values = interface_usage(
            ["exec", "--input", "--error-format"], commands
        )
        self.assertEqual(used, {"--input"})

    def test_equals_options_and_positionals_are_counted(self):
        commands = {item["name"]: item for item in manifest()["commands"]}
        command, used, values = interface_usage(
            ["--error-format=json", "scan", "toy", "--format=table"], commands
        )
        self.assertEqual(command, "scan")
        self.assertIn("tools", used)
        self.assertEqual(values["--format"], {"table"})

    def test_new_optional_positional_is_not_silently_covered(self):
        data = manifest()
        data["commands"][1]["arguments"].append(argument("extra", index=2))
        self.assertEqual(
            coverage_gaps(data, covered_cases())["scan"]["missing"], ["extra"]
        )

    def test_boolean_switch_does_not_require_literal_boolean_values(self):
        data = manifest()
        switch = argument("--offline")
        switch["values"] = ["true", "false"]
        data["commands"][1]["arguments"].append(switch)
        cases = covered_cases()
        cases[0]["argv"].append("--offline")
        self.assertEqual(coverage_gaps(data, cases)["scan"]["missing_values"], {})

    def test_new_example_entrypoint_fails_inventory_check(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "examples/new-example").mkdir(parents=True)
            (root / "examples/new-example/tool.py").write_text("print('new')")
            with self.assertRaises(RuntimeError):
                check_example_inventory(root)

    def test_registered_example_still_requires_a_passing_execution(self):
        cases = [{"example": path, "passed": True} for path in EXAMPLE_ENTRYPOINTS]
        check_example_coverage(cases)
        cases[0]["passed"] = False
        with self.assertRaises(RuntimeError):
            check_example_coverage(cases)


@unittest.skipUnless(os.name == "posix", "Process groups require Unix")
class ExecutionTests(unittest.TestCase):
    def test_exited_group_permission_error_does_not_fail_a_successful_case(self):
        process = Mock(pid=123)
        with patch(
            "runtime_validation.os.killpg", side_effect=PermissionError()
        ), patch("runtime_validation.group_has_live_process", return_value=False):
            stop_process_group(process)
        process.wait.assert_called()

    def test_live_group_permission_error_remains_a_cleanup_failure(self):
        process = Mock(pid=123)
        with patch(
            "runtime_validation.os.killpg", side_effect=PermissionError()
        ), patch("runtime_validation.group_has_live_process", return_value=True):
            with self.assertRaises(PermissionError):
                stop_process_group(process)

    def test_failed_case_makes_runner_fail_and_cleans_up_its_home(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "result.json"
            with contextlib.redirect_stdout(io.StringIO()), RuntimeValidation(
                sys.executable, report
            ) as session:
                root = session.root
                session.record(
                    "intentional failure",
                    [],
                    lambda entry: require(False, "fixture failure"),
                )
                self.assertEqual(session.finish(), 1)
            self.assertFalse(root.exists())
            saved = json.loads(report.read_text())
            self.assertEqual(saved["summary"]["failed"], 1)
            self.assertTrue(saved["workspace_removed"])

    def test_validation_checks_still_fail_under_python_optimization(self):
        script = "from runtime_validation import require; require(False, 'must fail')"
        result = subprocess.run(
            [sys.executable, "-O", "-c", script],
            env={**os.environ, "PYTHONPATH": str(Path(__file__).resolve().parents[1])},
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must fail", result.stderr)

    def test_timed_out_child_and_descendants_are_cleaned_up(self):
        with tempfile.TemporaryDirectory() as directory:
            with RuntimeValidation(
                sys.executable, Path(directory) / "result.json"
            ) as session:
                marker = session.root / "escaped"
                child = (
                    "import time;from pathlib import Path;time.sleep(0.8);Path(%r).write_text('escaped')"
                    % str(marker)
                )
                script = (
                    "import subprocess,sys,time;subprocess.Popen([sys.executable,'-c',%r]);time.sleep(5)"
                    % child
                )
                with self.assertRaises(subprocess.TimeoutExpired):
                    session.execute(["-c", script], timeout=0.2)
                import time

                time.sleep(1)
                self.assertFalse(marker.exists())


if __name__ == "__main__":
    unittest.main()

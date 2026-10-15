"""Shared standard-library infrastructure for real CLI and example checks."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import tempfile
import time
import traceback
from contextlib import contextmanager
from urllib.error import HTTPError
from urllib.request import ProxyHandler, Request, build_opener


class ValidationError(RuntimeError):
    """A required runtime behavior or coverage contract did not hold."""


def require(condition, message="Runtime validation failed"):
    if not condition:
        raise ValidationError(str(message))


def equal(actual, expected):
    require(actual == expected, f"Expected {expected!r}, received {actual!r}")


def contains(text):
    def check(result):
        require(text in result.stdout, f"Missing {text!r} in stdout: {result.stdout}")

    return check


def json_output(result):
    return json.loads(result.stdout)


def free_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


HTTP_OPENER = build_opener(ProxyHandler({}))


def http(url, method="GET", payload=None, token=None, headers=None):
    fields = dict(headers or {})
    if token:
        fields["Authorization"] = f"Bearer {token}"
    if payload is not None:
        fields["Content-Type"] = "application/json"
    request = Request(
        url,
        data=None if payload is None else json.dumps(payload).encode(),
        headers=fields,
        method=method,
    )
    try:
        with HTTP_OPENER.open(request, timeout=6) as response:
            return response.status, response.read().decode(), dict(response.headers)
    except HTTPError as error:
        return error.code, error.read().decode(), dict(error.headers)


def rpc(url, method, params=None, token=None):
    status, body, _headers = http(
        url,
        "POST",
        {"jsonrpc": "2.0", "id": 1, "method": method, "params": params or {}},
        token,
        {"MCP-Protocol-Version": "2025-06-18"},
    )
    equal(status, 200)
    return json.loads(body)


def group_has_live_process(group_id):
    executable = shutil.which("ps", path=os.defpath)
    require(executable, "Process cleanup requires the system ps command")
    result = subprocess.run(
        [executable, "-A", "-o", "pgid=,stat="],
        capture_output=True,
        text=True,
        timeout=5,
        check=True,
    )
    for line in result.stdout.splitlines():
        fields = line.split()
        if (
            len(fields) == 2
            and fields[0] == str(group_id)
            and not fields[1].startswith("Z")
        ):
            return True
    return False


def signal_process_group(group_id, requested_signal):
    try:
        os.killpg(group_id, requested_signal)
    except ProcessLookupError:
        pass
    except PermissionError:
        # macOS can refuse signals to a group whose remaining members have
        # already exited. A live member or a failed query must still fail.
        if group_has_live_process(group_id):
            raise


def stop_process_group(process):
    """Reap the owned process and kill descendants even if the parent exited."""
    signal_process_group(process.pid, signal.SIGTERM)
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        pass
    signal_process_group(process.pid, signal.SIGKILL)
    process.wait(timeout=5)


def interface_usage(arguments, commands):
    """Read option/positional use against the compiled Clap interface."""
    global_options = {
        spelling: item
        for item in commands["root"]["arguments"]
        if item.get("long") and item.get("global")
        for spelling in argument_spellings(item)
    }
    used, values, positionals = set(), {}, []
    index = 0
    while index < len(arguments) and arguments[index].startswith("-"):
        token, separator, inline = arguments[index].partition("=")
        option = global_options.get(token)
        if option:
            used.update([token, option["long"]])
            if option["takes_value"]:
                value = (
                    inline
                    if separator
                    else arguments[index + 1]
                    if index + 1 < len(arguments)
                    else None
                )
                if value is not None:
                    values.setdefault(option["long"], set()).add(value)
        index += 1 + bool(option and option["takes_value"] and not separator)
    parts = []
    while index < len(arguments):
        candidate = " ".join([*parts, arguments[index]])
        if candidate not in commands:
            break
        parts.append(arguments[index])
        index += 1
    command = " ".join(parts) or "root"
    descriptors = commands.get(command, commands["root"])["arguments"]
    options = {
        spelling: item
        for item in descriptors
        if item.get("long")
        for spelling in argument_spellings(item)
    }
    options.update(global_options)
    while index < len(arguments):
        token = arguments[index]
        if token == "--":
            remaining = arguments[index + 1 :]
            for item in descriptors:
                if item.get("last") and remaining:
                    used.add(item["id"])
            break
        flag, separator, inline = token.partition("=")
        option = options.get(flag)
        if option:
            used.add(flag)
            used.add(option["long"])
            if option["takes_value"]:
                if separator:
                    value = inline
                elif index + 1 < len(arguments):
                    index += 1
                    value = arguments[index]
                else:
                    value = None
                if value is not None:
                    values.setdefault(option["long"], set()).add(value)
        elif not token.startswith("-"):
            positionals.append(token)
        index += 1
    for item in descriptors:
        position = item.get("index")
        if position and not item.get("last") and len(positionals) >= position:
            used.add(item["id"])
            values.setdefault(item["id"], set()).add(positionals[position - 1])
    return command, used, values


def argument_spellings(argument):
    return [
        value
        for value in [
            argument.get("long"),
            argument.get("short"),
            *argument.get("aliases", []),
            *argument.get("short_aliases", []),
        ]
        if value
    ]


def coverage_gaps(manifest, cases):
    commands = {item["name"]: item for item in manifest["commands"]}
    observed = {name: {"cases": [], "used": set(), "values": {}} for name in commands}
    for case in cases:
        if not case.get("passed") or case.get("kind") in {"help", "invalid"}:
            continue
        command, used, values = interface_usage(case.get("argv", []), commands)
        for name in {command, "root"}:
            item = observed[name]
            item["cases"].append(case["name"])
            item["used"].update(used)
            for flag, variants in values.items():
                item["values"].setdefault(flag, set()).update(variants)
    result = {}
    for name, command in commands.items():
        item = observed[name]
        expected = []
        missing_values = {}
        for argument in command["arguments"]:
            if argument["id"] in {"help", "version"}:
                continue
            if name != "root" and argument.get("global"):
                continue
            key = argument.get("long") or argument["id"]
            expected.extend(
                argument_spellings(argument) if argument.get("long") else [key]
            )
            variants = set(argument["values"]) if argument["takes_value"] else set()
            absent = variants - item["values"].get(key, set())
            if absent:
                missing_values[key] = sorted(absent)
        result[name] = {
            "parameters": sorted(expected),
            "missing": sorted(set(expected) - item["used"]),
            "missing_values": missing_values,
            "scenario_count": len(item["cases"]),
        }
    return result


class RuntimeValidation:
    def __init__(self, binary, report, manifest=None, required_shells=()):
        self.repository = Path(__file__).resolve().parents[1]
        self.binary = str(Path(binary).resolve())
        self.report_path = Path(report).resolve()
        self.manifest = manifest
        self.required_shells = set(required_shells)
        self.report = {"schema_version": 1, "cases": [], "coverage": {}, "not_run": []}
        self.temporary = None

    def __enter__(self):
        require(os.name == "posix", "Runtime validation requires a Unix host")
        require(
            Path(self.binary).is_file(), f"Build apexe before validation: {self.binary}"
        )
        self.temporary = tempfile.TemporaryDirectory(prefix="apexe-runtime-")
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.home.mkdir()
        directories = [str(Path(self.binary).parent), *os.defpath.split(os.pathsep)]
        for tool in ["git", "jq", "curl", "zsh", "fish", "pwsh", "bwrap", "python3"]:
            resolved = shutil.which(tool)
            if resolved:
                directories.append(str(Path(resolved).parent))
        self.environment = {
            "HOME": str(self.home),
            "PATH": os.pathsep.join(dict.fromkeys(directories)),
            "TMPDIR": str(self.root),
            "LC_ALL": "C",
            "APEXE_OFFLINE": "true",
            "APEXE_OVERLAY_DIRS": "",
            "SHELL": "/bin/bash",
        }
        self.report["workspace"] = str(self.root)
        source = self.repository / "examples/openapi_import/demo.py"
        spec = importlib.util.spec_from_file_location("apexe_openapi_example", source)
        self.demo = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.demo)
        return self

    def __exit__(self, error_type, error, traceback):
        if error:
            self.report["fatal_error"] = str(error)
        self.temporary.cleanup()
        self.report["workspace_removed"] = not self.root.exists()
        self.save()

    def save(self):
        self.report_path.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(
            mode="w", dir=self.report_path.parent, delete=False
        ) as output:
            json.dump(self.report, output, indent=2)
            temporary = Path(output.name)
        temporary.replace(self.report_path)

    def record(self, name, arguments, body, kind="behavior"):
        entry = {"name": name, "argv": list(map(str, arguments)), "kind": kind}
        self.report["cases"].append(entry)
        try:
            body(entry)
            entry["passed"] = True
        except Exception as error:
            entry.update(
                passed=False,
                error=str(error),
                traceback=traceback.format_exc(),
                process_log=self.report.get("last_process_log", ""),
            )
            print(f"FAIL {name}: {error}", flush=True)
        self.save()
        return entry

    @contextmanager
    def process(self, arguments, environment=None, executable=None):
        with tempfile.TemporaryFile(mode="w+") as output:
            process = subprocess.Popen(
                [executable or self.binary, *map(str, arguments)],
                env=environment or self.environment,
                cwd=self.root,
                stdout=output,
                stderr=output,
                start_new_session=True,
            )
            try:
                yield process
            finally:
                stop_process_group(process)
                output.seek(0)
                self.report["last_process_log"] = output.read()

    def execute(
        self,
        arguments,
        environment=None,
        cwd=None,
        stdin=None,
        timeout=30,
        executable=None,
    ):
        """Capture bounded execution and always remove its process group."""
        with tempfile.TemporaryFile(mode="w+") as output, tempfile.TemporaryFile(
            mode="w+"
        ) as errors:
            process = subprocess.Popen(
                [executable or self.binary, *map(str, arguments)],
                env=environment or self.environment,
                cwd=cwd or self.root,
                stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
                stdout=output,
                stderr=errors,
                text=True,
                start_new_session=True,
            )
            try:
                process.communicate(stdin, timeout=timeout)
            finally:
                stop_process_group(process)
            output.seek(0)
            errors.seek(0)
            return subprocess.CompletedProcess(
                process.args, process.returncode, output.read(), errors.read()
            )

    def run(
        self,
        name,
        args,
        code=0,
        check=None,
        stdin=None,
        env=None,
        cwd=None,
        timeout=30,
        kind=None,
    ):
        if kind is None:
            kind = (
                "help"
                if "--help" in args or name.startswith("metadata")
                else "invalid"
                if name.startswith("invalid arguments")
                else "behavior"
            )

        def body(entry):
            result = self.execute(
                args, environment=env, cwd=cwd, stdin=stdin, timeout=timeout
            )
            entry.update(
                returncode=result.returncode, stdout=result.stdout, stderr=result.stderr
            )
            expected = {code} if isinstance(code, int) else set(code)
            require(
                result.returncode in expected,
                f"Exit {result.returncode}, expected {sorted(expected)}: {result.stderr}\n{result.stdout}",
            )
            if check:
                check(result)

        return self.record(name, args, body, kind)

    def ready(self, url, process, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            require(process.poll() is None, "Server exited during startup")
            try:
                if http(url + "/health")[0] == 200:
                    return
            except OSError:
                pass
            time.sleep(0.05)
        raise ValidationError("Server did not become healthy")

    def shell_executable(self, shell):
        return shutil.which(
            "pwsh" if shell == "powershell" else shell, path=self.environment["PATH"]
        )

    def check_coverage(self):
        require(
            self.manifest is not None, "A compiled CLI interface manifest is required"
        )
        coverage = coverage_gaps(self.manifest, self.report["cases"])
        self.report["coverage"] = coverage
        gaps = {
            name: item
            for name, item in coverage.items()
            if item["missing"] or item["missing_values"] or not item["scenario_count"]
        }
        self.record(
            "compiled CLI coverage",
            [],
            lambda _entry: require(not gaps, f"Uncovered CLI interface: {gaps}"),
            kind="coverage",
        )

    def finish(self):
        failed = [
            case["name"] for case in self.report["cases"] if not case.get("passed")
        ]
        self.report["summary"] = {
            "passed": len(self.report["cases"]) - len(failed),
            "failed": len(failed),
        }
        self.save()
        print(json.dumps(self.report["summary"]))
        return 1 if failed else 0

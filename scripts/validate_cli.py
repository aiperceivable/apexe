#!/usr/bin/env python3
"""Validate every compiled CLI parameter through real disposable execution."""

import argparse
import base64
from dataclasses import dataclass
import hashlib
import hmac
import json
import re
import shutil
import time
from pathlib import Path
from urllib.request import Request

from runtime_validation import (
    HTTP_OPENER,
    RuntimeValidation,
    contains,
    equal,
    free_port,
    http,
    json_output,
    require,
    rpc,
)


@dataclass
class CliFixture:
    binary: Path
    rules: Path
    overlay: Path
    bindings: Path
    acl: Path


def validate_help(session, fixture):
    for command in [
        item["name"] if item["name"] != "root" else ""
        for item in session.manifest["commands"]
    ]:
        args = command.split() + ["--help"]
        entry = session.run("help " + (command or "root"), args)
        session.report.setdefault("help_inventory", {})[command or "root"] = sorted(
            set(
                re.findall(
                    "^\\s+(?:-\\w,\\s+)?(--[\\w-]+)", entry.get("stdout", ""), re.M
                )
            )
        )
    for args in [["-h"], ["--version"], ["-V"], ["--man"], ["help", "exec"]]:
        session.run("metadata " + " ".join(args), args)


def validate_scan(session, fixture):
    fixture.binary = session.root / "toy"
    fixture.binary.write_text(
        '#!/bin/sh\ncase "$1" in\n--help|-h|help) printf "Usage: toy [OPTIONS]\\nOptions:\\n  --ready  Print readiness\\n  --wait  Pause execution\\n";;\n--version|-V|version) echo "toy 1.0";;\n--wait) sleep 4; echo late;;\n*) printf "ready\\n";;\nesac\n'
    )
    fixture.binary.chmod(493)
    fixture.rules = session.root / "rules"
    (fixture.rules / "overlays").mkdir(parents=True)
    fixture.overlay = fixture.rules / "overlays/toy.json"
    fixture.overlay.write_text(
        json.dumps(
            {
                "schema_version": "1.1",
                "command": "toy",
                "variant": "unknown",
                "mode": "authoritative",
                "confidence": "low",
                "description": "Synthetic real CLI validation fixture",
                "cross_flag_ordering": False,
                "short_flag_bundling": False,
                "streaming_stdin_safe": False,
                "flags": [
                    {
                        "long": "--ready",
                        "type": "boolean",
                        "description": "Print readiness",
                    },
                    {
                        "long": "--wait",
                        "type": "boolean",
                        "description": "Pause execution",
                    },
                ],
                "annotations": {
                    "readonly": True,
                    "destructive": False,
                    "idempotent": True,
                    "requires_approval": False,
                    "open_world": False,
                },
            }
        )
    )
    session.environment["PATH"] = str(session.root) + ":" + session.environment["PATH"]
    fixture.bindings = session.home / ".apexe/bindings"
    fixture.acl = session.home / ".apexe/acl.yaml"
    for fmt in ["table", "json", "yaml"]:
        args = [
            "scan",
            "toy",
            "--output-dir",
            fixture.bindings,
            "--depth",
            "1",
            "--no-cache",
            "--offline",
            "--format",
            fmt,
            "--skills-dir",
            session.root / "skills",
            "--overlay",
            fixture.overlay,
            "--verify",
        ]
        session.run(
            "scan output " + fmt,
            args,
            check=(lambda p: equal(json_output(p)["commands"][0]["id"], "cli.toy"))
            if fmt == "json"
            else None,
        )
    session.run(
        "scan dry run",
        [
            "scan",
            "toy",
            "--overlay",
            fixture.overlay,
            "--output-dir",
            session.root / "absent",
            "--skills-dir",
            session.root / "absent-skills",
            "--dry-run",
            "--depth",
            "5",
        ],
        check=lambda p: equal((session.root / "absent").exists(), False),
    )


def validate_configuration(session, fixture):
    for level in ["off", "error", "warn", "info", "debug", "trace"]:
        session.run(
            "global logging " + level,
            [
                "--log-level",
                level,
                "--log-stderr",
                "--timeout",
                "2",
                "config",
                "--show",
            ],
            check=contains("log_level:"),
        )
    for args in [[], ["--show"], ["--init"], ["--init"]]:
        session.run("config " + " ".join(args), ["config", *args])
    for key in ["log.level", "log_level"]:
        for level in ["off", "error", "warn", "info", "debug", "trace"]:
            session.run(
                "config set " + key + " " + level, ["config", "set", key, level]
            )
    for error_format in ["text", "json"]:
        session.run(
            "global error format " + error_format,
            ["--error-format", error_format, "config", "--show"],
        )


def validate_list(session, fixture):
    for fmt in ["table", "json"]:
        for verbose in [False, True]:
            session.run(
                "list " + fmt + str(verbose),
                [
                    "list",
                    "--bindings-dir",
                    fixture.bindings,
                    "--format",
                    fmt,
                    "--available-only",
                    "--acl",
                    fixture.acl,
                    *(["--verbose"] if verbose else []),
                ],
                check=contains("cli.toy"),
            )


def validate_execution(session, fixture):
    session.run(
        "inline input",
        [
            "exec",
            "cli.toy",
            "--input",
            '{"ready":true}',
            "--bindings-dir",
            fixture.bindings,
            "--acl",
            fixture.acl,
        ],
        check=lambda p: equal(json_output(p)["stdout"], "ready\n"),
    )
    request = session.root / "input.json"
    request.write_text('{"ready":true}')
    for source in [str(request), "-"]:
        session.run(
            "file input " + source,
            [
                "exec",
                "cli.toy",
                "--input-file",
                source,
                "--bindings-dir",
                fixture.bindings,
            ],
            stdin=request.read_text(),
            check=lambda p: equal(json_output(p)["stdout"], "ready\n"),
        )
    session.run(
        "module preflight",
        [
            "exec",
            "cli.toy",
            "--input",
            "{}",
            "--dry-run",
            "--as",
            "ci",
            "--acl",
            fixture.acl,
        ],
        check=contains("allowed"),
    )
    session.run(
        "native execution",
        [
            "exec",
            "--bindings-dir",
            fixture.bindings,
            "--acl",
            fixture.acl,
            "--",
            "toy",
            "--ready",
        ],
        check=contains("ready"),
    )
    session.run(
        "native preflight",
        ["exec", "--dry-run", "--as", "ci", "--", "toy", "--ready"],
        check=contains("allowed"),
    )
    for policy in ["deny", "approval", "allow"]:
        session.run(
            "unmanaged ACL refuses " + policy,
            ["exec", "--unmanaged", policy, "--", "/usr/bin/printf", "literal; data"],
            code=(1, 2, 3, 4),
        )
        clean = session.root / ("unmanaged-" + policy)
        clean.mkdir()
        session.run(
            "unmanaged " + policy,
            ["exec", "--unmanaged", policy, "--", "/usr/bin/printf", "literal; data"],
            env={**session.environment, "HOME": str(clean)},
            code=0 if policy == "allow" else (1, 2, 3, 4),
            check=contains("literal; data") if policy == "allow" else None,
        )
    session.run(
        "global native timeout",
        ["--timeout", "1", "exec", "--", "toy", "--wait"],
        code=(1, 124),
        timeout=8,
    )


def validate_policy(session, fixture):
    for fmt in ["table", "json"]:
        session.run("policy summary " + fmt, ["policy", "--format", fmt])
        for mode in ["read", "write"]:
            session.run(
                "policy allowed " + fmt + mode,
                [
                    "policy",
                    "--format",
                    fmt,
                    "--path",
                    session.root / "file",
                    "--mode",
                    mode,
                ],
            )
            session.run(
                "policy denied " + fmt + mode,
                [
                    "policy",
                    "--format",
                    fmt,
                    "--path",
                    session.home / ".ssh/id_rsa",
                    "--mode",
                    mode,
                ],
                code=0,
                check=contains("DENY")
                if fmt == "table"
                else lambda p: equal(json_output(p)["decision"], "deny"),
            )


def validate_source_verification(session, fixture):
    for isolation in ["auto", "required", "none"]:
        report = session.root / (isolation + "-report.json")
        session.run(
            "verify source " + isolation,
            [
                "verify",
                "--bindings-dir",
                fixture.bindings,
                "--overlay",
                fixture.overlay,
                "--report",
                report,
                "--strict",
                "--depth",
                "1",
                "--isolation",
                isolation,
            ],
            check=lambda p: equal(json_output(p)["drift"][0]["status"], "unchanged"),
        )


def validate_shells(session, fixture):
    for shell in ["sh", "bash", "zsh", "fish", "powershell"]:
        shim = session.root / ("shims-" + shell)
        rc = session.root / ("rc-" + shell)
        session.run(
            "env enable " + shell,
            [
                "env",
                "--enable",
                "--shell",
                shell,
                "--rc-file",
                rc,
                "--shim-dir",
                shim,
                "--bindings-dir",
                fixture.bindings,
                "--acl",
                fixture.acl,
                "--tags",
                "cli",
                "--prefix",
                "cli.toy",
            ],
        )
        session.run(
            "env activation " + shell,
            ["env", "--shell", shell, "--shim-dir", shim],
            check=contains(str(shim)),
        )
        active = {
            **session.environment,
            "PATH": str(shim) + ":" + session.environment["PATH"],
            "APEXE_SHIM_DIR": str(shim),
        }
        session.run(
            "env check " + shell, ["env", "--check", "--shim-dir", shim], env=active
        )
        if session.shell_executable(shell):

            def shell_body(entry, shell=shell, shim=shim):
                activate = session.execute(
                    ["env", "--shell", shell, "--shim-dir", str(shim)]
                ).stdout
                shell_script = activate + (
                    "\n(Get-Command toy).Source\ntoy --ready\n"
                    if shell == "powershell"
                    else "\ncommand -v toy\ntoy --ready\n"
                )
                shell_args = (
                    ["-NoProfile", "-Command", shell_script]
                    if shell == "powershell"
                    else ["-c", shell_script]
                )
                result = session.execute(
                    shell_args, executable=session.shell_executable(shell), timeout=10
                )
                equal(result.returncode, 0)
                require(
                    str(shim / "toy") in result.stdout and "ready" in result.stdout,
                    (result.stdout, result.stderr),
                )
                entry["stdout"] = result.stdout

            session.record(
                "actual shell activation " + shell,
                ["env", "--shell", shell, "--shim-dir", str(shim)],
                shell_body,
            )
        session.run(
            "env reload " + shell,
            ["env", "--reload", "--shim-dir", shim, "--shell", shell, "--rc-file", rc],
        )
        session.run(
            "env disable " + shell,
            ["env", "--disable", "--shim-dir", shim],
            check=lambda p: equal((shim / "toy").exists(), False),
        )
    for shell in ["sh", "bash", "zsh", "fish", "powershell"]:
        if not session.shell_executable(shell):
            session.report["not_run"].append(
                {
                    "name": "actual shell activation " + shell,
                    "reason": "Shell executable not installed",
                }
            )
            if shell in session.required_shells:
                session.record(
                    "required shell " + shell,
                    [],
                    lambda entry, shell=shell: require(
                        False, "Required shell not installed: " + shell
                    ),
                )


def validate_initialization(session, fixture):
    for args in [
        ["init", "-q"],
        ["add", "."],
        [
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Fixture",
        ],
        ["tag", "reviewed"],
    ]:
        equal(
            session.execute(
                ["-C", str(fixture.rules), *args],
                executable=shutil.which("git", path=session.environment["PATH"]),
            ).returncode,
            0,
        )
    for fmt in ["json", "table"]:
        session.run(
            "init local pinned " + fmt,
            [
                "init",
                "--overlays",
                fixture.rules,
                "--ref",
                "reviewed",
                "--refresh",
                "--offline",
                "--format",
                fmt,
            ],
            timeout=60,
        )


def validate_client_configuration(session, fixture):
    for client in ["claude-desktop", "cursor"]:
        for transport in ["stdio", "http", "sse"]:
            session.run(
                "client config " + client + transport,
                [
                    "serve",
                    "--show-config",
                    client,
                    "--name",
                    "validation",
                    "--transport",
                    transport,
                    "--host",
                    "127.0.0.1",
                    "--port",
                    "8911",
                    "--bindings-dir",
                    fixture.bindings,
                    "--acl",
                    fixture.acl,
                    "--tags",
                    "cli",
                    "--prefix",
                    "cli.toy",
                    "--enable-approval",
                    "--no-logging",
                    "--no-log-arguments",
                    "--no-circuit-breaker",
                    "--no-retry",
                ],
                check=lambda p: json_output(p),
            )


def validate_stdio(session, fixture):
    for flags in [
        [],
        ["--no-logging", "--no-circuit-breaker", "--no-retry"],
        ["--no-log-arguments"],
        ["--enable-approval"],
        ["--explorer", "--metrics"],
    ]:
        args = [
            "serve",
            "--transport",
            "stdio",
            "--bindings-dir",
            fixture.bindings,
            "--name",
            "validation-stdio",
            "--tags",
            "cli",
            "--prefix",
            "cli.toy",
            "--acl",
            fixture.acl,
            *flags,
        ]

        def body(entry, args=args):
            with session.demo.McpSession(
                [session.binary, *map(str, args)], session.environment, session.root
            ) as mcp_session:
                listed = mcp_session.request("tools/list", {})["result"]["tools"]
                listed = [x for x in listed if x["name"].startswith("cli.")]
                equal(len(listed), 1)
                response = mcp_session.request(
                    "tools/call",
                    {"name": listed[0]["name"], "arguments": {"ready": True}},
                )
                require("ready" in json.dumps(response), response)
                entry["result"] = response

        session.record("stdio actual call " + " ".join(flags), args, body)


def validate_authentication(session, fixture):
    def jwt(secret):
        def enc(obj):
            payload = json.dumps(obj, separators=(",", ":")).encode()
            return base64.urlsafe_b64encode(payload).rstrip(b"=").decode()

        value = (
            enc({"alg": "HS256", "typ": "JWT"})
            + "."
            + enc(
                {"sub": "validator", "roles": ["reader"], "exp": int(time.time()) + 600}
            )
        )
        return (
            value
            + "."
            + base64.urlsafe_b64encode(
                hmac.new(secret.encode(), value.encode(), hashlib.sha256).digest()
            )
            .rstrip(b"=")
            .decode()
        )

    for surface in ["serve", "a2a"]:
        for auth in ["none", "token", "jwt"]:
            port = free_port()
            url = f"http://127.0.0.1:{port}"
            token = "validation-test-token"
            secret = "validation-jwt-secret-at-least-32-bytes"
            credential = (
                token if auth == "token" else jwt(secret) if auth == "jwt" else None
            )
            args = [
                surface,
                "--bindings-dir",
                fixture.bindings,
                "--name",
                "validation-network",
                "--acl",
                fixture.acl,
                "--tags",
                "cli",
                "--prefix",
                "cli.toy",
                "--auth",
                auth,
                "--no-log-arguments",
                "--no-circuit-breaker",
                "--no-retry",
            ]
            args += (
                ["--auth-token", token]
                if auth == "token"
                else ["--jwt-secret", secret]
                if auth == "jwt"
                else []
            )
            args += (
                [
                    "--transport",
                    "http",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    port,
                    "--explorer",
                    "--metrics",
                ]
                if surface == "serve"
                else [
                    "--url",
                    url,
                    "--execution-timeout",
                    "1",
                    "--explorer",
                    "--cors-origin",
                    "https://one.example",
                    "--cors-origin",
                    "https://two.example",
                ]
            )

            def body(
                entry,
                args=args,
                url=url,
                surface=surface,
                credential=credential,
                auth=auth,
            ):
                with session.process(args) as p:
                    session.ready(url, p)
                    if surface == "serve":
                        if auth != "none":
                            equal(
                                http(
                                    url + "/mcp",
                                    "POST",
                                    {
                                        "jsonrpc": "2.0",
                                        "id": 1,
                                        "method": "tools/list",
                                        "params": {},
                                    },
                                )[0],
                                401,
                            )
                        equal(
                            rpc(
                                url + "/mcp",
                                "initialize",
                                {
                                    "protocolVersion": "2025-06-18",
                                    "capabilities": {},
                                    "clientInfo": {"name": "validator", "version": "1"},
                                },
                                credential,
                            )["result"]["serverInfo"]["name"],
                            "validation-network",
                        )
                        listed = rpc(url + "/mcp", "tools/list", token=credential)[
                            "result"
                        ]["tools"]
                        listed = [x for x in listed if x["name"].startswith("cli.")]
                        equal(len(listed), 1)
                        result = rpc(
                            url + "/mcp",
                            "tools/call",
                            {"name": listed[0]["name"], "arguments": {"ready": True}},
                            credential,
                        )
                        equal(
                            result["result"]["structuredContent"]["stdout"], "ready\n"
                        )
                        equal(http(url + "/metrics", token=credential)[0], 200)
                        equal(http(url + "/usage", token=credential)[0], 200)
                        equal(http(url + "/explorer/")[0], 200)
                    else:
                        card = json.loads(http(url + "/.well-known/agent-card.json")[1])
                        equal(card["name"], "validation-network")
                        equal(len(card["skills"]), 1)
                        payload = {
                            "jsonrpc": "2.0",
                            "id": 1,
                            "method": "message/send",
                            "params": {
                                "message": {
                                    "messageId": "validation",
                                    "role": "ROLE_USER",
                                    "parts": [{"data": {"ready": True}}],
                                    "metadata": {"skillId": "cli.toy"},
                                }
                            },
                        }
                        if auth != "none":
                            equal(http(url, "POST", payload)[0], 401)
                        status, body, headers = http(
                            url,
                            "POST",
                            payload,
                            credential,
                            {"Origin": "https://two.example"},
                        )
                        equal(status, 200)
                        result = json.loads(body)
                        equal(
                            result["result"]["status"]["state"], "TASK_STATE_COMPLETED"
                        )
                        equal(
                            headers.get("access-control-allow-origin"),
                            "https://two.example",
                        )
                        payload["params"]["message"]["parts"] = [
                            {"data": {"wait": True}}
                        ]
                        payload["params"]["message"]["messageId"] = "timeout"
                        result = json.loads(http(url, "POST", payload, credential)[1])
                        require(
                            result["result"]["status"]["state"]
                            != "TASK_STATE_COMPLETED",
                            result,
                        )
                    entry["actual_network_call"] = True

            session.record(surface + " network " + auth, args, body)


def validate_public_bind(session, fixture):
    for surface in ["serve", "a2a"]:
        port = free_port()
        url = f"http://127.0.0.1:{port}"
        args = [
            surface,
            "--bindings-dir",
            fixture.bindings,
            "--auth",
            "none",
            "--allow-unauthenticated-bind",
            "--no-logging",
        ]
        args += (
            ["--transport", "http", "--host", "0.0.0.0", "--port", port]
            if surface == "serve"
            else ["--url", f"http://0.0.0.0:{port}"]
        )

        def body(entry, args=args, url=url):
            with session.process(args) as process:
                session.ready(url, process)
                entry["health"] = 200

        session.record("acknowledged public " + surface, args, body)


def validate_openapi(session, fixture):
    source = json.loads(
        (session.repository / "examples/openapi_import/openapi.json").read_text()
    )
    with session.demo.api_server(source) as api:
        for surface in ["serve", "a2a"]:
            port = free_port()
            url = f"http://127.0.0.1:{port}"
            args = [
                surface,
                "--bindings-dir",
                session.root / "empty-api-bindings",
                "--openapi",
                api["url"] + "/openapi.json",
                "--openapi-base-url",
                api["url"],
                "--openapi-prefix",
                "pets",
                "--openapi-header",
                "X-Spec-Key: " + api["spec_token"],
                "--openapi-header",
                "X-Reviewed: true",
                "--openapi-token",
                api["api_token"],
                "--auth",
                "none",
            ]
            args += (
                ["--transport", "http", "--port", port]
                if surface == "serve"
                else ["--url", url]
            )

            def body(entry, args=args, url=url, surface=surface):
                with session.process(args) as process:
                    session.ready(url, process)
                    if surface == "serve":
                        result = rpc(
                            url + "/mcp",
                            "tools/call",
                            {
                                "name": "pets.get_pet",
                                "arguments": {"id": "42", "q": "matrix"},
                            },
                        )
                        require("matrix" in json.dumps(result), result)
                    else:
                        result = session.demo.a2a_request(
                            url, "unused", "pets.get_pet", {"id": "42", "q": "matrix"}
                        )
                        equal(
                            result["result"]["status"]["state"], "TASK_STATE_COMPLETED"
                        )
                    entry["upstream_read"] = True

            session.record("OpenAPI complete flags " + surface, args, body)


def validate_sse(session, fixture):
    port = free_port()
    url = f"http://127.0.0.1:{port}"
    args = [
        "serve",
        "--transport",
        "sse",
        "--allow-deprecated-sse",
        "--host",
        "127.0.0.1",
        "--port",
        port,
        "--bindings-dir",
        fixture.bindings,
        "--auth",
        "token",
        "--auth-token",
        "sse-token",
    ]

    def sse_body(entry):
        with session.process(args) as process:
            session.ready(url, process)
            stream = HTTP_OPENER.open(
                Request(
                    url + "/sse",
                    headers={
                        "Authorization": "Bearer sse-token",
                        "Accept": "text/event-stream",
                    },
                ),
                timeout=8,
            )
            try:
                endpoint = None
                while endpoint is None:
                    line = stream.readline().decode().strip()
                    if line.startswith("data:"):
                        endpoint = line[5:].strip()
                target = url + endpoint if endpoint.startswith("/") else endpoint

                def call(method, params):
                    status, _, _ = http(
                        target,
                        "POST",
                        {"jsonrpc": "2.0", "id": 1, "method": method, "params": params},
                        "sse-token",
                    )
                    require(status in [200, 202], status)
                    while True:
                        line = stream.readline().decode().strip()
                        if line.startswith("data:"):
                            result = json.loads(line[5:])
                            if result.get("id") == 1:
                                return result

                initialized = call(
                    "initialize",
                    {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": {"name": "matrix", "version": "1"},
                    },
                )
                require("result" in initialized, initialized)
                listed = call("tools/list", {})["result"]["tools"]
                name = next((x["name"] for x in listed if x["name"].startswith("cli.")))
                result = call(
                    "tools/call", {"name": name, "arguments": {"ready": True}}
                )
                require("ready" in json.dumps(result), result)
                entry["result"] = result
            finally:
                stream.close()

    session.record("legacy SSE actual exchange", args, sse_body)


def validate_recorded_cases(session, fixture):
    cases = session.root / "cases.json"
    cases.write_text(
        json.dumps(
            {
                "allowlist": ["cli.toy"],
                "cases": [
                    {
                        "module_id": "cli.toy",
                        "mode": "isolated",
                        "binary_digest": hashlib.sha256(
                            fixture.binary.read_bytes()
                        ).hexdigest(),
                        "fixtures": {},
                        "input": {"ready": True},
                        "expected_stdout": "ready\n",
                    }
                ],
            }
        )
    )
    args = [
        "verify",
        "--bindings-dir",
        fixture.bindings,
        "--overlay",
        fixture.overlay,
        "--cases",
        cases,
        "--allow-dry-run",
        "--isolation",
        "required",
        "--record",
        "--report",
        session.root / "behavior.json",
    ]
    session.run(
        "verified isolated case",
        args,
        check=lambda p: equal(json_output(p)["behavioral"][0]["status"], "passed"),
    )


def validate_invalid_arguments(session, fixture):
    request = session.root / "input.json"
    invalid = [
        ["--timeout", "0", "list"],
        ["--timeout", "18446744073709551615", "serve"],
        ["--log-level", "verbose", "list"],
        ["--error-format", "xml", "list"],
        ["scan", "toy", "--depth", "0"],
        ["scan", "toy", "--depth", "6"],
        ["scan", "toy", "--format", "xml"],
        ["init", "--format", "yaml"],
        ["exec"],
        ["exec", "cli.toy", "--input", "{}", "--input-file", request],
        ["exec", "cli.toy", "--as", "ci"],
        ["exec", "--unmanaged", "bad", "--", "toy"],
        ["env", "--enable", "--disable"],
        ["env", "--check", "--enable"],
        ["env", "--rc-file", session.root / "badrc"],
        ["env", "--shell", "invalid"],
        ["list", "--format", "yaml"],
        ["config", "--init", "--show"],
        ["config", "set", "unknown", "info"],
        ["verify", "--record"],
        ["verify", "--allow-dry-run"],
        ["verify", "--isolation", "invalid"],
        ["policy", "--mode", "execute"],
        ["policy", "--format", "yaml"],
        ["serve", "--port", "0"],
        ["serve", "--port", "65536"],
        ["serve", "--transport", "invalid"],
        ["serve", "--auth", "invalid"],
        ["serve", "--show-config", "unknown"],
        ["a2a", "--auth", "invalid"],
        ["serve", "--openapi-prefix", "pets"],
        ["a2a", "--openapi-header", "X-Test: value"],
    ]
    for index, args in enumerate(invalid):
        session.run(
            "invalid arguments " + str(index),
            ["--error-format", "json", *args],
            code=2,
            check=lambda p: json.loads(p.stderr),
        )
    for surface in ["serve", "a2a"]:
        for flags in [
            ["--auth", "jwt"],
            ["--auth", "none", "--host", "0.0.0.0"]
            if surface == "serve"
            else ["--auth", "none", "--url", "http://0.0.0.0:8899"],
            ["--tags", "cli,"],
        ]:
            session.run(
                "startup refusal " + surface + " ".join(flags),
                [
                    surface,
                    "--bindings-dir",
                    fixture.bindings,
                    *(["--transport", "http"] if surface == "serve" else []),
                    *flags,
                ],
                code=1,
            )


def validate_cli(session):
    fixture = CliFixture(
        session.root / "toy",
        session.root / "rules",
        session.root / "rules/overlays/toy.json",
        session.home / ".apexe/bindings",
        session.home / ".apexe/acl.yaml",
    )
    validate_help(session, fixture)
    validate_scan(session, fixture)
    validate_configuration(session, fixture)
    validate_list(session, fixture)
    validate_execution(session, fixture)
    validate_policy(session, fixture)
    validate_source_verification(session, fixture)
    validate_shells(session, fixture)
    validate_initialization(session, fixture)
    validate_client_configuration(session, fixture)
    validate_stdio(session, fixture)
    validate_authentication(session, fixture)
    validate_public_bind(session, fixture)
    validate_openapi(session, fixture)
    validate_sse(session, fixture)
    validate_recorded_cases(session, fixture)
    validate_invalid_arguments(session, fixture)
    session.check_coverage()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    repository = Path(__file__).resolve().parents[1]
    parser.add_argument("--apexe", type=Path, default=repository / "target/debug/apexe")
    parser.add_argument(
        "--manifest",
        type=Path,
        required=True,
        help="Compiled Clap interface exported by the Rust integration test",
    )
    parser.add_argument(
        "--report", type=Path, default=repository / "target/runtime-validation/cli.json"
    )
    parser.add_argument(
        "--require-shell",
        action="append",
        default=[],
        choices=["sh", "bash", "zsh", "fish", "powershell"],
    )
    arguments = parser.parse_args()
    manifest = json.loads(arguments.manifest.read_text())
    with RuntimeValidation(
        arguments.apexe, arguments.report, manifest, arguments.require_shell
    ) as session:
        validate_cli(session)
        return session.finish()


if __name__ == "__main__":
    raise SystemExit(main())

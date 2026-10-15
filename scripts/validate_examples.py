#!/usr/bin/env python3
"""Execute every documented example with isolated homes and local services."""

import argparse
import json
from pathlib import Path
import shutil
import sys

from runtime_validation import (
    RuntimeValidation,
    contains,
    equal,
    free_port,
    http,
    require,
    rpc,
)


EXAMPLE_ENTRYPOINTS = {
    "basic/run.sh",
    "cli_permissions/run.sh",
    "local_execution/demo.py",
    "verification/demo.py",
    "openapi_import/demo.py",
    "acl_demo/main.rs",
    "path_guard/main.rs",
    "programmatic.rs",
}


def check_example_inventory(repository):
    examples = repository / "examples"
    discovered = set()
    for source in examples.rglob("*"):
        if source.is_file() and (
            source.name in {"demo.py", "run.sh", "main.rs"}
            or source.parent == examples
            and source.suffix == ".rs"
        ):
            discovered.add(source.relative_to(examples).as_posix())
    equal(discovered, EXAMPLE_ENTRYPOINTS)
    # Discover Cargo targets too: a new custom example path must not escape the gate.
    import subprocess

    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--no-deps",
            "--offline",
            "--locked",
            "--format-version",
            "1",
        ],
        cwd=repository,
        capture_output=True,
        text=True,
        timeout=30,
        check=True,
    )
    targets = {
        Path(target["src_path"]).relative_to(examples).as_posix()
        for package in json.loads(result.stdout)["packages"]
        if Path(package["manifest_path"]).parent == repository
        for target in package["targets"]
        if "example" in target["kind"]
    }
    equal(targets, {path for path in EXAMPLE_ENTRYPOINTS if path.endswith(".rs")})


def run_rust_examples(session):
    for name in ["acl_demo", "path_guard", "programmatic"]:
        executable = Path(session.binary).parent / "examples" / name

        def body(entry, executable=executable, name=name):
            require(
                executable.is_file(),
                "Build examples with cargo build --all-features --examples",
            )
            result = session.execute([], executable=str(executable), timeout=90)
            entry.update(
                returncode=result.returncode, stdout=result.stdout, stderr=result.stderr
            )
            equal(result.returncode, 0)
            if name == "programmatic":
                contains("MCP server built successfully")(result)
                require(
                    "Export failed" not in result.stderr
                    and "Build failed" not in result.stderr,
                    result.stderr,
                )
            elif name == "acl_demo":
                contains("admin     -> ALLOWED")(result)
                equal(result.stdout.count("DENIED"), 2)
            else:
                contains("REFUSED")(result)
                contains("ALLOWED")(result)

        session.record("Rust example " + name, [], body)


def run_corpus_example(session, corpus):
    script = session.repository / "examples/cli_permissions/run.sh"
    bash = shutil.which("bash", path=session.environment["PATH"])
    environment = {**session.environment, "CLI_PERMISSIONS_DIR": str(corpus.parent)}

    def body(entry):
        result = session.execute(
            [script], executable=bash, environment=environment, timeout=90
        )
        entry.update(
            returncode=result.returncode, stdout=result.stdout, stderr=result.stderr
        )
        equal(result.returncode, 0)
        for module in ["cli.ls", "cli.grep", "cli.sort", "cli.tail", "--acl"]:
            contains(module)(result)

    session.record("cli_permissions walkthrough", [], body)
    for missing in ["", str(session.root / "missing")]:

        def refused(entry, missing=missing):
            result = session.execute(
                [script],
                executable=bash,
                environment={**environment, "CLI_PERMISSIONS_DIR": missing},
            )
            equal(result.returncode, 2)
            entry["stderr"] = result.stderr

        session.record("cli_permissions missing source " + missing, [], refused)


def basic_calls(url):
    token = "example-validation-token"
    initialized = rpc(
        url + "/mcp",
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "example-validation", "version": "1"},
        },
        token,
    )
    equal(initialized["result"]["serverInfo"]["name"], "apexe")
    tools = rpc(url + "/mcp", "tools/list", token=token)["result"]["tools"]
    names = {tool["name"] for tool in tools}
    require({"cli.ls", "cli.curl"}.issubset(names), names)
    for arguments in [{}, {"l": True, "file": ["/etc/hosts"]}]:
        result = rpc(
            url + "/mcp",
            "tools/call",
            {"name": "cli.ls", "arguments": arguments},
            token,
        )
        require("error" not in result and not result["result"].get("isError"), result)
        equal(result["result"]["structuredContent"]["exit_code"], 0)
    # ACL refusal must happen before the external URL could be contacted.
    denied = rpc(
        url + "/mcp",
        "tools/call",
        {
            "name": "cli.curl",
            "arguments": {"url": "https://httpbin.org/get", "silent": True},
        },
        token,
    )
    require("error" in denied or denied.get("result", {}).get("isError"), denied)


def run_basic_example(session):
    port = free_port()
    url = f"http://127.0.0.1:{port}"
    environment = {**session.environment, "APEXE_EXAMPLE_PORT": str(port)}
    script = session.repository / "examples/basic/run.sh"
    bash = shutil.which("bash", path=session.environment["PATH"])

    def body(entry):
        with session.process(
            [script], executable=bash, environment=environment
        ) as process:
            session.ready(url, process, timeout=90)
            equal(http(url + "/explorer/")[0], 200)
            equal(http(url + "/explorer/tools")[0], 200)
            basic_calls(url)
            entry["documented_calls"] = {
                "ls_empty": True,
                "ls_flags_operand": True,
                "curl_acl_refused": True,
            }

    session.record("basic walkthrough and live documented calls", [], body)


def run_python_example(session, name, arguments, check):
    script = session.repository / "examples" / name / "demo.py"

    def body(entry):
        result = session.execute(
            [script, "--apexe", session.binary, *arguments],
            executable=sys.executable,
            timeout=90,
        )
        entry.update(
            returncode=result.returncode, stdout=result.stdout, stderr=result.stderr
        )
        equal(result.returncode, 0)
        summary = json.loads(result.stdout)
        check(summary)
        entry["result"] = summary

    session.record(
        "Python example " + name + " " + " ".join(map(str, arguments)), [], body
    )


def check_local(summary):
    for key in ["acl_denied", "audited", "temporary_home_removed"]:
        equal(summary[key], True)
    equal(summary["captured_stdout"], "one\ntwo\n")


def check_native(summary):
    check_local(summary)
    equal(summary["path_denied"], True)
    equal(summary["native"]["pipeline_stdout"], "one\n")
    for key in ["reloaded", "startup_loaded", "startup_removed", "uninstalled"]:
        equal(summary["native"][key], True)


def check_verification(summary):
    for key in [
        "supported",
        "dry_run",
        "isolated_effects",
        "case_file_invalidation",
        "binary_invalidation",
        "bindings_preserved",
    ]:
        equal(summary.get(key), True)
    require(
        summary["backends"], "Verification must execute on a real OS isolation backend"
    )


def check_openapi(summary, remote):
    equal(summary["upstream_calls"], 4)
    equal(summary["spec_fetches"], 2 if remote else 0)
    equal(summary["spec_credentials_not_forwarded"], True)
    for surface in ["mcp", "a2a"]:
        for key in ["read", "json_body", "delete_denied"]:
            equal(summary["surfaces"][surface][key], True)
    equal(summary["surfaces"]["a2a"]["authenticated"], True)


def run_python_examples(session, corpus):
    run_python_example(session, "local_execution", [], check_local)
    if sys.platform == "darwin":
        run_python_example(
            session, "local_execution", ["--corpus", corpus, "--shims"], check_native
        )
    else:
        session.report["not_run"].append(
            {
                "name": "BSD native cat/head walkthrough",
                "reason": "Reviewed streaming contracts are BSD-only; the macOS CI job requires this scenario",
            }
        )
    run_python_example(session, "verification", [], check_verification)
    for remote in [False, True]:
        run_python_example(
            session,
            "openapi_import",
            ["--remote-spec"] if remote else [],
            lambda summary, remote=remote: check_openapi(summary, remote),
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    repository = Path(__file__).resolve().parents[1]
    parser.add_argument("--apexe", type=Path, default=repository / "target/debug/apexe")
    parser.add_argument(
        "--corpus",
        type=Path,
        default=repository.parent / "cli-permissions/overlays",
        help="Reviewed cli-permissions overlays directory or checkout",
    )
    parser.add_argument(
        "--report",
        type=Path,
        default=repository / "target/runtime-validation/examples.json",
    )
    arguments = parser.parse_args()
    corpus = arguments.corpus.resolve()
    if (corpus / "overlays").is_dir():
        corpus /= "overlays"
    with RuntimeValidation(arguments.apexe, arguments.report) as session:
        session.record(
            "example entrypoint coverage",
            [],
            lambda entry: check_example_inventory(repository),
        )
        session.record(
            "reviewed corpus required",
            [],
            lambda entry: require(
                corpus.is_dir(), f"Missing reviewed corpus: {corpus}; pass --corpus"
            ),
        )
        session.environment.update(
            APEXE_OVERLAY_DIRS=str(corpus),
            APEXE_AUTH_TOKEN="example-validation-token",
            RUST_LOG="error",
        )
        run_rust_examples(session)
        if corpus.is_dir():
            run_corpus_example(session, corpus)
            run_basic_example(session)
            run_python_examples(session, corpus)
        session.report["examples"] = sorted(EXAMPLE_ENTRYPOINTS)
        return session.finish()


if __name__ == "__main__":
    raise SystemExit(main())

"""Run local governance and optional native PATH shims in a temporary home."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def invoke(apexe, arguments, environment, work, expected_success=True):
    result = subprocess.run([apexe, *arguments], env=environment, cwd=work,
                            capture_output=True, text=True, timeout=30, check=False)
    if (result.returncode == 0) != expected_success:
        raise RuntimeError(f"Unexpected status for {arguments[0]}: {result.stderr}")
    return result


def file_input(schema, path):
    properties = schema.get("properties", {})
    operands = [(name, value) for name, value in properties.items()
                if "x-apexe-positional" in value]
    if len(operands) != 1:
        raise RuntimeError("The cat binding needs one reviewed file operand")
    name, value = operands[0]
    return {name: [path] if value.get("type") == "array" else path}


def run(apexe, corpus, shims):
    with tempfile.TemporaryDirectory(prefix="apexe-local-example-") as temporary:
        root = Path(temporary)
        home, work = root / "home", root / "work"
        home.mkdir()
        work.mkdir()
        (work / "sample.txt").write_text("one\ntwo\n", encoding="utf-8")
        environment = {"HOME":str(home), "PATH":os.defpath, "LC_ALL":"C", "RUST_LOG":"error"}
        if corpus:
            environment["APEXE_OVERLAY_DIRS"] = str(corpus.resolve())
        scanned = invoke(apexe, ["scan", "cat", "head", "--offline", "--no-cache", "--format", "json"], environment, work)
        report = json.loads(scanned.stdout)
        schema = next(command["input_schema"] for command in report["commands"] if command["id"] == "cli.cat")
        arguments = ["run", "cli.cat", "--input", json.dumps(file_input(schema, "sample.txt"))]
        preflight = json.loads(invoke(apexe, [*arguments, "--dry-run", "--as", "ci"], environment, work).stdout)
        if not preflight["valid"] or not preflight["caller_asserted"]:
            raise RuntimeError("Expected an allowed, non-executing caller simulation")
        output = json.loads(invoke(apexe, arguments, environment, work).stdout)
        if output["stdout"] != "one\ntwo\n" or output["exit_code"] != 0:
            raise RuntimeError("Captured execution did not match the fixture")
        native = run_shims(apexe, environment, work, home) if shims else None
        paths_declared = any(value.get("x-apexe-path") or value.get("items", {}).get("x-apexe-path")
                             for value in schema.get("properties", {}).values() if "x-apexe-positional" in value)
        if paths_declared:
            probe = invoke(apexe, ["run", "cli.cat", "--input", json.dumps(file_input(schema, str(home / ".ssh/id_rsa"))), "--dry-run"], environment, work, False)
            verdict = json.loads(probe.stdout)
            if not any(not item["allowed"] for item in verdict["path_guard"]):
                raise RuntimeError("Preflight did not identify the protected path")
        acl = home / ".apexe/acl.yaml"
        acl.write_text("version: '1.0'\ndefault_effect: deny\nrules: []\n", encoding="utf-8")
        invoke(apexe, arguments, environment, work, False)
        audit = (home / ".apexe/audit.jsonl").read_text(encoding="utf-8")
        entries = [json.loads(line) for line in audit.splitlines()]
        if not any(entry.get("event") == "refusal" or entry.get("decision") == "deny" for entry in entries):
            raise RuntimeError("Expected an audited policy denial")
        return {"preflight":"allowed", "captured_stdout":output["stdout"],
                "path_denied":True if paths_declared else None, "acl_denied":True,
                "audited":True, "native":native, "temporary_home_removed":True}


def run_shims(apexe, environment, work, home):
    startup = home / ".bashrc"
    invoke(apexe, ["env", "--enable", "--shell", "bash", "--rc-file", str(startup)], environment, work)
    original_startup = startup.read_text(encoding="utf-8")
    invoke(apexe, ["env", "--reload"], environment, work)
    if startup.read_text(encoding="utf-8") != original_startup:
        raise RuntimeError("Reload unexpectedly changed shell startup configuration")
    manifest = json.loads((home / ".apexe/bin/.apexe-shims.json").read_text())
    if not {"cat", "head"}.issubset(manifest["commands"]):
        raise RuntimeError("Native example requires reviewed cat/head shim capabilities; provide --corpus")
    shell = shutil.which("bash", path=os.defpath)
    if not shell:
        raise RuntimeError("The native example requires bash")
    script = ('test "$(command -v cat)" = "$HOME/.apexe/bin/cat" && '
              'test "$(command -v head)" = "$HOME/.apexe/bin/head" || exit 1\n'
              'cat sample.txt | head -n1')
    result = subprocess.run([shell, "--noprofile", "--rcfile", str(startup),
                             "-i", "-c", script], env=environment, cwd=work,
                            capture_output=True, text=True, timeout=30, check=False)
    if result.returncode or result.stdout != "one\n":
        raise RuntimeError(f"Native pipeline failed: {result.stderr}")
    invoke(apexe, ["env", "--disable"], environment, work)
    if (home / ".apexe/bin/cat").exists():
        raise RuntimeError("Owned shims were not removed")
    if "# >>> apexe shims" in startup.read_text(encoding="utf-8"):
        raise RuntimeError("Owned shell startup block was not removed")
    return {"pipeline_stdout":result.stdout, "reloaded":True, "startup_loaded":True,
            "startup_removed":True, "uninstalled":True}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apexe", default="apexe")
    parser.add_argument("--corpus", type=Path, help="Reviewed cli-permissions overlays directory")
    parser.add_argument("--shims", action="store_true", help="Include the native cat/head pipeline")
    arguments = parser.parse_args()
    executable = shutil.which(arguments.apexe)
    if not executable:
        parser.error("apexe executable was not found; build it or supply --apexe")
    print(json.dumps(run(str(Path(executable).resolve()), arguments.corpus, arguments.shims), indent=2))


if __name__ == "__main__":
    main()

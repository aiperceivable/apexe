#!/usr/bin/env python3
"""Exercise scoped verification and confidence in a disposable local corpus."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def run(apexe, environment, root, *arguments):
    return subprocess.run([str(apexe), *map(str, arguments)], env=environment, cwd=root,
                          capture_output=True, text=True, timeout=90)


def require_success(result):
    if result.returncode:
        raise RuntimeError(result.stderr)
    return result


def write_cli(root, name, destructive, body):
    binary = root / name
    binary.write_text(f"""#!/bin/sh
case "$1" in
--help|-h|help) printf 'Usage: {name} [OPTIONS] FILE\\nOptions:\\n  --dry-run  Review without writes\\n'; exit 0;;
--version|-V|version) echo 'fixture 1.0'; exit 0;;
--dry-run|/*) ;;
*) exit 0;;
esac
{body}
""", encoding="utf-8")
    binary.chmod(0o755)
    overlay = {"schema_version": "1.1", "command": name, "variant": "unknown",
               "mode": "authoritative", "confidence": "low", "description": "Synthetic verification demo",
               "flags": [{"long": "--dry-run", "type": "boolean"}],
               "positional_args": [{"name": "file", "type": "path", "required": True}],
               "annotations": {"readonly": False, "idempotent": not destructive,
                               "destructive": destructive, "requires_approval": destructive, "open_world": False}}
    (root / "overlays" / (name + ".json")).write_text(json.dumps(overlay), encoding="utf-8")
    return binary


def case(binary, module_id, mode, output, files, dry_run):
    return {"module_id": module_id, "mode": mode,
            "binary_digest": hashlib.sha256(binary.read_bytes()).hexdigest(),
            "dry_run_parameter": "dry_run", "input": {"dry_run": dry_run, "file": "$FIXTURE/input"},
            "fixtures": {"input": "original"}, "expected_stdout": output, "expected_files": files}


def demonstrate(apexe):
    with tempfile.TemporaryDirectory(prefix="apexe-verification-demo-") as directory:
        root = Path(directory)
        (root / "overlays").mkdir()
        home = root / "home"
        home.mkdir()
        environment = dict(os.environ, HOME=str(home), RUST_LOG="error",
                           APEXE_OVERLAY_DIRS=str(root / "overlays"))
        for key in ["APEXE_BINDINGS_DIR", "APCORE_CONFIG"]:
            environment.pop(key, None)
        dry = write_cli(root, "verify-dry", True, "printf 'dry-run\\n'")
        effects = write_cli(root, "verify-effects", False, "printf changed > \"$1\"; printf 'isolated\\n'")
        require_success(run(apexe, environment, root, "scan", dry, effects, "--no-cache"))
        bindings = home / ".apexe/bindings"
        before = {path.name: path.read_bytes() for path in bindings.glob("*.binding.yaml")}
        cases_path = root / "cases.json"
        cases_path.write_text(json.dumps({"allowlist": ["cli.verify_dry", "cli.verify_effects"], "cases": [
            case(dry, "cli.verify_dry", "dry_run", "dry-run\n", {"input": "original"}, True),
            case(effects, "cli.verify_effects", "isolated", "isolated\n", {"input": "changed"}, False),
        ]}), encoding="utf-8")
        result = run(apexe, environment, root, "verify", "--cases", cases_path, "--allow-dry-run",
                     "--isolation", "required", "--record")
        report = json.loads(result.stdout)
        assert before == {path.name: path.read_bytes() for path in bindings.glob("*.binding.yaml")}
        unavailable = [entry for entry in report["behavioral"] if entry["status"] == "unverifiable"]
        if unavailable:
            assert all("isolation unavailable" in entry["reason"] for entry in unavailable), report
            return {"supported": False, "reason": unavailable[0]["reason"], "bindings_preserved": True}
        require_success(result)
        assert all(entry["status"] == "passed" for entry in report["behavioral"]), report

        def confidence():
            result = require_success(run(apexe, environment, root, "list", "--verbose", "--format", "json"))
            return {entry["module_id"]: entry["metadata"] for entry in json.loads(result.stdout)}

        assert all(metadata["confidence"] == "verified" for metadata in confidence().values())
        cases_path.write_text(cases_path.read_text() + "\n", encoding="utf-8")
        assert all(metadata.get("confidence") != "verified" for metadata in confidence().values())
        require_success(run(apexe, environment, root, "verify", "--cases", cases_path,
                            "--allow-dry-run", "--isolation", "required", "--record"))
        dry.write_text(dry.read_text() + "\n# Updated executable build\n", encoding="utf-8")
        assert confidence()["cli.verify_dry"].get("confidence") != "verified"
        return {"supported": True, "dry_run": True, "isolated_effects": True,
                "case_file_invalidation": True, "binary_invalidation": True, "bindings_preserved": True,
                "backends": sorted({entry["isolation"] for entry in report["behavioral"]})}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apexe", type=Path, default=Path("target/debug/apexe"))
    arguments = parser.parse_args()
    print(json.dumps(demonstrate(arguments.apexe.resolve()), indent=2))


if __name__ == "__main__":
    main()

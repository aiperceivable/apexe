"""Measure the verified readonly shim pilot in an isolated temporary home."""
import argparse
import json
import os
import platform
import statistics
import subprocess
import tempfile
import time
from pathlib import Path


def invoke(command, environment, directory, stdin=None):
    started = time.perf_counter_ns()
    result = subprocess.run(
        command, env=environment, cwd=directory, input=stdin,
        capture_output=True, timeout=10, check=False,
    )
    return result, (time.perf_counter_ns() - started) / 1_000_000


def percentile(values, fraction):
    return sorted(values)[min(len(values) - 1, int(len(values) * fraction))]


def refusal_pilot(binaries, home, environment, work):
    """Measure a published synthetic usage corpus, not a real-user frequency estimate."""
    samples = []
    for flags in [[], ["-n"], ["-b"], ["-e"], ["-s"], ["-t"], ["-u"], ["-v"], ["-ns"], ["-vet"]]:
        samples.append(("cat", [*flags, "sample.txt"], "ordinary"))
    for flags in [["-n1"], ["-n", "2"], ["-c2"], ["-c", "5"], []]:
        samples.append(("head", [*flags, "sample.txt"], "ordinary"))
    for flag in ["-a", "-A", "-l", "-la", "-1", "-C", "-F", "-p", "-R", "-t", "-r", "-S", "-n", "-h", "-i", "-s", "-d", "-m", "-o", "-k"]:
        samples.append(("ls", [flag, "."], "ordinary"))
    for left, right in [("-B", "-b"), ("-C", "-g"), ("-F", "-p"), ("-g", "-m"), ("-m", "-n"), ("-o", "-1")]:
        samples.extend(("ls", [first, second, "."], "order_conflict") for first, second in [(left, right), (right, left)])
    rows = []
    for tool, arguments, category in samples:
        native, _ = invoke([binaries[tool], *arguments], environment, work)
        shim, _ = invoke([str(home / ".apexe/bin" / tool), *arguments], environment, work)
        row = {"tool":tool, "argv":arguments, "category":category, "native_exit":native.returncode,
               "shim_exit":shim.returncode, "refused":native.returncode == 0 and shim.returncode != 0}
        if native.returncode == 0 and shim.returncode == 0 and native.stdout != shim.stdout:
            raise RuntimeError(f"Admitted argv changed native semantics: {row}")
        rows.append(row)
    accepted_native = [row for row in rows if row["native_exit"] == 0]
    ordinary = [row for row in accepted_native if row["category"] == "ordinary"]
    return {"sampling":"47 equally weighted synthetic cases; no real-user frequency claim",
            "native_valid":len(accepted_native), "refused":sum(row["refused"] for row in accepted_native),
            "refusal_rate":round(sum(row["refused"] for row in accepted_native) / len(accepted_native), 4),
            "ordinary_native_valid":len(ordinary), "ordinary_refused":sum(row["refused"] for row in ordinary),
            "cases":rows}


def measure(apexe, corpus, repetitions):
    with tempfile.TemporaryDirectory(prefix="apexe-shim-pilot-") as temporary:
        root = Path(temporary)
        home, work = root / "home", root / "work"
        home.mkdir()
        work.mkdir()
        (work / "sample.txt").write_text("one\n\ntwo\n", encoding="utf-8")
        environment = {
            "HOME": str(home), "PATH": os.defpath, "LC_ALL": "C", "LANG": "C",
            "RUST_LOG": "error", "APEXE_OVERLAY_DIRS": str(corpus),
        }
        for command in ([apexe, "scan", "cat", "head", "ls", "--no-cache"], [apexe, "env", "--enable", "--shell", "sh"]):
            result, _ = invoke(command, environment, work)
            if result.returncode:
                raise RuntimeError(result.stderr.decode("utf-8", errors="replace"))
        manifest = json.loads((home / ".apexe/bin/.apexe-shims.json").read_text())
        bindings = home / ".apexe/bindings"
        scan, _ = invoke([apexe, "scan", "cat", "head", "ls", "--format", "json"], environment, work)
        binaries = {tool["name"]: tool["binary_path"] for tool in json.loads(scan.stdout)["tools"]}
        cases = [
            ("cat file", "cat", ["sample.txt"], None),
            ("cat stdin", "cat", [], b"one\ntwo\n"),
            ("cat bundle", "cat", ["-ns", "sample.txt"], None),
            ("head file", "head", ["-n1", "sample.txt"], None),
            ("head stdin", "head", ["-n", "1"], b"one\ntwo\n"),
            ("ls bundle", "ls", ["-la", "."], None),
        ]
        measurements = []
        for label, tool, arguments, stdin in cases:
            if tool not in manifest["commands"]:
                raise RuntimeError(f"{tool} needs verified native capabilities in the corpus")
            native, governed = [], []
            for iteration in range(repetitions + 2):
                direct, direct_ms = invoke([binaries[tool], *arguments], environment, work, stdin)
                shim, shim_ms = invoke([str(home / ".apexe/bin" / tool), *arguments], environment, work, stdin)
                if (direct.returncode, direct.stdout) != (shim.returncode, shim.stdout):
                    raise RuntimeError(f"{label} differs: native={direct.returncode}, shim={shim.returncode}: {shim.stderr!r}")
                if iteration >= 2:
                    native.append(direct_ms)
                    governed.append(shim_ms)
            measurements.append({
                "case": label, "samples": repetitions,
                "native_median_ms": round(statistics.median(native), 3),
                "shim_median_ms": round(statistics.median(governed), 3),
                "shim_p95_ms": round(percentile(governed, 0.95), 3),
            })
        refused, _ = invoke([str(home / ".apexe/bin/cat"), "--apexe-undeclared"], environment, work)
        if refused.returncode == 0:
            raise RuntimeError("Undeclared flags must be refused")
        return {
            "platform": platform.platform(), "machine": platform.machine(),
            "bindings": len(list(bindings.glob("*.binding.yaml"))),
            "acl": "generated readonly allowlist", "audit": "enabled",
            "supported_cases_passed": len(cases), "undeclared_refusal_passed": True,
            "measurements": measurements,
            "refusal_pilot": refusal_pilot(binaries, home, environment, work),
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--apexe", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--samples", type=int, default=25)
    arguments = parser.parse_args()
    if arguments.samples < 1:
        parser.error("--samples must be positive")
    result = measure(str(arguments.apexe.resolve()), arguments.corpus.resolve(), arguments.samples)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()

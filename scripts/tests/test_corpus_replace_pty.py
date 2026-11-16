"""PTY tests for the interactive same-name corpus replacement prompt.

A non-interactive refusal is covered by unit tests; what those cannot show is
that answering the prompt does what it says. These cases run the real binary on
a terminal, answer `y` and `n`, and check which rules remain installed.
"""

import io
import json
import os
import pty
import select
import subprocess
import tarfile
import tempfile
import time
import unittest
from pathlib import Path

REPOSITORY = Path(__file__).resolve().parents[2]
APEXE = REPOSITORY / "target/debug/apexe"
NAME = "cli-permissions"


def overlay(flag, variant):
    return {
        "schema_version": "2.0",
        "command": "cat",
        "variant": variant,
        "mode": "merge",
        "confidence": "low",
        "cross_flag_ordering": False,
        "short_flag_bundling": False,
        "streaming_stdin_safe": False,
        "commands": [
            {
                "path": [],
                "flags": [
                    {
                        "long": flag,
                        "type": "boolean",
                        "description": "Fixture flag",
                    }
                ],
                "annotations": {
                    "readonly": True,
                    "destructive": False,
                    "idempotent": True,
                    "requires_approval": False,
                    "open_world": False,
                },
            }
        ],
    }


def manifest(label):
    return {
        "schema_version": "1.0",
        "name": NAME,
        "description": f"Fixture corpus {label}",
        "version": "0.1.0",
        "overlay_directory": "overlays",
        "license": "Apache-2.0",
    }


class CorpusReplacePtyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.home = Path(self.temporary.name)
        self.first = self.archive("a", "--fixture-a")
        self.second = self.archive("b", "--fixture-b")
        path = ":".join(
            entry
            for entry in os.environ.get("PATH", "").split(":")
            if entry and not entry.endswith("/.apexe/bin")
        )
        self.environment = {
            **os.environ,
            "HOME": str(self.home),
            "PATH": path,
            "SHELL": "/bin/sh",
            "APEXE_OFFLINE": "true",
            "RUST_LOG": "error",
        }
        for name in (
            "APEXE_DEFAULT_OVERLAY_REPOSITORY",
            "APEXE_OVERLAY_DIRS",
            "APEXE_BINDINGS_DIR",
            "APEXE_SHIM_DIR",
            "APCORE_CONFIG",
        ):
            self.environment.pop(name, None)
        self.install(self.first)

    def tearDown(self):
        self.temporary.cleanup()

    def archive(self, label, flag):
        path = self.home / f"corpus-{label}.tar.gz"
        top = f"corpus-{label}"
        with tarfile.open(path, "w:gz") as tar:
            for name, document in [
                ("manifest.json", manifest(label)),
                ("overlays/cat@bsd.json", overlay(flag, "bsd")),
                ("overlays/cat@gnu.json", overlay(flag, "gnu")),
            ]:
                payload = json.dumps(document).encode()
                info = tarfile.TarInfo(f"{top}/{name}")
                info.size = len(payload)
                tar.addfile(info, io.BytesIO(payload))
        return path

    def install(self, archive):
        result = subprocess.run(
            [str(APEXE), "init", "--overlays", str(archive)],
            cwd=self.home,
            env=self.environment,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def installed_source(self):
        result = subprocess.run(
            [str(APEXE), "corpus", "list", "--format", "json"],
            cwd=self.home,
            env=self.environment,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        corpora = json.loads(result.stdout)
        self.assertEqual(len(corpora), 1, corpora)
        return corpora[0]["repository"]

    def run_pty(self, arguments, answer, timeout=20):
        master, slave = pty.openpty()
        process = subprocess.Popen(
            [str(APEXE), *arguments],
            cwd=self.home,
            env=self.environment,
            stdin=slave,
            stdout=slave,
            stderr=slave,
        )
        os.close(slave)
        output = b""
        sent = False
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                try:
                    chunk = os.read(master, 4096)
                except OSError:
                    break
                if not chunk:
                    break
                output += chunk
                if not sent and b"Replace it with" in output:
                    os.write(master, answer)
                    sent = True
            if process.poll() is not None:
                break
        try:
            code = process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            code = process.wait()
        os.close(master)
        return output.decode("utf-8", errors="replace"), code, sent

    def test_answering_yes_replaces_the_installed_corpus(self):
        output, code, prompted = self.run_pty(
            ["init", "--overlays", str(self.second)], b"y\n"
        )
        self.assertTrue(prompted, output)
        self.assertEqual(code, 0, output)
        self.assertIn("Replacing corpus", output)
        self.assertIn("corpus-b.tar.gz", self.installed_source())

    def test_answering_no_keeps_the_installed_corpus(self):
        output, code, prompted = self.run_pty(
            ["init", "--overlays", str(self.second)], b"n\n"
        )
        self.assertTrue(prompted, output)
        self.assertNotEqual(code, 0, output)
        self.assertIn("kept the installed corpus", output)
        self.assertIn("corpus-a.tar.gz", self.installed_source())


if __name__ == "__main__":
    unittest.main()

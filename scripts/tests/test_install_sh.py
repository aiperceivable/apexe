"""Tests for the install script's platform resolution and dry-run plan."""

import functools
import hashlib
import http.server
import os
from pathlib import Path
import platform
import pty
import select
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/install.sh"

TARGETS = {
    ("Darwin", "arm64"): "aarch64-apple-darwin",
    ("Darwin", "x86_64"): "x86_64-apple-darwin",
    ("Linux", "x86_64"): "x86_64-unknown-linux-gnu",
    ("Linux", "aarch64"): "aarch64-unknown-linux-gnu",
}


def run(*arguments):
    return subprocess.run(
        ["sh", str(SCRIPT), *arguments],
        capture_output=True,
        text=True,
        env=dict(os.environ),
    )


def pipe_script_through_a_terminal(arguments, environment, timeout=40.0):
    """Run the script exactly as `curl ... | sh` does, with a terminal present.

    stdin is a pipe carrying the script in small pieces, and a pty provides the
    controlling terminal `/dev/tty` resolves to. A script that rebinds its own
    stdin stops reading the pipe and waits on the terminal instead, which shows
    up here as a deadline instead of a blocked suite.

    Returns the terminal output, the child's wait status and whether the
    deadline was hit.
    """
    read_end, write_end = os.pipe()
    pid, terminal = pty.fork()
    if pid == 0:  # child: script on the pipe, terminal for /dev/tty
        os.dup2(read_end, 0)
        os.close(read_end)
        os.close(write_end)
        os.execve("/bin/sh", ["sh", "-s", "--", *arguments], environment)
    os.close(read_end)

    script = SCRIPT.read_bytes()
    output = b""
    sent = 0
    status = None
    stalled = True
    eof = False
    deadline = time.monotonic() + timeout
    try:
        while time.monotonic() < deadline:
            if sent < len(script):
                try:
                    sent += os.write(write_end, script[sent:sent + 64])
                except BrokenPipeError:
                    sent = len(script)
                time.sleep(0.002)
            if eof:
                time.sleep(0.01)
            else:
                ready, _, _ = select.select([terminal], [], [], 0.02)
                if ready:
                    try:  # EIO once the child is gone
                        chunk = os.read(terminal, 65536)
                    except OSError:
                        chunk = b""
                    if chunk:
                        output += chunk
                    else:
                        eof = True
            waited, state = os.waitpid(pid, os.WNOHANG)
            if waited == pid:
                status, stalled = state, False
                break
    finally:
        os.close(write_end)
        if stalled:
            try:
                os.kill(pid, 9)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
    return output.decode(errors="replace"), status, stalled


class InstallPlanTests(unittest.TestCase):
    def test_dry_run_names_the_target_and_its_checksum(self):
        target = TARGETS.get((platform.system(), platform.machine()))
        if target is None:
            self.skipTest("the install script only supports macOS and Linux")
        result = run("--dry-run", "--version", "v0.15.0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"apexe-{target}.tar.gz", result.stdout)
        self.assertIn(f"apexe-{target}.tar.gz.sha256", result.stdout)
        self.assertIn("/releases/download/v0.15.0/", result.stdout)

    def test_dry_run_uses_the_latest_release_path_by_default(self):
        result = run("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("/releases/latest/download/", result.stdout)

    def test_dry_run_creates_nothing(self):
        prefix = Path(tempfile.mkdtemp()) / "bin"
        result = run("--dry-run", "--prefix", str(prefix), "--no-setup")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(str(prefix), result.stdout)
        self.assertNotIn("Then:", result.stdout)
        self.assertFalse(prefix.exists())

    def test_setup_flags_are_forwarded(self):
        result = run("--dry-run", "--yes", "--no-hosts", "--offline")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("apexe setup --yes --no-hosts --offline", result.stdout)

    def test_unknown_option_is_rejected(self):
        result = run("--bogus")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown option", result.stderr)

    def test_option_without_a_value_is_rejected(self):
        result = run("--version")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--version requires a tag", result.stderr)

    def test_script_parses_under_posix_sh(self):
        result = subprocess.run(
            ["sh", "-n", str(SCRIPT)], capture_output=True, text=True
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()


class _QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):  # keep test output readable
        pass


class InstallExecutionTests(unittest.TestCase):
    """Run the script against a local release server.

    A real request exercises what a dry run cannot: checksum verification,
    extraction, installation, and the refusal to install unverified bytes.
    """

    @classmethod
    def setUpClass(cls):
        cls.target = TARGETS.get((platform.system(), platform.machine()))
        if cls.target is None:
            raise unittest.SkipTest("the install script only supports macOS and Linux")
        cls.directory = tempfile.TemporaryDirectory()
        root = Path(cls.directory.name)
        release = root / "releases/latest/download"
        release.mkdir(parents=True)
        name = f"apexe-{cls.target}"
        package = root / name
        package.mkdir()
        binary = package / "apexe"
        binary.write_text(
            "#!/bin/sh\n"
            "if [ \"$1\" = \"--version\" ]; then echo 'apexe 0.0.0-fixture'; exit 0; fi\n"
            "log=${APEXE_FIXTURE_LOG:-/dev/null}\n"
            "if [ -t 0 ]; then terminal=yes; else terminal=no; fi\n"
            "printf '%s tty=%s\\n' \"$*\" \"$terminal\" >> \"$log\"\n",
            encoding="utf-8",
        )
        binary.chmod(0o755)
        archive = release / f"{name}.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(package, arcname=name)
        cls.checksum = release / f"{name}.tar.gz.sha256"
        cls.correct_checksum = (
            f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {name}.tar.gz\n"
        )
        cls.checksum.write_text(cls.correct_checksum, encoding="utf-8")
        handler = functools.partial(_QuietHandler, directory=str(root))
        cls.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = f"http://127.0.0.1:{cls.server.server_address[1]}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.directory.cleanup()

    def install(self, prefix):
        return subprocess.run(
            ["sh", str(SCRIPT), "--prefix", str(prefix), "--no-setup"],
            capture_output=True,
            text=True,
            env=dict(os.environ, APEXE_RELEASE_REPOSITORY=self.base),
        )

    def test_install_downloads_verifies_and_places_the_binary(self):
        prefix = Path(self.directory.name) / "prefix-ok"
        result = self.install(prefix)
        self.assertEqual(result.returncode, 0, result.stderr)
        installed = prefix / "apexe"
        self.assertTrue(installed.is_file())
        version = subprocess.run(
            [str(installed), "--version"], capture_output=True, text=True
        )
        self.assertIn("apexe 0.0.0-fixture", version.stdout)

    def test_piped_script_hands_off_without_reading_its_own_text(self):
        """`curl | sh` must not end up waiting for the script on the terminal.

        The script is its own shell's stdin, so attaching the terminal to the
        shell (rather than to `apexe setup`) makes it look for the rest of the
        script on the tty and stall after `Installed ...`.
        """
        prefix = Path(self.directory.name) / "prefix-handoff"
        log = Path(self.directory.name) / "setup.log"
        environment = dict(
            os.environ,
            APEXE_RELEASE_REPOSITORY=self.base,
            APEXE_FIXTURE_LOG=str(log),
        )
        output, status, stalled = pipe_script_through_a_terminal(
            ["--prefix", str(prefix)], environment
        )
        self.assertFalse(stalled, f"the piped script stalled:\n{output}")
        self.assertEqual(status, 0, output)
        self.assertIn("Preparing rules", output)
        recorded = log.read_text(encoding="utf-8") if log.is_file() else ""
        self.assertIn("setup", recorded, output)
        # The terminal reaches setup, which is where a prompt belongs.
        self.assertIn("tty=yes", recorded, output)

    def test_install_refuses_a_checksum_mismatch(self):
        self.checksum.write_text("deadbeef  apexe.tar.gz\n", encoding="utf-8")
        try:
            prefix = Path(self.directory.name) / "prefix-bad"
            result = self.install(prefix)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("checksum verification failed", result.stderr)
            self.assertFalse((prefix / "apexe").exists())
        finally:
            self.checksum.write_text(self.correct_checksum, encoding="utf-8")

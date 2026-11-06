import os
import pty
import select
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

REPOSITORY = Path(__file__).resolve().parents[2]
APEXE = REPOSITORY / "target/debug/apexe"


class AgentShellPtyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.home = Path(self.temporary.name)
        self.bindings = self.home / ".apexe/bindings"
        self.bindings.mkdir(parents=True)
        self.script = self.home / "approval.sh"
        self.script.write_text("#!/bin/sh\necho APPROVED\n")
        self.script.chmod(0o755)
        self.bindings.joinpath("cli.approval.binding.yaml").write_text(
            self.binding_yaml(self.script)
        )
        self.environment = {
            **os.environ,
            "HOME": str(self.home),
            "SHELL": "/bin/sh",
            "APEXE_OFFLINE": "true",
            "RUST_LOG": "error",
        }
        for name in (
            "APEXE_REAL_SHELL",
            "APEXE_SHIM_DIR",
            "APEXE_BINDINGS_DIR",
            "APCORE_CONFIG",
        ):
            self.environment.pop(name, None)
        subprocess.run(
            [str(APEXE), "env", "--enable"],
            cwd=self.home,
            env=self.environment,
            check=True,
            capture_output=True,
        )

    def tearDown(self):
        self.temporary.cleanup()

    @staticmethod
    def binding_yaml(script):
        return f"""spec_version: '1.0'
bindings:
- module_id: cli.approval
  target: exec://{script}
  description: PTY approval probe
  documentation: null
  tags:
  - pilot
  version: 1.0.0
  annotations:
    readonly: true
    destructive: false
    idempotent: false
    requires_approval: true
    open_world: false
    streaming: false
    cacheable: false
    cache_ttl: 0
    cache_key_fields: null
    paginated: false
    pagination_style: cursor
    discoverable: true
    extra: {{}}
  examples: []
  metadata:
    apexe_scan_format: 3
    command_path:
    - approval
    variant: unknown
    shim_contract:
      authoritative: true
      variant: unknown
      short_flag_bundling: true
      streaming_stdin_safe: false
      cross_flag_ordering: false
  input_schema:
    type: object
    x-apexe-end-of-options: true
    x-apexe-short-flag-bundling: true
    x-apexe-cross-flag-ordering: false
    x-apexe-streaming-stdin-safe: false
    properties: {{}}
    additionalProperties: false
  output_schema:
    type: object
"""

    def run_pty(
        self,
        arguments,
        timeout=6,
        extra_environment=None,
        input_bytes=None,
        input_delay=0.0,
    ):
        environment = {**self.environment, **(extra_environment or {})}
        pid, master = pty.fork()
        if pid == 0:
            os.chdir(self.home)
            os.execvpe(str(APEXE), [str(APEXE), *arguments], environment)
        output = b""
        started = time.monotonic()
        deadline = started + timeout
        sent = input_bytes is None
        while time.monotonic() < deadline:
            if not sent and time.monotonic() >= started + input_delay:
                os.write(master, input_bytes)
                sent = True
            ready, _, _ = select.select([master], [], [], 0.1)
            if ready:
                try:
                    chunk = os.read(master, 4096)
                except OSError:
                    break
                if not chunk:
                    break
                output += chunk
            done, _ = os.waitpid(pid, os.WNOHANG)
            if done:
                break
        else:
            os.kill(pid, 9)
            os.waitpid(pid, 0)
        while True:
            ready, _, _ = select.select([master], [], [], 0.2)
            if not ready:
                break
            try:
                chunk = os.read(master, 4096)
            except OSError:
                break
            if not chunk:
                break
            output += chunk
        os.close(master)
        return output.decode("utf-8", errors="replace")

    def test_ctrl_c_interrupts_the_foreground_command(self):
        output = self.run_pty(
            ["sh", "-c", "sleep 30; echo DONE"],
            timeout=6,
            input_bytes=b"\x03",
            input_delay=0.6,
        )
        self.assertNotIn("DONE", output)

    def test_interactive_shell_runs_a_background_job_and_waits(self):
        output = self.run_pty(
            ["sh"],
            timeout=6,
            input_bytes=b"sleep 0.2 &\nwait\necho JOB_DONE\nexit\n",
            input_delay=0.4,
        )
        self.assertIn("JOB_DONE", output)

    def test_approval_times_out_without_input(self):
        output = self.run_pty(
            ["sh", "-c", "approval"],
            timeout=6,
            extra_environment={"APEXE_APPROVAL_TIMEOUT_SECONDS": "1"},
        )
        self.assertIn("timed out after 1 seconds", output)
        self.assertNotIn("APPROVED", output)


if __name__ == "__main__":
    unittest.main()

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
        self.script = self.home / "approval"
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
    # A current binding carries the per-route grammar stamp; without it the
    # route is `legacy` (admitted like a scanned one, never reviewed -- #47).
    grammar: reviewed
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
        wait_for=None,
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
            if (
                not sent
                and time.monotonic() >= started + input_delay
                and (wait_for is None or wait_for in output)
            ):
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

    def embedded_approval_fixture(self, mode, parent_requires_approval, child_requires_approval):
        find = self.home / "find"
        find.write_text('#!/bin/sh\necho PARENT_STARTED\nexec /usr/bin/find "$@"\n')
        find.chmod(0o755)
        find_binding = self.binding_yaml(find).replace(
            "cli.approval", "cli.find"
        ).replace("    - approval", "    - find").replace(
            "requires_approval: true", "requires_approval: false"
        ).replace(
            "    properties: {}",
            """    properties:
      path:
        type: array
        items: {type: string, x-apexe-path: true}
        x-apexe-positional: 0
        x-apexe-operand-position: before-flags
      expression:
        type: array
        items: {type: string}
        x-apexe-positional: 1
        x-apexe-tail-parsed: true
      exec:
        type: string
        x-apexe-flag: -exec
        x-apexe-exec-list: true""",
        )
        self.bindings.joinpath("cli.find.binding.yaml").write_text(find_binding)
        child_binding = self.binding_yaml(self.script)
        if not child_requires_approval:
            child_binding = child_binding.replace(
                "requires_approval: true", "requires_approval: false"
            )
        self.bindings.joinpath("cli.approval.binding.yaml").write_text(child_binding)
        self.home.joinpath(".apexe/config.yaml").write_text(f"child_exec:\n  mode: {mode}\n")
        self.home.joinpath("embedded.txt").touch()
        acl = "default_effect: allow\nrules:"
        if parent_requires_approval:
            acl += """
- callers: [local]
  targets: [cli.find]
  effect: allow
  approval: required
"""
        else:
            acl += " []\n"
        self.home.joinpath(".apexe/acl.yaml").write_text(acl)
        subprocess.run(
            [str(APEXE), "env", "--enable"],
            cwd=self.home,
            env=self.environment,
            check=True,
            capture_output=True,
        )

    def test_embedded_approval_decision_is_combined_in_both_modes(self):
        args = ["exec", "--", str(self.home / "find"), "embedded.txt", "-exec", "approval", ";"]
        path = str(self.home) + os.pathsep + self.environment["PATH"]
        extra = {"PATH": path, "APEXE_APPROVAL_TIMEOUT_SECONDS": "1"}
        audit_path = self.home.joinpath(".apexe/audit.jsonl")
        def grant_count():
            return audit_path.read_text().count('"decision":"delegate_grant"') if audit_path.exists() else 0

        for mode in ("analyze", "delegate"):
            for parent, child in ((False, False), (True, False), (False, True), (True, True)):
                with self.subTest(mode=mode, parent=parent, child=child):
                    self.embedded_approval_fixture(mode, parent, child)
                    if parent or child:
                        for answer in (b"n\n", None):
                            before = grant_count()
                            refused = self.run_pty(
                                args, extra_environment=extra, input_bytes=answer, wait_for=b"Approve?"
                            )
                            self.assertEqual(refused.count("Approve?"), 1, refused)
                            self.assertNotIn("PARENT_STARTED", refused)
                            self.assertNotIn("APPROVED", refused)
                            if answer is None:
                                self.assertIn("timed out after 1 seconds", refused)
                            self.assertEqual(grant_count(), before)
                        if child:
                            self.assertIn("Approval also covers embedded modules: cli.approval", refused)
                    accepted = self.run_pty(
                        args, extra_environment=extra, input_bytes=b"y\n", wait_for=b"Approve?"
                    )
                    self.assertEqual(accepted.count("Approve?"), int(parent or child), accepted)
                    self.assertIn("PARENT_STARTED", accepted)
                    self.assertIn("APPROVED", accepted)
                    grants = self.home.joinpath(".apexe/delegation")
                    self.assertEqual(list(grants.iterdir()) if grants.exists() else [], [])


if __name__ == "__main__":
    unittest.main()

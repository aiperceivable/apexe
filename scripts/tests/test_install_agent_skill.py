"""Exercise skill installation through the public script entry point."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


REPOSITORY = Path(__file__).resolve().parents[2]
INSTALLER = REPOSITORY / "scripts/install_agent_skill.py"
SOURCE = REPOSITORY / "skills/apexe/SKILL.md"


class InstallationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / "home"
        self.home.mkdir()
        self.project = self.root / "project"
        self.project.mkdir()

    def run_installer(self, *arguments: str) -> subprocess.CompletedProcess:
        environment = dict(os.environ, HOME=str(self.home))
        return subprocess.run(
            [sys.executable, str(INSTALLER), *arguments],
            cwd=self.project, env=environment, capture_output=True,
            text=True, timeout=10, check=False,
        )

    def test_all_agents_receive_identical_self_contained_skill(self) -> None:
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = SOURCE.read_bytes()
        files = sorted(self.project.rglob("SKILL.md"))
        self.assertEqual(len(files), 2)
        self.assertEqual({file.read_bytes() for file in files}, {expected})
        self.assertTrue((self.project / ".claude/skills/apexe/SKILL.md").is_file())
        self.assertTrue((self.project / ".agents/skills/apexe/SKILL.md").is_file())
        self.assertEqual(list(self.home.iterdir()), [])

    def test_individual_agent_installation_does_not_install_other_hosts(self) -> None:
        for agent, directory in [("claude", ".claude"), ("codex", ".agents"),
                                 ("pi", ".agents")]:
            with self.subTest(agent=agent):
                project = self.project / agent
                result = self.run_installer("--agent", agent, "--project-dir", str(project))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(list(project.rglob("SKILL.md"))), 1)
                self.assertEqual((project / directory / "skills/apexe/SKILL.md").read_bytes(),
                                 SOURCE.read_bytes())

    def test_user_scope_writes_only_the_isolated_home(self) -> None:
        result = self.run_installer("--scope", "user")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(list(self.home.rglob("SKILL.md"))), 2)
        self.assertEqual(list(self.project.iterdir()), [])

    def test_identical_reinstallation_does_not_touch_installed_files(self) -> None:
        self.assertEqual(self.run_installer().returncode, 0)
        files = list(self.project.rglob("SKILL.md"))
        timestamps = {file: file.stat().st_mtime_ns for file in files}
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(timestamps, {file: file.stat().st_mtime_ns for file in files})

    def test_conflict_prevents_writes_to_every_planned_destination(self) -> None:
        existing = self.project / ".agents/skills/apexe/SKILL.md"
        existing.parent.mkdir(parents=True)
        existing.write_text("User-maintained skill", encoding="utf-8")
        result = self.run_installer()
        self.assertEqual(result.returncode, 1)
        self.assertIn("--force", result.stderr)
        self.assertEqual(existing.read_text(), "User-maintained skill")
        self.assertFalse((self.project / ".claude").exists())

    def test_explicit_replacement_preserves_other_skill_files(self) -> None:
        directory = self.project / ".agents/skills/apexe"
        directory.mkdir(parents=True)
        (directory / "SKILL.md").write_text("Old skill", encoding="utf-8")
        reference = directory / "notes.md"
        reference.write_text("Personal notes", encoding="utf-8")
        result = self.run_installer("--agent", "codex", "--force")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((directory / "SKILL.md").read_bytes(), SOURCE.read_bytes())
        self.assertEqual(reference.read_text(), "Personal notes")

    def test_dry_run_does_not_create_a_project_or_skill_files(self) -> None:
        missing = self.root / "missing-project"
        result = self.run_installer("--project-dir", str(missing), "--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Would install:", result.stdout)
        self.assertFalse(missing.exists())

    @unittest.skipUnless(hasattr(os, "symlink"), "Symlinks are unavailable")
    def test_symlinked_directory_and_file_are_refused_even_with_force(self) -> None:
        outside = self.root / "outside"
        outside.mkdir()
        link = self.project / ".agents"
        link.symlink_to(outside, target_is_directory=True)
        result = self.run_installer("--agent", "pi", "--force")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(list(outside.iterdir()), [])
        link.unlink()
        directory = self.project / ".agents/skills/apexe"
        directory.mkdir(parents=True)
        original = outside / "original.md"
        original.write_text("Original", encoding="utf-8")
        (directory / "SKILL.md").symlink_to(original)
        result = self.run_installer("--agent", "codex", "--force")
        self.assertEqual(result.returncode, 1)
        self.assertEqual(original.read_text(), "Original")

    def test_user_scope_rejects_an_ambiguous_project_destination(self) -> None:
        result = self.run_installer("--scope", "user", "--project-dir", str(self.project))
        self.assertEqual(result.returncode, 2)
        self.assertEqual(list(self.home.iterdir()), [])
        self.assertEqual(list(self.project.iterdir()), [])


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Install the shared apexe Agent Skill from a source checkout."""

import argparse
from pathlib import Path
import tempfile


AGENT_DIRECTORIES = {
    "claude": Path(".claude/skills/apexe"),
    "codex": Path(".agents/skills/apexe"),
    "pi": Path(".agents/skills/apexe"),
}
SKILL_SOURCE = Path(__file__).resolve().parents[1] / "skills/apexe/SKILL.md"


def plan_installation(base: Path, agent: str, content: bytes,
                      force: bool) -> list[tuple[Path, bool]]:
    """Check all destinations before writing any skill file."""
    agents = AGENT_DIRECTORIES if agent == "all" else [agent]
    directories = dict.fromkeys(AGENT_DIRECTORIES[name] for name in agents)
    plan = []
    for relative in directories:
        directory = base
        for component in relative.parts:
            directory /= component
            if directory.is_symlink():
                raise ValueError(f"Refusing symlinked skill directory: {directory}")
            if directory.exists() and not directory.is_dir():
                raise ValueError(f"Skill directory is not a directory: {directory}")
        destination = directory / "SKILL.md"
        if destination.is_symlink():
            raise ValueError(f"Refusing symlinked skill file: {destination}")
        if destination.exists() and not destination.is_file():
            raise ValueError(f"Skill destination is not a file: {destination}")
        unchanged = destination.exists() and destination.read_bytes() == content
        if destination.exists() and not unchanged and not force:
            raise ValueError(f"Skill already exists: {destination}; use --force to replace it")
        plan.append((destination, unchanged))
    return plan


def write_skill(destination: Path, content: bytes) -> None:
    """Publish a complete skill file and clean up any failed temporary write."""
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as stream:
            temporary = Path(stream.name)
            stream.write(content)
        temporary.chmod(0o644)
        temporary.replace(destination)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--agent", choices=[*AGENT_DIRECTORIES, "all"], default="all",
                        help="Target agent; Codex and pi share the Agent Skills directory")
    parser.add_argument("--scope", choices=["project", "user"], default="project",
                        help="Install for one project (default) or the current user")
    parser.add_argument("--project-dir", type=Path,
                        help="Project root; defaults to the current directory")
    parser.add_argument("--force", action="store_true",
                        help="Replace a different existing SKILL.md; preserve other files")
    parser.add_argument("--dry-run", action="store_true",
                        help="Print planned destinations without writing files")
    args = parser.parse_args()
    if args.scope == "user" and args.project_dir is not None:
        parser.error("--project-dir requires --scope project")
    base = Path.home() if args.scope == "user" else (args.project_dir or Path.cwd())
    base = base.expanduser().resolve()
    try:
        content = SKILL_SOURCE.read_bytes()
        plan = plan_installation(base, args.agent, content, args.force)
        for destination, unchanged in plan:
            if unchanged:
                print(f"Unchanged: {destination}")
            elif args.dry_run:
                print(f"Would install: {destination}")
            else:
                write_skill(destination, content)
                print(f"Installed: {destination}")
    except (OSError, ValueError) as error:
        parser.exit(1, f"Error: {error}\n")


if __name__ == "__main__":
    main()

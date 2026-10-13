#!/usr/bin/env python3
"""Stage MkDocs sources while preserving repository-relative Markdown links."""

import argparse
import os
from pathlib import Path
import re
import shutil
from urllib.parse import quote, unquote, urlsplit


REPOSITORY_URL = "https://github.com/aiperceivable/apexe"
LINK = re.compile(r"(?P<prefix>\]\()(?P<target>[^\s)]+)")


def rewrite_links(text: str, source: Path, destination: Path,
                  root: Path, pages: dict[Path, Path]) -> str:
    """Rebase published pages and send repository-only files to GitHub."""
    def replace(match: re.Match) -> str:
        target = match["target"]
        parsed = urlsplit(target)
        if parsed.scheme or parsed.netloc or not parsed.path:
            return match[0]
        original = (source.parent / unquote(parsed.path)).resolve()
        if original.is_dir() and (original / "README.md").is_file():
            original /= "README.md"
        if original in pages:
            rewritten = quote(os.path.relpath(pages[original], destination.parent), safe="/.")
        else:
            relative = original.relative_to(root).as_posix()
            if not original.exists():
                raise ValueError(f"Missing documentation link in {source}: {target}")
            kind = "tree" if original.is_dir() else "blob"
            rewritten = f"{REPOSITORY_URL}/{kind}/main/{quote(relative, safe='/')}"
        if parsed.query:
            rewritten += "?" + parsed.query
        if parsed.fragment:
            rewritten += "#" + parsed.fragment
        return match["prefix"] + rewritten

    return LINK.sub(replace, text)


def prepare(root: Path, output: Path) -> None:
    """Write a standalone build tree without changing the repository sources."""
    docs = output / "docs"
    shutil.copytree(root / "docs", docs, dirs_exist_ok=True)
    pages = {source.resolve(): docs / source.relative_to(root / "docs")
             for source in (root / "docs").rglob("*") if source.is_file()}
    pages.update({source.resolve(): docs / source.relative_to(root)
                  for source in (root / "examples").rglob("*.md")})
    pages.update({root / "README.md": docs / "index.md",
                  root / "docs/README.md": docs / "documentation.md",
                  root / "CHANGELOG.md": docs / "changelog.md",
                  root / "examples/README.md": docs / "examples.md",
                  root / "apexe-logo.svg": docs / "assets/apexe-logo.svg"})
    # MkDocs excludes README.md beside index.md; publish the guide index explicitly.
    (docs / "README.md").unlink(missing_ok=True)
    for source, destination in pages.items():
        destination.parent.mkdir(parents=True, exist_ok=True)
        if source.suffix == ".md":
            text = source.read_text(encoding="utf-8")
            text = text.replace(
                "https://raw.githubusercontent.com/aiperceivable/apexe/main/apexe-logo.svg",
                "./assets/apexe-logo.svg",
            ) if source == root / "README.md" else text
            # The README logo is HTML, so it does not pass through the Markdown rewriter.
            text = text.replace('src="./apexe-logo.svg"', 'src="./assets/apexe-logo.svg"')
            destination.write_text(rewrite_links(text, source, destination, root, pages),
                                   encoding="utf-8")
        else:
            shutil.copyfile(source, destination)
    configuration = (root / "mkdocs.yml").read_text(encoding="utf-8")
    configuration += "\ndocs_dir: docs\nsite_dir: site\n"
    (output / "mkdocs.yml").write_text(configuration, encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    output = args.output.resolve()
    if output == root or root in output.parents or output in root.parents:
        parser.error("The build tree must be outside the repository")
    output.mkdir(parents=True, exist_ok=True)
    prepare(root, output)


if __name__ == "__main__":
    main()

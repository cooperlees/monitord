#!/usr/bin/env python3
"""Build the landing page by rendering README.md into the HTML template."""

import sys
from pathlib import Path

import markdown


def main() -> int:
    repo_root = Path(__file__).parent
    template_path = repo_root / "landing_page.html"
    readme_path = repo_root / "README.md"

    template = template_path.read_text(encoding="utf-8")
    readme_md = readme_path.read_text(encoding="utf-8")

    # Find the first top-level heading ("# ") and skip it plus the immediately
    # following paragraph (non-blank lines) and surrounding blank lines.
    lines = readme_md.splitlines()
    start = 0
    h1_index = None
    for i, line in enumerate(lines):
        if line.startswith("# "):
            h1_index = i
            break

    if h1_index is not None:
        idx = h1_index + 1
        n = len(lines)
        # Skip blank lines right after the H1
        while idx < n and lines[idx].strip() == "":
            idx += 1
        # Skip the tagline/paragraph lines (continuous non-blank lines)
        while idx < n and lines[idx].strip() != "":
            idx += 1
        # Skip any blank lines after the tagline/paragraph
        while idx < n and lines[idx].strip() == "":
            idx += 1
        start = idx

    readme_md = "\n".join(lines[start:])

    readme_html = markdown.markdown(
        readme_md,
        extensions=["fenced_code", "tables"],
    )

    placeholder = "<!-- README_CONTENT -->"
    count = template.count(placeholder)
    if count == 0:
        raise RuntimeError(
            f"Template {template_path} does not contain the expected placeholder {placeholder!r}"
        )
    if count > 1:
        raise RuntimeError(
            f"Template {template_path} contains the placeholder {placeholder!r} {count} times; expected exactly once"
        )

    output = template.replace(placeholder, readme_html)
    sys.stdout.write(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

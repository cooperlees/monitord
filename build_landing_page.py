#!/usr/bin/env python3
"""Build the landing page by rendering README.md into the HTML template."""

import sys
from pathlib import Path

import markdown


def main() -> int:
    repo_root = Path(__file__).parent
    template_path = repo_root / "landing_page.html"
    readme_path = repo_root / "README.md"

    template = template_path.read_text()
    readme_md = readme_path.read_text()

    # Skip the first h1 + tagline (already in the template header)
    lines = readme_md.splitlines()
    start = 0
    for i, line in enumerate(lines):
        if i == 0 and line.startswith("# "):
            continue
        if i == 1 and line.strip() == "":
            continue
        if i == 2 and "know how happy" in line:
            continue
        if i == 3 and line.strip() == "":
            start = i + 1
            break
        start = i
        break

    readme_md = "\n".join(lines[start:])

    readme_html = markdown.markdown(
        readme_md,
        extensions=["fenced_code", "tables"],
    )

    output = template.replace("<!-- README_CONTENT -->", readme_html)
    sys.stdout.write(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

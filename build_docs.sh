#!/bin/bash
set -euo pipefail

# Backup github CNAME file
cp -v docs/CNAME /tmp/

rm -rf target/doc

cargo doc --no-deps

# Setup Python venv for README rendering
python3 -m venv .venv --clear
.venv/bin/pip install --quiet markdown

# Build landing page with README embedded
.venv/bin/python build_landing_page.py > target/doc/index.html

# Ensure .nojekyll exists for GitHub Pages
touch target/doc/.nojekyll

# Restore github CNAME file
cp -v /tmp/CNAME target/doc/

rsync -av --delete target/doc/ docs/

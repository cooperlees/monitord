#!/bin/bash

# Backup github CNAME file
cp -v docs/CNAME /tmp/

rm -rf target/doc

cargo doc --no-deps

# Copy landing page into target/doc
cp -v landing_page.html target/doc/index.html
# Ensure .nojekyll exists for GitHub Pages
touch target/doc/.nojekyll

# Restore github CNAME file
cp -v /tmp/CNAME target/doc/

rsync -av --delete target/doc/ docs/

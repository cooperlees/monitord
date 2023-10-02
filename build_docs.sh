#!/bin/bash

# Backup github CNAME file
cp -v docs/CNAME /tmp/

rm -rf target/doc

cargo doc --no-deps

echo '<meta http-equiv="refresh" content="0; url=monitord">' > target/doc/index.html
# Restore github CNAME file
cp -v /tmp/CNAME target/doc/

rsync -av --delete target/doc/ docs/

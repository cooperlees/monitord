#!/bin/bash

rm -rf target/doc
cargo doc --no-deps
echo '<meta http-equiv="refresh" content="0; url=monitord">' > target/doc/index.html
rsync -av --delete target/doc/ docs/

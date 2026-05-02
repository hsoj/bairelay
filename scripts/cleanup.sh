#!/usr/bin/env bash
#
# cleanup.sh — wipe target/ and rebuild release. After this runs,
# target/ contains exactly the current release build's artifacts.

set -euo pipefail
cd "$(dirname "$0")/.."

before="(none)"
[[ -d target ]] && before=$(du -sh target 2>/dev/null | cut -f1)
echo "Before: target/ is $before"

cargo clean --quiet
cargo build --quiet
cargo build --quiet --release

after=$(du -sh target 2>/dev/null | cut -f1)
echo "After:  target/ is $after"

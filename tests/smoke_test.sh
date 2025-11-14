#!/usr/bin/env bash
set -e
cd "$(dirname "$0")/.."

echo "Running printree smoke test..."
cargo build --quiet

echo "== Tree =="
target/release/printree --max-depth 2

echo "== Tree (with gitignore) =="
target/release/printree --gitignore on --max-depth 2

echo "== Diff HEAD~1..HEAD =="
target/release/printree diff --rev-a HEAD~1 --rev-b HEAD || echo "(no git repo)"

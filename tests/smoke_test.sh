#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${PRINTREE_BIN:-target/release/printree}

if [[ ! -x "$BIN" ]]; then
  echo "printree binary not found at $BIN; build with cargo build --release first" >&2
  exit 1
fi

echo "Running printree smoke test..."

echo "== Tree =="
"$BIN" --max-depth 2

echo "== Tree (with gitignore) =="
"$BIN" --gitignore on --max-depth 2

echo "== Diff HEAD~1..HEAD =="
"$BIN" diff --rev-a HEAD~1 --rev-b HEAD || echo "(no git repo)"

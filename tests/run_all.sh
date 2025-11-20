#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${PRINTREE_BIN:-target/release/printree}

if [[ ! -x "$BIN" ]]; then
  echo "Building release binary at $BIN..."
  cargo build --release --locked
fi

echo "Using binary: $BIN"
PRINTREE_BIN="$BIN" tests/smoke_test.sh
PRINTREE_BIN="$BIN" tests/tree_test.sh
PRINTREE_BIN="$BIN" tests/diff_test.sh

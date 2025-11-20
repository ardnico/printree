#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

BIN=${PRINTREE_BIN:-target/release/printree}

if [[ ! -x "$BIN" ]]; then
  echo "printree binary not found at $BIN; build with cargo build --release first" >&2
  exit 1
fi

"$BIN" diff --rev-a HEAD~1 --rev-b HEAD || echo "not in a git repo"

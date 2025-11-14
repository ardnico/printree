#!/usr/bin/env bash
set -e
cd "$(dirname "$0")/.."

target/release/printree --include '**/*.rs' --match-mode path --color always --max-depth 3

#!/usr/bin/env bash
set -e
cd "$(dirname "$0")/.."

target/release/printree diff --rev-a HEAD~1 --rev-b HEAD || echo "not in a git repo"

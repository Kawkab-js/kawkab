#!/usr/bin/env bash
# Baseline checks for Node-compat work (local + CI).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
cargo test --workspace --features tokio-uring "$@"

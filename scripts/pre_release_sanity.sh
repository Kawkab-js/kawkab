#!/usr/bin/env bash
set -euo pipefail

echo "[pre-release] Running documentation consistency gate..."
./scripts/docs_consistency_check.sh

echo "[pre-release] Running rustfmt check..."
cargo fmt --all -- --check

echo "[pre-release] Running clippy workspace check..."
cargo clippy --workspace --all-targets --features tokio-uring

echo "[pre-release] OK: sanity checks passed."

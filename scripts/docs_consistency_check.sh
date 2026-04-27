#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOCS_DIR="$ROOT_DIR/docs"

echo "[docs-check] Running documentation consistency checks..."

echo "[docs-check] 1) Terminology drift check"
if rg -n "To reach" "$DOCS_DIR"; then
  echo "[docs-check] ERROR: Found legacy 'To reach' phrasing. Use 'Remaining vs Node v23'."
  exit 1
fi
if rg -n "Remaining vs Node" "$DOCS_DIR" | rg -v "Remaining vs Node v23"; then
  echo "[docs-check] ERROR: Found legacy 'Remaining vs Node' phrasing. Use 'Remaining vs Node v23'."
  exit 1
fi

echo "[docs-check] 2) Central index linkage check"
REQUIRED_FILES=(
  "NODE_COMPATIBILITY.md"
  "FEATURE_BASELINE.md"
  "COMPAT_DEFINITION_OF_DONE.md"
  "RELEASE_CHECKLIST.md"
  "COMPAT_KPI.md"
  "NPM_CORPUS.md"
  "PRODUCT_VISION.md"
  "NODE_NON_GOALS.md"
)

for file in "${REQUIRED_FILES[@]}"; do
  target="$DOCS_DIR/$file"
  if [[ ! -f "$target" ]]; then
    echo "[docs-check] ERROR: Missing required doc: $file"
    exit 1
  fi
  if ! rg -q "Docs index|Central docs index|INDEX\\.md" "$target"; then
    echo "[docs-check] ERROR: Missing INDEX.md link in $file"
    exit 1
  fi
done

echo "[docs-check] 3) Quick navigation presence check"
for file in "${REQUIRED_FILES[@]}"; do
  target="$DOCS_DIR/$file"
  if ! rg -q "^Quick navigation:" "$target"; then
    echo "[docs-check] ERROR: Missing 'Quick navigation:' block in $file"
    exit 1
  fi
done

echo "[docs-check] OK: documentation consistency checks passed."

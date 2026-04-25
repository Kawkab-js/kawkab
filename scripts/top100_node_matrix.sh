#!/usr/bin/env bash
# Validate Top 100 basket smoke scripts with Node.js (same fixtures as QuickJS matrix).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIST="$ROOT/docs/data/top100-packages.txt"
FIXTURE="$ROOT/fixtures/kpi/top100-qjs"
CHECK="$FIXTURE/check-one.cjs"

if [[ ! -f "$FIXTURE/node_modules/semver/package.json" ]]; then
  echo "top100_node_matrix: run npm install in $FIXTURE first" >&2
  exit 2
fi

command -v node >/dev/null 2>&1 || {
  echo "top100_node_matrix: node is required" >&2
  exit 2
}

failures=0
total=0
while IFS= read -r pkg || [[ -n "$pkg" ]]; do
  [[ "$pkg" =~ ^#.*$ ]] && continue
  [[ -z "${pkg// }" ]] && continue
  total=$((total + 1))
  printf '%s' "$pkg" >"$FIXTURE/.current-pkg"
  if ! out="$(cd "$FIXTURE" && node "$CHECK" 2>&1)"; then
    echo "FAIL $pkg (node)" >&2
    echo "$out" >&2
    failures=$((failures + 1))
    continue
  fi
  if ! node -e 'const j=JSON.parse(process.argv[1]); if(!j||j.ok!==true) process.exit(1);' "$out" 2>/dev/null; then
    echo "FAIL $pkg (bad json): $out" >&2
    failures=$((failures + 1))
  fi
done <"$LIST"

echo "top100_node_matrix: $((total - failures))/$total passed (Node)"
if [[ "$failures" -ne 0 ]]; then
  exit 1
fi

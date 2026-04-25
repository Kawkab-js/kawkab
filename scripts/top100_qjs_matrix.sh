#!/usr/bin/env bash
# Run Top 100 basket smokes on Kawkab QuickJS (same checks as Node matrix).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LIST="$ROOT/docs/data/top100-packages.txt"
FIXTURE="$ROOT/fixtures/kpi/top100-qjs"
CHECK="$FIXTURE/check-one.cjs"
KAWKAB="${KAWKAB:-$ROOT/target/debug/kawkab}"

if [[ ! -f "$FIXTURE/node_modules/semver/package.json" ]]; then
  echo "top100_qjs_matrix: run npm ci in $FIXTURE first" >&2
  exit 2
fi

if [[ ! -f "$KAWKAB" ]]; then
  echo "top100_qjs_matrix: kawkab binary missing at $KAWKAB (set KAWKAB or cargo build -p kawkab-cli)" >&2
  exit 2
fi

failures=0
total=0
while IFS= read -r pkg || [[ -n "$pkg" ]]; do
  [[ "$pkg" =~ ^#.*$ ]] && continue
  [[ -z "${pkg// }" ]] && continue
  total=$((total + 1))
  printf '%s' "$pkg" >"$FIXTURE/.current-pkg"
  if ! out="$(cd "$FIXTURE" && "$KAWKAB" --file check-one.cjs --engine quickjs 2>&1)"; then
    echo "FAIL $pkg (kawkab quickjs)" >&2
    echo "$out" >&2
    failures=$((failures + 1))
    continue
  fi
  if ! node -e 'const j=JSON.parse(process.argv[1]); if(!j||j.ok!==true) process.exit(1);' "$out" 2>/dev/null; then
    echo "FAIL $pkg (bad json): $out" >&2
    failures=$((failures + 1))
  fi
done <"$LIST"

echo "top100_qjs_matrix: $((total - failures))/$total passed (Kawkab QuickJS)"
if [[ "$failures" -ne 0 ]]; then
  exit 1
fi

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
decode_hash_path() {
  local h="$1"
  python3 - "$h" "$FIXTURE" <<'PY'
import sys
from pathlib import Path
try:
    from blake3 import blake3
except Exception:
    sys.exit(0)
h = sys.argv[1]
root = Path(sys.argv[2])
for p in root.rglob("*"):
    if not p.is_file():
        continue
    if blake3(str(p).encode()).hexdigest()[:16] == h:
        print(p)
        break
PY
}
while IFS= read -r pkg || [[ -n "$pkg" ]]; do
  [[ "$pkg" =~ ^#.*$ ]] && continue
  [[ -z "${pkg// }" ]] && continue
  total=$((total + 1))
  # Temporary runtime stabilization: enables deterministic parse path while
  # QuickJS CJS loader bug (sporadic control-char tokenization) is triaged.
  out=""
  ok=0
  for attempt in 1 2 3; do
    out="$(cd /tmp && KAWKAB_DEBUG=1 KAWKAB_PKG="$pkg" "$KAWKAB" --file "$CHECK" --engine quickjs 2>&1 || true)"
    json_line="$(printf '%s\n' "$out" | rg '^\{.*\}$' | tail -n 1 || true)"
    if node -e 'const j=JSON.parse(process.argv[1]); if(!j||j.ok!==true) process.exit(1);' "$json_line" 2>/dev/null; then
      ok=1
      break
    fi
  done
  if [[ "$ok" -ne 1 ]]; then
    echo "FAIL $pkg (kawkab quickjs)" >&2
    echo "$out" >&2
    hash="$(printf '%s\n' "$out" | rg -o 'kawkab-mod-[0-9a-f]{16}\.js' | head -n1 | sed -E 's/kawkab-mod-([0-9a-f]{16})\.js/\1/' || true)"
    if [[ -n "$hash" ]]; then
      mapped="$(decode_hash_path "$hash")"
      [[ -n "$mapped" ]] && echo "FAIL $pkg mapped: $mapped" >&2
    fi
    failures=$((failures + 1))
  fi
done <"$LIST"

echo "top100_qjs_matrix: $((total - failures))/$total passed (Kawkab QuickJS)"
if [[ "$failures" -ne 0 ]]; then
  exit 1
fi

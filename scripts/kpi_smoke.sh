#!/usr/bin/env bash
# KPI smoke: default = http minimal (QuickJS-safe). Express requires Node engine; see fixtures/kpi/README.md.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

KAWKAB="${KAWKAB:-$ROOT/target/debug/kawkab}"
MODE="${KAWKAB_KPI_MODE:-http}"

if [[ ! -x "$KAWKAB" ]]; then
  cargo build -p kawkab-cli
fi

case "$MODE" in
  http)
    FIXTURE="$ROOT/fixtures/kpi/http-minimal"
    EXPECT_BODY="kawkab_http_ok"
    ;;
  express)
    FIXTURE="$ROOT/fixtures/kpi/express-minimal"
    EXPECT_BODY="kawkab_express_ok"
    (cd "$FIXTURE" && npm install --silent --no-fund --no-audit)
    ;;
  *)
    echo "kpi_smoke: unknown KAWKAB_KPI_MODE=$MODE (use http|express)" >&2
    exit 2
    ;;
esac

log="$(mktemp)"
trap 'rm -f "$log"' EXIT

EXTRA=()
if [[ "$MODE" == "express" ]]; then
  if command -v node >/dev/null 2>&1; then
    EXTRA=(--engine node)
  else
    echo "kpi_smoke: express mode needs Node.js (set PATH or use KAWKAB_KPI_MODE=http)" >&2
    exit 2
  fi
fi

"$KAWKAB" "${EXTRA[@]}" --file "$FIXTURE/server.js" >"$log" 2>&1 &
pid=$!

port=""
for _ in $(seq 1 100); do
  if port=$(awk '/^listening /{print $2; exit}' "$log") && [[ -n "$port" ]]; then
    break
  fi
  sleep 0.1
done

if [[ -z "$port" ]]; then
  echo "kpi_smoke: timeout waiting for listening line" >&2
  cat "$log" >&2 || true
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  exit 1
fi

body="$(curl -fsS "http://127.0.0.1:${port}/" || true)"
kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true

if [[ "$body" != "$EXPECT_BODY" ]]; then
  echo "kpi_smoke: expected body $EXPECT_BODY, got: ${body:-<empty>}" >&2
  cat "$log" >&2 || true
  exit 1
fi

echo "kpi_smoke: ok ($MODE, port $port)"

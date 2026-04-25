#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

KAWKAB="${KAWKAB:-$ROOT/target/release/kawkab}"
ITERATIONS="${KAWKAB_PERF_ITERATIONS:-5}"
MAX_QJS_AVG_MS="${KAWKAB_PERF_MAX_QJS_AVG_MS:-250}"
MAX_AUTO_AVG_MS="${KAWKAB_PERF_MAX_AUTO_AVG_MS:-300}"
MAX_KPI_TOTAL_MS="${KAWKAB_PERF_MAX_KPI_TOTAL_MS:-1200}"

cargo build --release --features tokio-uring -p kawkab-cli >/dev/null

tmp_dir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT
export KAWKAB_CACHE_DIR="$tmp_dir/cache"
mkdir -p "$KAWKAB_CACHE_DIR"
export KAWKAB_SKIP_BYTECODE="${KAWKAB_SKIP_BYTECODE:-1}"

cat >"$tmp_dir/hello.js" <<'EOF'
console.log("perf_ok");
EOF

now_ms() {
  python3 - <<'PY'
import time
print(time.monotonic_ns() // 1_000_000)
PY
}

measure_avg_ms() {
  local engine="$1"
  local total=0
  local i=0
  for ((i = 1; i <= ITERATIONS; i++)); do
    local t0 t1 dt out
    t0="$(now_ms)"
    out="$("$KAWKAB" --engine "$engine" --file "$tmp_dir/hello.js")"
    t1="$(now_ms)"
    if (( t1 >= t0 )); then
      dt=$((t1 - t0))
    else
      # Guard against rare clock anomalies in constrained runtimes.
      dt=0
    fi
    if [[ "$out" != *"perf_ok"* ]]; then
      echo "runtime_perf_gate: engine=$engine produced bad output: $out" >&2
      exit 1
    fi
    total=$((total + dt))
  done
  echo $((total / ITERATIONS))
}

qjs_avg="$(measure_avg_ms quickjs)"
auto_avg="$(measure_avg_ms auto)"

if (( qjs_avg > MAX_QJS_AVG_MS )); then
  echo "runtime_perf_gate: quickjs avg regression (${qjs_avg}ms > ${MAX_QJS_AVG_MS}ms)" >&2
  exit 1
fi

if (( auto_avg > MAX_AUTO_AVG_MS )); then
  echo "runtime_perf_gate: auto avg regression (${auto_avg}ms > ${MAX_AUTO_AVG_MS}ms)" >&2
  exit 1
fi

kpi_line="$(
  timeout 45s env \
    KAWKAB="$KAWKAB" \
    KAWKAB_KPI_MODE=http \
    KAWKAB_KPI_INCLUDE_EXTENDED=0 \
    KAWKAB_KPI_MAX_TOTAL_MS="$MAX_KPI_TOTAL_MS" \
    "$ROOT/scripts/kpi_smoke.sh"
)"
kpi_total="$(printf '%s' "$kpi_line" | sed -n 's/.*total=\([0-9][0-9]*\)ms.*/\1/p')"
if [[ -z "$kpi_total" ]]; then
  echo "runtime_perf_gate: unable to parse KPI total from: $kpi_line" >&2
  exit 1
fi

echo "runtime_perf_gate: ok (qjs_avg=${qjs_avg}ms, auto_avg=${auto_avg}ms, kpi_total=${kpi_total}ms)"

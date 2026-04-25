#!/usr/bin/env bash
# KPI smoke runner for ecosystem scenarios.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

KAWKAB="${KAWKAB:-$ROOT/target/debug/kawkab}"
MODE="${KAWKAB_KPI_MODE:-http}"
MAX_STARTUP_MS="${KAWKAB_KPI_MAX_STARTUP_MS:-0}"
MAX_REQUEST_MS="${KAWKAB_KPI_MAX_REQUEST_MS:-0}"
MAX_TOTAL_MS="${KAWKAB_KPI_MAX_TOTAL_MS:-0}"
export KAWKAB_SKIP_BYTECODE="${KAWKAB_SKIP_BYTECODE:-1}"

now_ms() {
  python3 - <<'PY'
import time
print(time.monotonic_ns() // 1_000_000)
PY
}

if [[ ! -x "$KAWKAB" ]]; then
  cargo build -p kawkab-cli
fi

need_node_engine() {
  local mode="$1"
  [[ "$mode" == "express" || "$mode" == "express-json" || "$mode" == "express-static" || "$mode" == "top100-sample" ]]
}

mode_fixture() {
  local mode="$1"
  case "$mode" in
  http) echo "$ROOT/fixtures/kpi/http-minimal" ;;
  express) echo "$ROOT/fixtures/kpi/express-minimal" ;;
  express-json) echo "$ROOT/fixtures/kpi/express-json" ;;
  express-static) echo "$ROOT/fixtures/kpi/express-static" ;;
  top100-sample) echo "$ROOT/fixtures/kpi/top100-sample" ;;
  *) return 1 ;;
  esac
}

prepare_mode() {
  local mode="$1"
  local fixture
  fixture="$(mode_fixture "$mode")" || {
    echo "kpi_smoke: unknown mode=$mode" >&2
    exit 2
  }
  if need_node_engine "$mode"; then
    command -v node >/dev/null 2>&1 || {
      echo "kpi_smoke: mode=$mode requires Node.js" >&2
      exit 2
    }
    if [[ -f "$fixture/package.json" ]]; then
      (cd "$fixture" && npm install --silent --no-fund --no-audit)
    fi
  fi
}

run_server_mode() {
  local mode="$1"
  local fixture
  fixture="$(mode_fixture "$mode")"
  local extra=()
  if need_node_engine "$mode"; then
    extra=(--engine node)
  fi

  local log
  log="$(mktemp)"

  local request_path="/"
  local request_method="GET"
  local request_data=""
  local expect=""

  case "$mode" in
  http) expect="kawkab_http_ok" ;;
  express) expect="kawkab_express_ok" ;;
  express-json)
    request_path="/echo"
    request_method="POST"
    request_data='{"msg":"ok","n":1}'
    expect='{"ok":true,"body":{"msg":"ok","n":1}}'
    ;;
  express-static)
    request_path="/file.txt"
    expect="kawkab_static_ok"
    ;;
  *)
    echo "kpi_smoke: server mode mismatch: $mode" >&2
    exit 2
    ;;
  esac

  local start_ms ready_ms req_start_ms req_end_ms end_ms
  start_ms="$(now_ms)"
  local forced_port=""
  local pid=""
  local launched=0
  for _attempt in $(seq 1 5); do
    forced_port="$(( (RANDOM % 20000) + 30000 ))"
    KPI_PORT="$forced_port" "$KAWKAB" "${extra[@]}" --file "$fixture/server.js" >"$log" 2>&1 &
    pid=$!
    launched=1

    local port=""
    port="$forced_port"
    for _ in $(seq 1 120); do
      if curl -s -o /dev/null "http://127.0.0.1:${port}/"; then
        break
      fi
      if ! kill -0 "$pid" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    ready_ms="$(now_ms)"
    if [[ -n "$port" ]] && curl -s -o /dev/null "http://127.0.0.1:${port}/"; then
      break
    fi
    kill "$pid" 2>/dev/null || true
    for _ in $(seq 1 20); do
      if ! kill -0 "$pid" 2>/dev/null; then
        break
      fi
      sleep 0.05
    done
    kill -9 "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    if ! rg -q "EADDRINUSE|address already in use" "$log"; then
      echo "kpi_smoke: timeout waiting for listening line (mode=$mode)" >&2
      cat "$log" >&2 || true
      rm -f "$log"
      exit 1
    fi
    : >"$log"
    launched=0
  done
  if [[ "$launched" -ne 1 ]]; then
    echo "kpi_smoke: failed to launch server after port retries (mode=$mode)" >&2
    rm -f "$log"
    exit 1
  fi

  req_start_ms="$(now_ms)"
  local body=""
  if [[ "$request_method" == "POST" ]]; then
    body="$(curl -fsS -X POST -H "content-type: application/json" --data "$request_data" "http://127.0.0.1:${port}${request_path}" || true)"
  else
    body="$(curl -fsS "http://127.0.0.1:${port}${request_path}" || true)"
  fi
  req_end_ms="$(now_ms)"

  kill "$pid" 2>/dev/null || true
  for _ in $(seq 1 20); do
    if ! kill -0 "$pid" 2>/dev/null; then
      break
    fi
    sleep 0.05
  done
  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  end_ms="$(now_ms)"

  local startup_ms request_ms total_ms
  startup_ms=$((ready_ms - start_ms))
  request_ms=$((req_end_ms - req_start_ms))
  total_ms=$((end_ms - start_ms))

  local body_norm expect_norm
  body_norm="$(printf '%s' "$body" | tr -d '\r' | sed -E 's/[[:space:]]+$//')"
  expect_norm="$(printf '%s' "$expect" | tr -d '\r' | sed -E 's/[[:space:]]+$//')"
  if [[ "$body_norm" != "$expect_norm" ]]; then
    echo "kpi_smoke: mode=$mode expected '$expect', got '${body:-<empty>}'" >&2
    cat "$log" >&2 || true
    rm -f "$log"
    exit 1
  fi
  if (( MAX_STARTUP_MS > 0 && startup_ms > MAX_STARTUP_MS )); then
    echo "kpi_smoke: mode=$mode startup regression ($startup_ms ms > ${MAX_STARTUP_MS} ms)" >&2
    rm -f "$log"
    exit 1
  fi
  if (( MAX_REQUEST_MS > 0 && request_ms > MAX_REQUEST_MS )); then
    echo "kpi_smoke: mode=$mode request regression ($request_ms ms > ${MAX_REQUEST_MS} ms)" >&2
    rm -f "$log"
    exit 1
  fi
  if (( MAX_TOTAL_MS > 0 && total_ms > MAX_TOTAL_MS )); then
    echo "kpi_smoke: mode=$mode total regression ($total_ms ms > ${MAX_TOTAL_MS} ms)" >&2
    rm -f "$log"
    exit 1
  fi
  rm -f "$log"
  echo "kpi_smoke: ok ($mode, startup=${startup_ms}ms, request=${request_ms}ms, total=${total_ms}ms)"
}

run_script_mode() {
  local mode="$1"
  local fixture
  fixture="$(mode_fixture "$mode")"
  local extra=()
  if need_node_engine "$mode"; then
    extra=(--engine node)
  fi
  local out
  out="$("$KAWKAB" "${extra[@]}" --file "$fixture/check.js")"
  if [[ "$out" != '{"chunks":[[1,2],[3,4]],"valid":"1.0.0"}' ]]; then
    echo "kpi_smoke: mode=$mode output mismatch: $out" >&2
    exit 1
  fi
  echo "kpi_smoke: ok ($mode)"
}

run_mode() {
  local mode="$1"
  prepare_mode "$mode"
  if [[ "$mode" == "top100-sample" ]]; then
    run_script_mode "$mode"
  else
    run_server_mode "$mode"
  fi
}

if [[ "$MODE" == "all" ]]; then
  run_mode http
  run_mode express
  if [[ "${KAWKAB_KPI_INCLUDE_EXTENDED:-0}" == "1" ]]; then
    run_mode express-json
    run_mode express-static
    run_mode top100-sample
  else
    echo "kpi_smoke: skip extended modes in all (set KAWKAB_KPI_INCLUDE_EXTENDED=1)"
  fi
else
  run_mode "$MODE"
fi

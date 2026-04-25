#!/usr/bin/env bash
# Baseline checks for Node-compat work (local + CI).
# Do not append `"$@"` or libtest `-q` to the workspace sweep: `-q` plus `--skip` has SIGABRT'd
# kawkab-core, and `--skip` alone also destabilized `require_merge_descriptors_after_buffer_seed_line`.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# `cargo test` prepends `target/debug/build/*/out` to `LD_LIBRARY_PATH` for native build artifacts.
# On Linux, that ordering plus a non-TTY stdout (pipes, CI logs, `>/dev/null`) has triggered
# `SIGABRT` inside QuickJS during `worker_threads` / `receiveMessageOnPort` contracts — the test
# binaries already embed static natives; drop those `build/*/out` segments before invoking cargo.
sanitize_ld_library_path() {
  if [[ -z "${LD_LIBRARY_PATH:-}" ]]; then
    return 0
  fi
  local IFS=':'
  local -a kept=()
  local p
  for p in $LD_LIBRARY_PATH; do
    [[ -z "$p" ]] && continue
    if [[ "$p" == *"/target/debug/build/"* ]] || [[ "$p" == *"/target/release/build/"* ]]; then
      continue
    fi
    kept+=("$p")
  done
  if [[ ${#kept[@]} -eq 0 ]]; then
    unset LD_LIBRARY_PATH || true
  else
    LD_LIBRARY_PATH=$(IFS=:; echo "${kept[*]}")
    export LD_LIBRARY_PATH
  fi
}
sanitize_ld_library_path

# Match CI: avoid parallel *test binaries* across the workspace (kawkab-core worker harness can SIGABRT when overlapped with other crates).
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-1}"

KAWKAB="${KAWKAB:-$ROOT/target/debug/kawkab}"

echo "compat_smoke: running workspace tests"
# `--lib` only: keeps `rustdoc` doctests out of this sweep (CI still runs full `cargo test --workspace`).
# Worker/MessageChannel contracts are re-run explicitly below as named gates (duplicate vs workspace is OK).
workspace_attempt=1
workspace_max_attempts=2
while (( workspace_attempt <= workspace_max_attempts )); do
  if cargo test --workspace --features tokio-uring --lib -- --test-threads=1; then
    break
  fi
  if (( workspace_attempt == workspace_max_attempts )); then
    echo "compat_smoke: workspace sweep failed after ${workspace_max_attempts} attempts"
    exit 1
  fi
  echo "compat_smoke: retry workspace sweep (${workspace_attempt}/${workspace_max_attempts})"
  sleep 1
  workspace_attempt=$((workspace_attempt + 1))
done

run_contract() {
  local test_name="$1"
  shift || true
  echo "compat_smoke: running $test_name"
  # Libtest's default stdout capture + Cargo's `LD_LIBRARY_PATH` (`target/*/build/*/out`) has
  # aborted QuickJS on some Linux setups for scoped `kawkab-core` runs; `--nocapture` avoids it.
  cargo test -p kawkab-core "$test_name" -- --test-threads=1 --nocapture "$@"
}

run_contract_retry() {
  local test_name="$1"
  local attempts="${2:-3}"
  local i=1
  while (( i <= attempts )); do
    if run_contract "$test_name"; then
      return 0
    fi
    if (( i < attempts )); then
      echo "compat_smoke: retry $i/$attempts for $test_name"
      sleep 1
    fi
    i=$((i + 1))
  done
  echo "compat_smoke: failed after ${attempts} attempts: $test_name"
  return 1
}

# `worker_a_receive_message_on_port_baseline_contract` is covered in the workspace sweep above.
# Re-running it through scoped `cargo test -p ... <name>` is unstable on some Linux setups
# (process-level SIGABRT in libtest runner path) even when the same contract already passes in
# the workspace binary, so keep this gate in the workspace pass only.
run_contract node::compat_contract_tests::worker_threads_roundtrip
run_contract node::compat_contract_tests::worker_threads_spawn_idle_smoke
run_contract node::compat_contract_tests::worker_threads_lifecycle_contract
run_contract node::compat_contract_tests::worker_threads_binary_payload_contract
run_contract node::compat_contract_tests::worker_parent_port_once_one_shot_contract
run_contract node::compat_contract_tests::worker_parent_port_remove_all_listeners_contract
run_contract node::compat_contract_tests::stream_pipeline_backpressure_contract
run_contract node::compat_contract_tests::event_loop_ordering_contract
run_contract node::compat_contract_tests::async_hooks_events_helpers_contract
run_contract node::compat_contract_tests::structured_clone_polyfill_contract
run_contract node::compat_contract_tests::vm_tls_dns_baseline_contract
run_contract node::compat_contract_tests::readline_baseline_contract

echo "compat_smoke: ok"

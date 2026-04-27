# Release checklist

Use this before tagging a release or publishing binaries. Adjust version numbers and artifacts to your release process.

Quick navigation:
- [Preconditions](#preconditions)
- [Build and tests](#build-and-tests)
- [Smoke tests](#smoke-tests)
- [Performance gate](#performance-gate)
- [Security and defaults](#security-and-defaults)
- [Artifacts and communication](#artifacts-and-communication)

## Preconditions

- All changes intended for the release are merged on the release branch.
- `docs/NODE_COMPATIBILITY.md` and `docs/FEATURE_BASELINE.md` match the code being released.

## Build and tests

- On Linux (or CI): `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --features tokio-uring`
- `RUST_TEST_THREADS=1 cargo test --workspace --features tokio-uring` (matches CI `env`; avoids workspace-wide parallel test-binary overlap that can `SIGABRT` on `worker_threads` harness tests)
- `cargo build --release --features tokio-uring`

## Smoke tests

- Run runtime smoke suite: `./scripts/compat_smoke.sh`
- Confirm behavior contracts pass inside smoke (`event_loop_ordering_contract`, `worker_threads_lifecycle_contract`, `worker_parent_port_once_one_shot_contract`, `worker_parent_port_remove_all_listeners_contract`).
- Confirm web/http compatibility contract passes (`web_platform_http_surface_contract`) as part of the workspace sweep in smoke.
- Confirm local behavior contract pass (`stream_pipeline_backpressure_contract`).
- Verify ignored contract list is unchanged and documented (`http_client_local_behavior_contract`, `worker_a_receive_message_on_port_baseline_contract`, `worker_threads_worker_isolate_flags_contract`).
- Execute ignored HTTP local behavior contract explicitly and record result in release notes: `cargo test -p kawkab-core compat_contract_tests::http_client_local_behavior_contract -- --ignored --nocapture` (must not be treated as pass-by-default just because it is `#[ignore]` in normal sweeps).
- Run KPI smoke suite: `./scripts/kpi_smoke.sh`
- Run a minimal script: `./target/release/kawkab --file` on a small `.js` file (see root `README.md`).
- If PM changes: run `kawkab install` / `kawkab run` on a tiny fixture project.

## Performance gate

- Run performance gate with thresholds: `./scripts/runtime_perf_gate.sh`
- If thresholds are tuned, record env overrides used in release notes (`KAWKAB_PERF_MAX_*` and `KAWKAB_KPI_MAX_*`).

## Security and defaults

- Confirm host-risk features (e.g. `child_process`) remain opt-in and documented.

## Artifacts and communication

- Note breaking changes in changelog or release notes.
- Attach or publish the `kawkab` binary for intended platforms (currently Linux-focused).
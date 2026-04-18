# Release checklist

Use this before tagging a release or publishing binaries. Adjust version numbers and artifacts to your release process.

## Preconditions

- [ ] All changes intended for the release are merged on the release branch.
- [ ] `docs/NODE_COMPATIBILITY.md` and `docs/FEATURE_BASELINE.md` match the code being released.

## Build and tests

- [ ] On Linux (or CI): `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --features tokio-uring`
- [ ] `cargo test --workspace --features tokio-uring`
- [ ] `cargo build --release --features tokio-uring`

## Smoke tests

- [ ] Run a minimal script: `./target/release/kawkab --file` on a small `.js` file (see root `README.md`).
- [ ] If PM changes: run `kawkab install` / `kawkab run` on a tiny fixture project.

## Security and defaults

- [ ] Confirm host-risk features (e.g. `child_process`) remain opt-in and documented.

## Artifacts and communication

- [ ] Note breaking changes in changelog or release notes.
- [ ] Attach or publish the `kawkab` binary for intended platforms (currently Linux-focused).

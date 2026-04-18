# Feature baseline

This file is the **contractual baseline** for what the repository aims to ship. When behavior changes materially, update this document in the same change as the code and refresh `docs/NODE_COMPATIBILITY.md` where relevant.

## Workspace layout

- **`kawkab-core`** (directory `core/`): QuickJS-backed runtime, Node-style bootstrap, module loading, transpilation (SWC), bytecode cache, HTTP/IO integration surface.
- **`bridge`**: Console bridge install hook and flush helpers for the CLI path.
- **`io`**: I/O primitives; optional **`tokio-uring`** feature for Linux.
- **`snapshot`**: Experimental snapshot manifest metadata (not a full VM snapshot image).
- **`kawkab-cli`** (directory `kawkab/`): `kawkab` binary — script execution, engine modes (`auto` / QuickJS / Node fallback), package-manager subcommands.
- **`pm`**: Native package manager library used by the CLI (`package.json`, lockfile, registry install, `kawkab run`, `why`, `doctor`).

## Build and platform

- Primary target: **Linux** (including WSL2). Workspace builds with `--features tokio-uring` on supported kernels.
- QuickJS via **`hirofa-quickjs-sys`** (workspace version); compatibility layer in `core/src/qjs_compat.rs`.

## Runtime capabilities (summary)

- Execute JS/TS/JSX/TSX via transpilation; CommonJS `require` and ESM entrypoints as documented in `docs/NODE_COMPATIBILITY.md`.
- Node compatibility is **best-effort**; built-ins and globals are a curated subset — see the compatibility matrix, not npm “works everywhere”.
- **Security:** `child_process` and similar host capabilities are policy-gated (e.g. `KAWKAB_ALLOW_CHILD_PROCESS`); default is restrictive.

## Package manager (CLI)

- Reads/writes `package.json` dependency sections; maintains `kawkab.lock` with resolved URLs and integrity.
- Registry fetch and tarball cache under OS cache (`.../kawkab/packages`).
- Workspace selectors: `workspace:*`, `workspace:^`, `workspace:~`.
- Diagnostics: `kawkab why` and `kawkab doctor` support human and JSON output; schema/fixture policy in `pm/fixtures/README.md`.

## Native vector API

- `kawkab.vec.*` typed-array helpers (Rust offload); legacy `kawkab.fast*` retained for compatibility — see root `README.md`.

## Documentation pairing

- **Compatibility detail:** `docs/NODE_COMPATIBILITY.md`
- **Release process:** `docs/RELEASE_CHECKLIST.md`

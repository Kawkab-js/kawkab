# Feature baseline

This file is the **contractual baseline** for what the repository aims to ship. When behavior changes materially, update this document in the same change as the code and refresh `docs/NODE_COMPATIBILITY.md` where relevant. Status semantics: [`docs/COMPAT_DEFINITION_OF_DONE.md`](COMPAT_DEFINITION_OF_DONE.md). Product non-goals: [`docs/NODE_NON_GOALS.md`](NODE_NON_GOALS.md). Reference scenarios: [`docs/NPM_CORPUS.md`](NPM_CORPUS.md).

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

- Execute JS/TS/JSX/TSX via transpilation; CommonJS `require` and ESM entrypoints as documented in `docs/NODE_COMPATIBILITY.md`. Module resolution includes bare-specifier package/subpath splitting, `package.json` `exports` / `imports` (conditional and pattern subset), `require('module').createRequire`, and **`process.env.NODE_ENV`** driving `development` / `production` export conditions during resolution.
- Expanded built-in surface (still best-effort vs Node): `fs` (`copyFileSync`, `rmSync`), `path` (`relative`, `parse`), `zlib` (`deflateSync`, `inflateSync`), `punycode` IDNA (`toASCII` / `toUnicode` via `idna`), `crypto` hashes (`md5`, `blake3`), `http` / `https` blocking client (`get`, `request` via reqwest; servers remain simplified; HTTPS server is not TLS-terminated in-process).
- **`Isolate::eval`** passes an exact byte length for the `CString` script buffer (excluding the implicit NUL) to match QuickJS `JS_Eval` expectations and avoid out-of-bounds reads.
- Priority built-in contract: `node::compat_contract_tests::priority_builtins_green_contract` in `core/src/node/compat_contract_tests.rs` (requires network for `example.com` HTTP/HTTPS checks).
- Repeatable checks: `cargo test --workspace --features tokio-uring` or [`scripts/compat_smoke.sh`](../scripts/compat_smoke.sh).
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

- **Product vision (positioning, themes):** `docs/PRODUCT_VISION.md`
- **Compatibility detail:** `docs/NODE_COMPATIBILITY.md`
- **Definition of Done (🟢/🟡):** `docs/COMPAT_DEFINITION_OF_DONE.md`
- **Non-goals / 🔴 stance:** `docs/NODE_NON_GOALS.md`
- **Corpus / smoke scenarios:** `docs/NPM_CORPUS.md`
- **Release process:** `docs/RELEASE_CHECKLIST.md`

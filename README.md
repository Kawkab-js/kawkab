# Kawkab Workspace

A Rust JavaScript runtime project organized as a multi-crate workspace.

Current state of this repository:
- Builds successfully with `tokio-uring`.
- `kawkab-cli` now uses a QuickJS runtime with a Node-style bootstrap layer.
- QuickJS FFI integration is now migrated to `hirofa-quickjs-sys 0.14.0` via a compatibility adapter (`core/src/qjs_compat.rs`).
- CommonJS loading and several Node-like built-ins are available (see compatibility table below).
- Feature baseline source-of-truth: `docs/FEATURE_BASELINE.md`.
- Product positioning (sweet spots, non-goals, engineering themes): `docs/PRODUCT_VISION.md`.

## Project Structure

- `core`: Rust package **`kawkab-core`** — core runtime logic (QuickJS integration, Node-style bootstrap, loaders, transpiler).
- `bridge`: Integration/bridge layer (console bridge install hook + explicit flush helpers).
- `io`: I/O layer, with optional `tokio-uring` feature support and async file driver baseline.
- `snapshot`: Experimental snapshot manifest writer (`snapshot/src/lib.rs`) with validated context/error reporting.
- `pm`: Native package manager library (`package.json`, lockfile, install, `why` / `doctor` helpers) consumed by the CLI.
- `kawkab`: Executable crate (`kawkab-cli`) that produces the `kawkab` binary.

## Requirements

- Linux / WSL2 (recommended for this setup).
- Rust + Cargo (recent stable toolchain).
- If you are on Windows, run commands inside Ubuntu/WSL (not PowerShell).

## Linux Setup (Ubuntu / WSL2)

Install everything needed to build and run this project:

```bash
sudo apt update && sudo apt upgrade -y
sudo apt install -y build-essential pkg-config libssl-dev clang cmake curl git zip
```

Install Rust (stable toolchain) via `rustup`:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup default stable
rustup update
```

Optional checks:

```bash
rustc --version
cargo --version
```

Notes:
- Run all commands inside Linux/WSL terminal.
- `tokio-uring` requires Linux kernel support (works on modern Ubuntu/WSL2 kernels).

## Build

From the project root:

```bash
cargo build --release --features tokio-uring -q
```

Generated binary:

```bash
./target/release/kawkab
```

## Run a JS File

### 1) Create a test file

```bash
cat > test.js <<'EOF'
console.log("hello from kawkab test");
EOF
```

### 2) Run it

```bash
./target/release/kawkab --file test.js
```

Expected output:

```text
hello from kawkab test
```

## CLI Usage

Current syntax:

```bash
kawkab --file <path-to-script.(js|jsx|ts|tsx)> [--engine auto|quickjs|node] [--verbose]
```

Also supported:

```bash
kawkab -f <path-to-script.(js|jsx|ts|tsx)> [--engine auto|quickjs|node] [-v]
```

Package manager subcommands:

```bash
kawkab install
kawkab i
kawkab add <name[@range]> [--dev|--peer|--optional]
kawkab remove <name>
kawkab update [--minor|--patch]
kawkab run <script> [args...]
kawkab outdated
kawkab why <name> [--json] [--pretty=false] [--json-schema]
kawkab doctor [--json] [--pretty=false]
kawkab init [--yes|-y] [--force] [--entry <file>]
```

Engine modes:
- `auto` (default): tries QuickJS first, then falls back to Node.js if QuickJS fails.
- `quickjs`: forces QuickJS only (no fallback).
- `node`: runs directly with Node.js engine.
- `--verbose` / `-v`: prints auto-mode fallback details when switching from QuickJS to Node.js.

## Native Package Manager (kawkab pm)

- `kawkab` can now read/write `package.json` dependency sections:
  - `dependencies`, `devDependencies`, `peerDependencies`, `optionalDependencies`
- Install/update operations generate and maintain `kawkab.lock` with:
  - resolved URL
  - integrity value
  - deterministic package list ordering
- Registry layer downloads package metadata/tarballs from npm registry and caches tarballs locally under OS cache directory (`.../kawkab/packages`).
- Installer materializes `node_modules` and `.bin` links, and links local workspace packages when `workspaces` is configured.
- `kawkab run` executes scripts from `package.json` with `node_modules/.bin` injected into `PATH`.
- `kawkab why` supports:
  - text tree diagnostics (required-by path + peer requirements + peer conflicts)
  - JSON diagnostics via `--json`
  - compact JSON for CI via `--json --pretty=false`
  - schema contract output via `--json-schema`
- `kawkab doctor` supports:
  - human-readable health checks for PM environment
  - JSON diagnostics for automation (`--json --pretty=false`)
- Diagnostics contract fixtures and update policy are documented in:
  - `pm/fixtures/README.md`
- Workspace dependency selectors are recognized during resolution:
  - `workspace:*`, `workspace:^`, `workspace:~`

## TypeScript / JSX Support

- Kawkab transpiles `.ts`, `.tsx`, and `.jsx` sources directly using SWC (Rust).
- No separate `tsc` or Babel step is required for runtime execution.
- `require()` module resolution currently supports:
  - `.js`, `.json`, `.ts`, `.tsx`, `.jsx`
  - `index.js`, `index.json`, `index.ts`, `index.tsx`, `index.jsx`

## Kawkab Vector API (Native Rust Offload)

Kawkab exposes a native data-path API for typed numeric workloads:

```js
kawkab.vec.u32.sum(u32Array)
kawkab.vec.u32.map(u32Array, mul, add)
kawkab.vec.u32.filter(u32Array, min)

kawkab.vec.f64.sum(f64Array)
kawkab.vec.f64.map(f64Array, mul, add)
kawkab.vec.f64.filter(f64Array, min)
```

Notes:
- These operations execute in native Rust, not JS loops.
- API returns typed arrays for `map` / `filter`.
- Legacy `kawkab.fast*` functions remain available for backward compatibility, but `kawkab.vec.*` is the recommended interface for new code.

## Node Compatibility Snapshot

Detailed module/global status matrix:
- `docs/NODE_COMPATIBILITY.md`

### Supported now
- Full script evaluation through QuickJS (`functions`, `loops`, `expressions`, etc.).
- Global shims: `global`, `globalThis`, `process`, `Buffer`.
- Timers: `setTimeout`, `clearTimeout`, `setInterval`, `clearInterval`, `setImmediate`, `clearImmediate`.
- Microtask API: `queueMicrotask` and `process.nextTick` (callback + args).
- **ESM:** QuickJS native module loader can run ESM entrypoints and imports when the file is ESM (alongside CJS `require`); see `core/src/node/esm_loader.rs` and `kawkab` entry routing.
- CommonJS loader:
  - `require("./module")`
  - package main resolution (`package.json` -> `main` / exports subset via `resolve_module_path`)
  - `.js`, `.json`, `index.js`, `index.json`
- Built-ins (current subset; authoritative list: `docs/NODE_COMPATIBILITY.md` + `js_require` in `core/src/node/mod.rs`):
  - `assert`: native `require('assert')` (`AssertionError`, `deepEqual`/`deepStrictEqual`, real `==` for `equal`/`notEqual`, `throws`, `match`, `rejects`, …)
  - `process`: **global only** (no built-in `require('process')` override); `argv`, `env`, `version`, `versions`, `uptime()`, `hrtime()` / `hrtime.bigint()`, stdio hooks, etc.
  - `fs`: sync APIs (`readFileSync`, `writeFileSync`, `existsSync`, `mkdirSync` with `{ recursive: true }`, `readdirSync`, `unlinkSync`, `rmdirSync`, `statSync`, …) plus **`fs.promises.readFile` / `writeFile`**
  - `path`: `join`, `dirname`, `basename`, `extname`, `resolve`, `normalize`, `sep`, `delimiter`
  - `punycode`: `decode`/`encode`/`toASCII`/`toUnicode` — ASCII-only identity baseline; inputs with `xn--` rejected for APIs that need real Punycode decoding
  - `os`: `platform`, `tmpdir`, `homedir`
  - `events`: `EventEmitter` with `on`, `off`, `emit` (listener add/remove/dispatch)
  - `util`: `inspect`, `types.isDate` (uses QuickJS `Date` class identity); `sys` is the same subset (legacy alias)
  - `Buffer`: global constructor from `core/src/node/buffer.rs` (`Uint8Array` subclass + native helpers); `require('buffer')` re-exports the same global
  - `child_process`: `execSync`, `spawnSync` (policy-gated; disabled by default)
  - Compatibility-focused baseline behavior added for: `stream`, `url`, `punycode`, `querystring`, `string_decoder`, `crypto` (hash/hmac + `randomBytes`; see NODE_COMPATIBILITY), `dgram`, `diagnostics_channel`, `dns` (full callback + `dns/promises` surface), `tls`, `vm`, `worker_threads`, `timers` (and `timers/promises` subset), `perf_hooks`, `node:test`
  - Web-style globals at bootstrap (when missing): `atob`, `btoa`, minimal `performance` (`now`, `timeOrigin`), JSON-subset `structuredClone`; see `install_web_compat_globals` in `core/src/node/mod.rs`
  - `http`: TCP-backed `createServer` / `listen` / `close` with parsed `req.method`, `req.url`, `req.headers`, `req.body` (string), `req.httpVersion`, plus `res.statusCode`, `res.setHeader`, `res.writeHead`, `res.end` (keeps listening until `server.close()`)
  - `https`: same `createServer` shim as `http` (no TLS; packages that only `require('https')` can load)
  - `net`: same `createServer` entrypoint as `http` for compatibility-style usage

### Partially supported / intentionally simplified
- Timer behavior is synchronous/blocking for now (compatibility API first, performance model later).
- Microtask/timer scheduling is currently synchronous (not a full Node event loop yet).
- `Buffer` is a compatibility layer (typed-array-backed), not full Node v23 `buffer` parity.
- **`console` (CLI):** only `console.log` is installed on the default `kawkab` path; `error`/`warn`/etc. are not. There is no `require('console')`. See `docs/NODE_COMPATIBILITY.md`.
- `events` / `util` compatibility covers common baseline behavior but is not yet full Node parity.
- Compatibility modules are now split between:
  - behavior-ready baseline paths (`stream`, `url`, `punycode`, `querystring`, `string_decoder`, `crypto`, `dgram`, `diagnostics_channel`, `dns`, `worker_threads`, `vm`, `timers`/`timers/promises`, `perf_hooks`, `node:test`, plus global `atob`/`btoa`/`performance`/`structuredClone`)
  - `https` is loadable and mirrors the `http` `createServer` shim only (no TLS); real HTTPS belongs on the `tls` parity track.
  - API-shape-first paths that still need deeper parity hardening (`tls` and advanced Node internals).
- `querystring` now includes repeated-key array handling and URL-style encode/decode for baseline compatibility flows.
- Current `crypto` digests are deterministic compatibility outputs for integration checks, not cryptographic-grade parity yet.
- Current `worker_threads` emulation supports in-runtime lifecycle callbacks (`on`/`postMessage`/`terminate`) but is not a real multi-thread runtime yet.
- `http`/`net` are not full Node `IncomingMessage`/`ServerResponse` stacks: no streaming body, chunked transfer, or WebSockets; use `server.close()` to stop the accept loop and exit the process for short scripts.
- `snapshot` crate currently writes an experimental manifest format, not a full VM snapshot image.
- `bridge` currently exposes baseline console bridge hooks only (not a complete host logging pipeline).

### Not yet supported
- Full Node core module coverage and byte-for-byte API parity.
- Native addons (`*.node` binaries).
- Complete event loop semantics matching modern Node behavior.

## Useful Commands

QuickJS entry scripts are cached under `~/.cache/kawkab/bytecode` (or `KAWKAB_CACHE_DIR`). The cache key includes the transpiled source fingerprint; if you see odd `SyntaxError` after upgrading the runtime, remove that directory once.

Clean rebuild:

```bash
cargo clean
cargo build --release --features tokio-uring -q
```

Run via Cargo:

```bash
cargo run --release --features tokio-uring -p kawkab-cli -- --file path/to/script.js
```

## Security Policy (Host Capabilities)

- Child process execution is disabled by default.
- To enable `child_process` / host command execution explicitly:

```bash
export KAWKAB_ALLOW_CHILD_PROCESS=1
```

## Release Readiness

- Baseline contract: `docs/FEATURE_BASELINE.md`
- Product vision: `docs/PRODUCT_VISION.md`
- Release checklist: `docs/RELEASE_CHECKLIST.md`

Create a zip archive (excluding `target`):

```bash
zip -r kawkab-workspace.zip . -x "target/*" -x "*/target/*"
```

Exclude both `target` and `.git`:

```bash
zip -r kawkab-workspace.zip . -x "target/*" -x "*/target/*" -x ".git/*"
```

## Known Non-Blocking Warning

Depending on your lockfile state, Cargo may still print non-blocking warnings from transitive crates.
This does not block successful builds right now.

To reduce build output noise, use `-q` as shown above.

## Troubleshooting

- **No output when running:**
  - Ensure your JS file contains `console.log("...")` or `console.log('...')`.
  - Ensure you rebuilt the latest code:
    ```bash
    cargo build --release --features tokio-uring -q
    ```
- **`missing value for --file` error:**
  - Use:
    ```bash
    ./target/release/kawkab --file test.js
    ```
- **Running from Windows PowerShell does not behave as expected:**
  - Run from WSL in the project directory.

## Cursor agent transcript

Related Cursor chat / agent transcript id: `06cbab36-73c3-4aa8-a9de-26f67f931f8f` (log file: `agent-transcripts/06cbab36-73c3-4aa8-a9de-26f67f931f8f.jsonl` under the Cursor project directory for this workspace).

# Codetxt Command
codetxt . --exclude-pattern "target/" --exclude-pattern "Cargo.lock" --output kawkab-project.txt
# Kawkab Node.js Compatibility

Kawkab expands **best-effort** alignment with Node.js **built-in names and rough API shape** over time; this is **not** a promise that every npm package will run unchanged. This page tracks API/module/global compatibility status in one place and is intended to be updated frequently.

Compatibility target in this document is aligned to **Node.js v23** surface area for *naming and expectations*, not byte-for-byte parity with Node or universal registry coverage. For product-level scope, see [`PRODUCT_VISION.md`](PRODUCT_VISION.md). For **🟢/🟡 definitions**, see [`COMPAT_DEFINITION_OF_DONE.md`](COMPAT_DEFINITION_OF_DONE.md). For **explicit non-goals / 🔴 stance**, see [`NODE_NON_GOALS.md`](NODE_NON_GOALS.md).

## What “npm compatibility” means here

There is **no guarantee** that an arbitrary package from the public npm registry will run. Compatibility means:

- **Built-ins:** Only the modules and globals listed below are implemented or shimmed; anything else is `🔴` unless it loads as **user JS** from `node_modules`.
- **Loading:** CommonJS `require()` is implemented for the built-in table plus filesystem resolution (`resolve_module_path` / `resolve_module_path_with_kind` in [`core/src/node/module_loader.rs`](../core/src/node/module_loader.rs)). **Bare specifiers** split package name and subpath (e.g. `lodash/get`, `@scope/pkg/x`) before resolving under `node_modules`. **`package.json`** is parsed as JSON; **`"exports"`** supports the `"."` and subpath keys, conditional objects (including nested `node` groups), string/array targets, and a single `*` per pattern key/target pair; **`"imports"`** resolves internal `#…` specifiers from the nearest `package.json`. **`development` / `production`** conditions follow **`process.env.NODE_ENV`** as visible in JS (refreshed before module resolution / `import` loads), defaulting to `production` when unset. Edge cases not matching Node 23 exactly include some multi-star or highly unusual export maps. ESM `import`/`export` is handled by the QuickJS module loader when the entry or a dependency is ESM (see [`core/src/node/mod.rs`](../core/src/node/mod.rs)).
- **Native addons:** `*.node` / N-API native modules are **not** supported.
- **Policy:** `child_process` is disabled unless `KAWKAB_ALLOW_CHILD_PROCESS=1`.

Use this file plus a quick smoke test for any package you care about.

## Implementation reference (source of truth)

| Area | Location |
|------|----------|
| `require()` / `require('node:…')` branches | [`js_require` in `core/src/node/mod.rs`](../core/src/node/mod.rs) (built-in name matching after stripping the `node:` prefix) |
| Runtime bootstrap, primed shims, globals | [`install_runtime` in `core/src/node/mod.rs`](../core/src/node/mod.rs) |
| Path resolution, `package.json`, ESM/CJS detection | [`core/src/node/module_loader.rs`](../core/src/node/module_loader.rs), [`esm_loader`](../core/src/node/) |
| CLI: order of setup, bytecode CJS wrapper | [`kawkab/src/main.rs`](../kawkab/src/main.rs) |

**Built-in names handled inside `js_require` today:** `assert`, `buffer`, `console`, `process`, `fs`, `path`, `os`, `punycode`, `events`, `util`, `sys`, `net`, `http`, `https`, `child_process`, `stream`, `crypto`, `url`, `querystring`, `string_decoder`, `dgram`, `diagnostics_channel`, `dns`, `dns/promises`, `readline`, `zlib`, `tls`, `vm`, `worker_threads`, `timers`, `timers/promises`, `perf_hooks`, `module`, `node:test`, `test`. Any other specifier falls through to file / `node_modules` resolution (or fails).

## Status Legend

- `🟢` **Typical npm / corpus coverage** per [`COMPAT_DEFINITION_OF_DONE.md`](COMPAT_DEFINITION_OF_DONE.md) (not byte-for-byte Node v23); each row lists **Remaining vs Node** where relevant.
- `🟡` Partially implemented / simplified / important caveats
- `🔴` Not implemented

## Built-in Node.js Modules

- `node:assert` — `🟢` Core API implemented: `AssertionError` exported as a native constructor (`new assert.AssertionError(...)` matches Node’s shape); failed assertions from native `assert.*` helpers throw `Error` values with `name === 'AssertionError'` and `code === 'ERR_ASSERTION'` (not guaranteed to be `instanceof assert.AssertionError`); `equal`/`notEqual` use real abstract equality (`==` via `__kawkabLooseEq`, plus a nullish fast-path); `deepEqual`/`deepStrictEqual` recurse arrays, plain objects, `Date`, `RegExp`, and detect common cycles; `throws` accepts optional RegExp / validator function / constructor filter; `match` / `rejects` implemented (`rejects` returns a Promise).
  - **Remaining vs Node v23:** full per-method options objects (`message`/`operator`/`stackStartFn` everywhere), `assert.strict` / `CallTracker` / `partialDeepStrictEqual`, and byte-for-byte `util.isDeepStrictEqual` edge-case parity (e.g. `Map`/`Set`/typed arrays).
- `node:buffer` — `🟢` `require('buffer')` re-exports the **global** `Buffer` constructor (and `SlowBuffer`, `kMaxLength`, `constants`) installed from [`core/src/node/buffer.rs`](../core/src/node/buffer.rs): `Uint8Array` subclass with native helpers (`from`, `alloc`, `concat`, etc.). Automated check: `priority_builtins_green_contract` in [`core/src/node/compat_contract_tests.rs`](../core/src/node/compat_contract_tests.rs).
  - **Remaining vs Node v23:** full `Buffer` method matrix, `transcode`, pooled `SlowBuffer`, and edge-case encoding / `INSPECT_MAX_BYTES` parity.
- `node:console` — `🟡` `require('console')` / `require('node:console')` resolves to the same object as `globalThis.console` (installed from [`core/src/console.rs`](../core/src/console.rs) on the CLI). Suitable for typical logging; not full Node v23 `console` module parity.
- `node:dgram` — `🟡` Real UDP I/O (Rust): `createSocket` supports `bind` / `send` / `close` / `address`, `message` events with `(msg, rinfo)`, and listener helpers (`on`, `addListener`, `once`, `removeListener`, `removeAllListeners`). Multicast / TTL helpers are compatibility no-ops.
  - **Remaining vs Node v23:** full `bind`/`send` overload matrix, membership APIs, error surface, and test-suite parity.
- `node:diagnostics_channel` — `🟢` Runtime surface: `channel(name)`, `hasSubscribers(name)`, `subscribe(name, fn)`, `unsubscribe(name, fn)`, `tracingChannel(nameOrChannels)`, `boundedChannel(nameOrChannels)`; channel objects expose `publish`, `subscribe`, `unsubscribe`, `bindStore`, `unbindStore`, `runStores`, and a **`hasSubscribers` boolean property** (updated when subscribers change; not the same shape as Node’s getter in every edge case).
- `node:dns` — `🟢` Compatibility-oriented resolver: callback APIs `lookup` (numeric/object options and `{ all: true }`), `resolve(host, [rrtype], cb)`, `resolve4`/`resolve6`/`resolveAny`/`resolveCaa`/`resolveCname`/`resolveMx`/`resolveNaptr`/`resolveNs`/`resolvePtr`/`resolveSoa`/`resolveSrv`/`resolveTxt`, `reverse`, `lookupService`, `getServers`/`setServers`, and `Resolver` with matching methods.
  - **`dns/promises`:** `🟢` Promise-based surface for the same family (`lookup`, `resolve*`, `reverse`, `lookupService`, `Resolver`).
- `node:events` — `🟡` Expanded compatibility shim: `EventEmitter` supports `on`/`addListener`, `off`/`removeListener`, `once`, `prependListener`, `prependOnceListener`, `removeAllListeners`, `emit` (variadic args), `listenerCount` (instance + static helper), `listeners`, `rawListeners`, `eventNames` (including `Symbol` keys), and max-listener controls (`setMaxListeners` / `getMaxListeners`) with leak-warning behavior.
  - **To reach `🟢`:** full Node v23 equivalence for nuanced listener lifecycle and warning/error edge-cases (including exact async-iterator semantics, full helper parity, and Node test-suite byte-for-byte behavior).
- `node:fs` — `🟢` Sync APIs: `readFileSync`, `writeFileSync`, `copyFileSync`, `rmSync` (optional `{ recursive, force }`), `existsSync`, `mkdirSync`, `readdirSync`, `unlinkSync`, `rmdirSync`, `statSync`. **`fs.promises`:** `readFile`, `writeFile`, `stat`, `readdir`, `mkdir`, `unlink` (host async path with sync fallback when no Tokio handle). Corpus/contract: `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full async API surface, watch streams, full `open`/`fd` matrix, permission/ACL models, and error `code` parity.
- `node:http` — `🟢` Baseline server APIs (`createServer`/`listen`/`close`) with simplified request/response model. **Client:** blocking `get` / `request` (URL string + callback) via **reqwest**; callback runs synchronously after the full body is read; response is a plain object with `statusCode`, `statusMessage`, `headers`, and **`body`** as `ArrayBuffer` (not a streaming `IncomingMessage`). Contract: live `http.get('http://example.com')` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** streaming `IncomingMessage`/`ClientRequest`, full `request` options object, `Agent`, and WebSocket upgrades.
- `node:https` — `🟢` Same server baseline as `node:http` (**no TLS** on `createServer`). **Client:** HTTPS URLs use **rustls** through reqwest; same non-streaming response shape as `http`. Contract: `https.get('https://example.com')` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** TLS `createServer`, client cert pinning/options matrix, ALPN/h2, streaming parity with `http`.
- `node:os` — `🟡` Partial (`platform`, `tmpdir`, `homedir`).
- `node:path` — `🟢` `join`, `dirname`, `basename`, `extname`, `resolve`, `normalize`, `relative`, `parse`, `sep`, `delimiter`. Contract: `path.join` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** `posix`/`win32` namespaces, `pathToFileURL`, `fileURLToPath`, and full Windows long-path edge cases.
- `node:punycode` — `🟡` `decode`/`encode`: ASCII-oriented baseline (unchanged). **`toASCII` / `toUnicode`:** IDNA via **`idna`** crate (Unicode domains / ACE), closer to Node’s domain handling than the old ASCII-only stub.
  - **Remaining vs Node:** full RFC 3492 punycode module parity for non-domain strings, edge-case errors.
- `node:querystring` — `🟡` Compatibility-focused behavior (`parse`, `stringify`) including repeated-key arrays and URL-style encode/decode for common cases.
  - **To reach `🟢`:** deeper edge-case parity (complete option matrix + full Node test-suite equivalence).
- `node:readline` — `🟡` Minimal JS shim: `createInterface` returns a stub with `question` (invokes callback asynchronously with an empty answer), plus no-op `on` / `close` / `pause` / `resume`. Interactive REPL behavior is not implemented.
- `node:stream` — `🟢` Baseline behavior (`Readable`, `Writable`, `Duplex`, `Transform`) including basic `pipe`/`data`/`end` (primed shim in [`mod.rs`](../core/src/node/mod.rs)). Contract: `Readable` push/end in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full backpressure / object mode / `pipeline` / async iterator parity and Node stream test-suite behavior.
- `node:string_decoder` — `🟡` `StringDecoder` baseline (`write`, `end`).
  - **To reach `🟢`:** full multibyte boundary handling and encoding parity.
- `node:timers` — `🟢` `require('timers')` exports `setTimeout`, `clearTimeout`, `setInterval`, `clearInterval`, `setImmediate`, `clearImmediate` bound to the same native implementations as globals. Without a Tokio task sender, `setTimeout(fn, 0)` runs the callback inline after a host sleep (see `schedule_timer_inner` in [`mod.rs`](../core/src/node/mod.rs)). Contract: `timers.setTimeout` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full event-loop ordering vs I/O, `ref`/`unref`, and exact delay/immediate ordering under load.
  - **Subpath `timers/promises`:** `🟡` `setTimeout(delay, value)` and `setImmediate(value)` return Promises. `setInterval` async-iterator style from Node is not implemented.
- `node:tty` — `🔴` Not implemented.
- `node:url` — `🟢` Baseline `URL`/`URLSearchParams` behavior (`append`, `set`, `get`, `getAll`, `delete`, `toString`, computed `href`) via primed shim. Contract: `URL` + `searchParams` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full WHATWG URL normalization, `pathToFileURL`, legacy `url.parse` / `format` if required by corpus.
- `node:zlib` — `🟡` Native sync helpers: `gzipSync`, `gunzipSync`, `deflateSync`, `inflateSync` (via `flate2`). No streaming API or full deflate/gzip option matrix vs Node.
- `node:async_hooks` — `🔴` Not implemented.
- `node:child_process` — `🟢` `execSync` and `spawnSync` available behind policy gate (`KAWKAB_ALLOW_CHILD_PROCESS=1`). Contract: `execSync('echo …')` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** async `spawn`/`exec`, stdio streams, `fork`, `execFile`, signal handling, and shell option parity when policy allows.
- `node:cluster` — `🔴` Not implemented.
- `node:crypto` — `🟢` Loaded via primed JS shim (`CRYPTO_SHIM_SRC`) plus native `__kawkabCrypto*` helpers: `createHash` / `createHmac` with `update` and `digest('hex' | 'base64' | default Buffer)`; `createHash` supports **`sha1`**, **`sha256`**, **`sha384`**, **`sha512`**, **`md5`**, **`blake3`**; `randomBytes` sync and callback/async (Promise-backed) forms returning `Buffer`. Contract: `createHash('sha256')` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** ciphers, `createSign`/`verify`, ECDH, `subtle`, Web Crypto alignment, and full OpenSSL-compatible option matrices.
- `node:domain` — `🔴` Not implemented.
- `node:http2` — `🔴` Not implemented.
- `node:module` — `🟡` **`createRequire(filename | file URL)`** returns a `require` function with resolution rooted at the parent directory of `filename` (after stripping a `file://` prefix). Other `module` APIs (`enableCompileCache`, `Module`, sync hooks, etc.) are not implemented.
- `node:net` — `🟢` Compatibility entrypoint: same `createServer` shape as `http` for baseline usage. Contract: `createServer` export in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** real `Socket`/`Server` TCP lifecycle, `connect`, TLS bridging, and `BlockList`/`SocketAddress` APIs.
- `node:perf_hooks` — `🟡` Minimal export: `performance` re-exports the global `performance` object (`now`, `timeOrigin`).
  - **To reach `🟢`:** full `perf_hooks` API (`PerformanceObserver`, histograms, etc.) matching Node.
- `node:process` — `🟢` The **`process` object is installed as a global** during `install_runtime` (`argv`, `env`, `cwd`, `nextTick`, timing helpers, `stdout`/`stderr` via `process::install_stdio`, etc.). **`require('process')` / `require('node:process')`** returns a duplicate of the global `process` binding (`js_dup_value`) for packages that expect the module form. Contract: `process.cwd` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** `emit`, `binding`, full `versions`/`release`, `permission`, `dlopen`, and internal fields used by advanced tooling.
- `node:sys` — `🟡` Legacy alias: same exports as `node:util` (`inspect`, `types.isDate`) for packages that still `require('sys')`.
- `node:tls` — `🟡` Baseline API surface (`connect`, `createServer`) as compatibility placeholders.
  - **To reach `🟢`:** real TLS handshake/session behavior and Node option parity.
- `node:util` — `🟡` Partial (`inspect`, `types.isDate`). **`promisify`** is provided when `globalThis.__kawkabUtilPromisify` installs successfully at bootstrap (Node-style callback-last wrapping into a Promise).
- `node:v8` — `🔴` Not implemented.
- `node:vm` — `🟡` Baseline API surface (`runInThisContext`, `runInNewContext`, `Script`, `createContext`, `isContext`) with simplified semantics.
  - **Current behavior:** `runInNewContext(code, sandbox)` maps own string properties of `sandbox` to `Function` parameters and evaluates `code` as `return (<code>);` inside that function (expression-oriented, not full Node `Script` / VM isolation).
  - **To reach `🟢`:** isolated context guarantees and broader script/module execution parity.
- `node:wasi` — `🔴` Not implemented.
- `node:worker_threads` — `🟢` **Baseline workers on real OS threads:** each `Worker` runs in its own **QuickJS isolate** on a dedicated Rust thread (`core/src/node/worker_threads.rs`). Main thread: `Worker` constructor (script path), `postMessage` / `on('message')`, `terminate`, `isMainThread`, `threadId`. Worker isolate: `parentPort.postMessage` / `parentPort.on('message')`. Messages are **JSON text** (values must round-trip via `JSON.stringify` / `JSON.parse`; otherwise a `TypeError` is thrown). Nested `Worker` from inside a worker is rejected. Contract: `node::compat_contract_tests::worker_threads_roundtrip` (and idle spawn smoke).
  - **Remaining vs Node:** full **structured clone** (including `ArrayBuffer`, `SharedArrayBuffer`, transfer lists), `new Worker(new URL(...))` / `import.meta.url`, `workerData`, `MessageChannel` / `MessagePort` semantics, `resourceLimits`, `markAsUntransferable`, `receiveMessageOnPort`, `BroadcastChannel`, `setEnvironmentData` / `getEnvironmentData`, `performance` in worker, `eventLoopUtilization`, and full `events`/EventEmitter parity on `Worker`.
- `node:inspector` — `🔴` Not implemented.
- `node:repl` — `🔴` Not implemented.
- `node:sqlite` — `🔴` Not implemented.
- `node:test` / `test` — `🟡` Baseline (`test`, `it`, `describe`) for simple cases.
  - **To reach `🟢`:** richer runner semantics, hooks, mocks/snapshots/timers parity.
- `node:trace_events` — `🔴` Not implemented.

## Node.js Globals

- `AbortController` — `🟢` Fully implemented.
- `AbortSignal` — `🟢` Fully implemented.
- `Blob` — `🔴` Not implemented.
- `Buffer` — `🟢` Global constructor installed with [`core/src/node/buffer.rs`](../core/src/node/buffer.rs) (`Uint8Array` subclass + native helpers). Same **Remaining vs Node** as `node:buffer`.
- `ByteLengthQueuingStrategy` — `🔴` Not implemented.
- `__dirname` — `🟢` Fully implemented.
- `__filename` — `🟢` Fully implemented.
- `atob()` — `🟡` Baseline implemented (Web-style base64 decode to binary string).
  - **To reach `🟢`:** full WebIDL error behavior and edge-case parity with Node.
- `Atomics` — `🔴` Not implemented.
- `BroadcastChannel` — `🔴` Not implemented.
- `btoa()` — `🟡` Baseline implemented (binary string to base64).
  - **To reach `🟢`:** full WebIDL error behavior and edge-case parity with Node.
- `clearImmediate()` — `🟢` Fully implemented.
- `clearInterval()` — `🟢` Fully implemented.
- `clearTimeout()` — `🟢` Fully implemented.
- `CompressionStream` — `🔴` Not implemented.
- `console` — `🟡` On the **`kawkab` CLI**, [`core::console::install`](../core/src/console.rs) runs before runtime bootstrap and wires **`log` / `error` / `warn` / `info` / `debug`** on `globalThis.console`. `require('console')` / `require('node:console')` exposes the same object.
- `CountQueuingStrategy` — `🔴` Not implemented.
- `Crypto` — `🔴` Not implemented.
- `SubtleCrypto (crypto)` — `🔴` Not implemented.
- `CryptoKey` — `🔴` Not implemented.
- `CustomEvent` — `🔴` Not implemented.
- `DecompressionStream` — `🔴` Not implemented.
- `Event` — `🔴` Not implemented.
- `EventTarget` — `🔴` Not implemented.
- `exports` — `🟢` Fully implemented.
- `fetch` — `🟡` Compatibility shim available (non-standard backend behavior).
  - **To reach `🟢`:** standards-compliant request/response lifecycle and full fetch semantics.
- `FormData` — `🔴` Not implemented.
- `global` — `🟢` Implemented.
- `globalThis` — `🟢` Implemented.
- `Headers` — `🟡` Compatibility shim available.
  - **To reach `🟢`:** complete headers normalization/iteration semantics parity.
- `MessageChannel` — `🔴` Not implemented.
- `MessageEvent` — `🔴` Not implemented.
- `MessagePort` — `🔴` Not implemented.
- `module` — `🟢` Fully implemented (CommonJS runtime context).
- `PerformanceEntry` — `🔴` Not implemented.
- `PerformanceMark` — `🔴` Not implemented.
- `PerformanceMeasure` — `🔴` Not implemented.
- `PerformanceObserver` — `🔴` Not implemented.
- `PerformanceObserverEntryList` — `🔴` Not implemented.
- `PerformanceResourceTiming` — `🔴` Not implemented.
- `performance` — `🟡` Minimal `performance.now()` and `performance.timeOrigin` (compatibility-oriented; not full high-resolution / Node perf_hooks parity).
  - **To reach `🟢`:** timing APIs aligned with Node `perf_hooks` and `Performance` object surface.
- `process` — `🟢` Mostly implemented with compatibility caveats (see `node:process`).
  - **Remaining vs Node:** same bullets as `node:process`.
- `queueMicrotask()` — `🟢` Implemented.
- `ReadableByteStreamController` — `🔴` Not implemented.
- `ReadableStream` — `🔴` Not implemented.
- `ReadableStreamBYOBReader` — `🔴` Not implemented.
- `ReadableStreamBYOBRequest` — `🔴` Not implemented.
- `ReadableStreamDefaultController` — `🔴` Not implemented.
- `ReadableStreamDefaultReader` — `🔴` Not implemented.
- `require()` — `🟢` Implemented for the CommonJS + built-in subset described above.
- `Response` — `🟡` Compatibility shim available.
  - **To reach `🟢`:** full body/stream/status/header behavior parity.
- `Request` — `🟡` Compatibility shim available.
  - **To reach `🟢`:** full request cloning/body/headers semantics parity.
- `setImmediate()` — `🟢` Fully implemented (same native path as `setTimeout` in this embedding; see `node:timers`).
- `setInterval()` — `🟢` Fully implemented.
- `setTimeout()` — `🟢` Fully implemented.
- `structuredClone()` — `🟡` Baseline via `JSON.parse(JSON.stringify(value))` when missing (JSON-serializable values only; no `Map`/`Set`/`ArrayBuffer`/full `Date` semantics).
  - **To reach `🟢`:** spec-correct structured cloning including transfer lists and non-JSON types.
- `SubtleCrypto` — `🔴` Not implemented.
- `DOMException` — `🔴` Not implemented.
- `TextDecoder` — `🟡` Compatibility shim available.
  - **To reach `🟢`:** complete encoding/error-mode parity for all supported encodings.
- `TextDecoderStream` — `🔴` Not implemented.
- `TextEncoder` — `🟡` Compatibility shim available.
  - **To reach `🟢`:** complete byte-level parity in all edge cases.
- `TextEncoderStream` — `🔴` Not implemented.
- `TransformStream` — `🔴` Not implemented.
- `TransformStreamDefaultController` — `🔴` Not implemented.
- `URL` — `🟡` Compatibility constructor exposed.
  - **To reach `🟢`:** full WHATWG URL conformance and edge-case normalization parity.
- `URLSearchParams` — `🟡` Compatibility constructor exposed.
  - **To reach `🟢`:** full iteration/sorting/encoding parity with Node.
- `WebAssembly` — `🔴` Not implemented.
- `WritableStream` — `🔴` Not implemented.
- `WritableStreamDefaultController` — `🔴` Not implemented.
- `WritableStreamDefaultWriter` — `🔴` Not implemented.

## Compatibility coverage (reference scripts)

Reference scripts under `examples/` were removed. When you add new checks, list them in [`docs/NPM_CORPUS.md`](NPM_CORPUS.md). Workspace tests and [`scripts/compat_smoke.sh`](../scripts/compat_smoke.sh) provide a repeatable baseline.

## Notes

- This status reflects current implementation in this repository and intentionally marks unsupported Node APIs as `🔴`.
- The compact snapshot in `README.md` is a quick overview; **this file is the detailed reference** and should stay aligned with `js_require` / `install_runtime`.
- If a module/global is added or upgraded, update this file and `docs/FEATURE_BASELINE.md` in the same change.

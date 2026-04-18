# Kawkab Node.js Compatibility

Kawkab is moving toward complete Node.js compatibility. This page tracks API/module/global compatibility status in one place and is intended to be updated frequently.

Compatibility target in this document is aligned to **Node.js v23** surface area for *naming and expectations*, not byte-for-byte parity.

## What “npm compatibility” means here

There is **no guarantee** that an arbitrary package from the public npm registry will run. Compatibility means:

- **Built-ins:** Only the modules and globals listed below are implemented or shimmed; anything else is `🔴` unless it loads as **user JS** from `node_modules`.
- **Loading:** CommonJS `require()` is implemented for the built-in table plus filesystem resolution (`resolve_module_path` / `resolve_module_path_with_kind` in [`core/src/node/module_loader.rs`](../core/src/node/module_loader.rs)). **`package.json` `"exports"`:** the `"."` entry may expose `require`, `import`, and `default` strings; resolution picks the path matching the load context (CJS vs ESM). Complex export maps, `imports`, and conditions beyond that subset are not fully handled. ESM `import`/`export` is handled by the QuickJS module loader when the entry or a dependency is ESM (see runtime bootstrap in [`core/src/node/mod.rs`](../core/src/node/mod.rs)).
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

**Built-in names handled inside `js_require` today:** `assert`, `buffer`, `console`, `process`, `fs`, `path`, `os`, `punycode`, `events`, `util`, `sys`, `net`, `http`, `https`, `child_process`, `stream`, `crypto`, `url`, `querystring`, `string_decoder`, `dgram`, `diagnostics_channel`, `dns`, `dns/promises`, `readline`, `zlib`, `tls`, `vm`, `worker_threads`, `timers`, `timers/promises`, `perf_hooks`, `node:test`, `test`. Any other specifier falls through to file / `node_modules` resolution (or fails).

## Status Legend

- `🟢` Fully implemented
- `🟡` Partially implemented / simplified / caveats
- `🔴` Not implemented

## Built-in Node.js Modules

- `node:assert` — `🟢` Core API implemented: `AssertionError` exported as a native constructor (`new assert.AssertionError(...)` matches Node’s shape); failed assertions from native `assert.*` helpers throw `Error` values with `name === 'AssertionError'` and `code === 'ERR_ASSERTION'` (not guaranteed to be `instanceof assert.AssertionError`); `equal`/`notEqual` use real abstract equality (`==` via `__kawkabLooseEq`, plus a nullish fast-path); `deepEqual`/`deepStrictEqual` recurse arrays, plain objects, `Date`, `RegExp`, and detect common cycles; `throws` accepts optional RegExp / validator function / constructor filter; `match` / `rejects` implemented (`rejects` returns a Promise).
  - **Remaining vs Node v23:** full per-method options objects (`message`/`operator`/`stackStartFn` everywhere), `assert.strict` / `CallTracker` / `partialDeepStrictEqual`, and byte-for-byte `util.isDeepStrictEqual` edge-case parity (e.g. `Map`/`Set`/typed arrays).
- `node:buffer` — `🟡` `require('buffer')` re-exports the **global** `Buffer` constructor (and `SlowBuffer`, `kMaxLength`, `constants`) installed from [`core/src/node/buffer.rs`](../core/src/node/buffer.rs): `Uint8Array` subclass with native helpers (`from`, `alloc`, `concat`, etc.). Suitable for many npm stacks; not full Node v23 `buffer` module parity.
- `node:console` — `🟡` `require('console')` / `require('node:console')` resolves to the same object as `globalThis.console` (installed from [`core/src/console.rs`](../core/src/console.rs) on the CLI). Suitable for typical logging; not full Node v23 `console` module parity.
- `node:dgram` — `🟡` Real UDP I/O (Rust): `createSocket` supports `bind` / `send` / `close` / `address`, `message` events with `(msg, rinfo)`, and listener helpers (`on`, `addListener`, `once`, `removeListener`, `removeAllListeners`). Multicast / TTL helpers are compatibility no-ops.
  - **Remaining vs Node v23:** full `bind`/`send` overload matrix, membership APIs, error surface, and test-suite parity.
- `node:diagnostics_channel` — `🟢` Runtime surface: `channel(name)`, `hasSubscribers(name)`, `subscribe(name, fn)`, `unsubscribe(name, fn)`, `tracingChannel(nameOrChannels)`, `boundedChannel(nameOrChannels)`; channel objects expose `publish`, `subscribe`, `unsubscribe`, `bindStore`, `unbindStore`, `runStores`, and a **`hasSubscribers` boolean property** (updated when subscribers change; not the same shape as Node’s getter in every edge case).
- `node:dns` — `🟢` Compatibility-oriented resolver: callback APIs `lookup` (numeric/object options and `{ all: true }`), `resolve(host, [rrtype], cb)`, `resolve4`/`resolve6`/`resolveAny`/`resolveCaa`/`resolveCname`/`resolveMx`/`resolveNaptr`/`resolveNs`/`resolvePtr`/`resolveSoa`/`resolveSrv`/`resolveTxt`, `reverse`, `lookupService`, `getServers`/`setServers`, and `Resolver` with matching methods.
  - **`dns/promises`:** `🟢` Promise-based surface for the same family (`lookup`, `resolve*`, `reverse`, `lookupService`, `Resolver`).
- `node:events` — `🟡` Expanded compatibility shim: `EventEmitter` supports `on`/`addListener`, `off`/`removeListener`, `once`, `prependListener`, `prependOnceListener`, `removeAllListeners`, `emit` (variadic args), `listenerCount` (instance + static helper), `listeners`, `rawListeners`, `eventNames` (including `Symbol` keys), and max-listener controls (`setMaxListeners` / `getMaxListeners`) with leak-warning behavior.
  - **To reach `🟢`:** full Node v23 equivalence for nuanced listener lifecycle and warning/error edge-cases (including exact async-iterator semantics, full helper parity, and Node test-suite byte-for-byte behavior).
- `node:fs` — `🟡` Sync APIs: `readFileSync`, `writeFileSync`, `existsSync`, `mkdirSync`, `readdirSync`, `unlinkSync`, `rmdirSync`, `statSync`. **`fs.promises`:** `readFile`, `writeFile`, `stat`, `readdir`, `mkdir`, `unlink` (host async path with sync fallback when no Tokio handle).
- `node:http` — `🟡` Baseline server APIs (`createServer`/`listen`/`close`) with simplified request/response model.
- `node:https` — `🟡` Same baseline entry as `node:http` (`createServer` only; no TLS — not real HTTPS).
  - **To reach `🟢`:** TLS-backed `https.request`/`get`/`createServer` parity with Node.
- `node:os` — `🟡` Partial (`platform`, `tmpdir`, `homedir`).
- `node:path` — `🟡` Partial (`join`, `dirname`, `basename`, `extname`, `resolve`, `normalize`, `sep`, `delimiter`).
- `node:punycode` — `🟡` Baseline surface: `decode`/`encode`/`toASCII`/`toUnicode` are **ASCII-only** pass-through; `decode`/`toUnicode` **reject** inputs containing `xn--` (real ACE / Punycode is not implemented).
  - **To reach `🟢`:** RFC 3492 / Node `punycode` behavior parity.
- `node:querystring` — `🟡` Compatibility-focused behavior (`parse`, `stringify`) including repeated-key arrays and URL-style encode/decode for common cases.
  - **To reach `🟢`:** deeper edge-case parity (complete option matrix + full Node test-suite equivalence).
- `node:readline` — `🟡` Minimal JS shim: `createInterface` returns a stub with `question` (invokes callback asynchronously with an empty answer), plus no-op `on` / `close` / `pause` / `resume`. Interactive REPL behavior is not implemented.
- `node:stream` — `🟡` Baseline behavior (`Readable`, `Writable`, `Duplex`, `Transform`) including basic `pipe`/`data`/`end`.
  - **To reach `🟢`:** backpressure semantics, object mode parity, and broader stream lifecycle compatibility.
- `node:string_decoder` — `🟡` `StringDecoder` baseline (`write`, `end`).
  - **To reach `🟢`:** full multibyte boundary handling and encoding parity.
- `node:timers` — `🟡` `require('timers')` exports `setTimeout`, `clearTimeout`, `setInterval`, `clearInterval`, `setImmediate`, `clearImmediate` bound to the same native implementations as globals. **`setImmediate` / `setTimeout` in this embedding share the same underlying C callback** (`js_set_timeout`); delay semantics differ from Node in edge cases. Event-loop behavior remains simplified vs Node.
  - **Subpath `timers/promises`:** `🟡` `setTimeout(delay, value)` and `setImmediate(value)` return Promises. `setInterval` async-iterator style from Node is not implemented.
- `node:tty` — `🔴` Not implemented.
- `node:url` — `🟡` Baseline `URL`/`URLSearchParams` behavior (`append`, `set`, `get`, `getAll`, `delete`, `toString`, computed `href`).
  - **To reach `🟢`:** full WHATWG URL edge-case parity and normalization matching Node.
- `node:zlib` — `🟡` Native sync helpers: `gzipSync`, `gunzipSync` (via `flate2`). No streaming API, deflate/inflate matrix, or full option parity with Node.
- `node:async_hooks` — `🔴` Not implemented.
- `node:child_process` — `🟡` `execSync` and `spawnSync` available behind policy gate (`KAWKAB_ALLOW_CHILD_PROCESS=1`).
- `node:cluster` — `🔴` Not implemented.
- `node:crypto` — `🟡` Loaded via primed JS shim (`CRYPTO_SHIM_SRC`) plus native `__kawkabCrypto*` helpers: `createHash` / `createHmac` with `update` and `digest('hex' | 'base64' | default Buffer)`; `randomBytes` sync and callback/async (Promise-backed) forms returning `Buffer`.
  - **To reach `🟢`:** algorithm matrix, encoding/error behavior, and cryptographic parity with Node.
- `node:domain` — `🔴` Not implemented.
- `node:http2` — `🔴` Not implemented.
- `node:module` — `🟡` CommonJS loading subset is implemented; full Node `module` internals are not.
- `node:net` — `🟡` Compatibility entrypoint: same `createServer` shape as `http` for baseline usage.
- `node:perf_hooks` — `🟡` Minimal export: `performance` re-exports the global `performance` object (`now`, `timeOrigin`).
  - **To reach `🟢`:** full `perf_hooks` API (`PerformanceObserver`, histograms, etc.) matching Node.
- `node:process` — `🟡` The **`process` object is installed as a global** during `install_runtime` (`argv`, `env`, `cwd`, `nextTick`, timing helpers, `stdout`/`stderr` via `process::install_stdio`, etc.). **`require('process')` / `require('node:process')`** returns a duplicate of the global `process` binding (`js_dup_value`) for packages that expect the module form.
- `node:sys` — `🟡` Legacy alias: same exports as `node:util` (`inspect`, `types.isDate`) for packages that still `require('sys')`.
- `node:tls` — `🟡` Baseline API surface (`connect`, `createServer`) as compatibility placeholders.
  - **To reach `🟢`:** real TLS handshake/session behavior and Node option parity.
- `node:util` — `🟡` Partial (`inspect`, `types.isDate`). **`promisify`** is provided when `globalThis.__kawkabUtilPromisify` installs successfully at bootstrap (Node-style callback-last wrapping into a Promise).
- `node:v8` — `🔴` Not implemented.
- `node:vm` — `🟡` Baseline API surface (`runInThisContext`, `runInNewContext`, `Script`, `createContext`, `isContext`) with simplified semantics.
  - **Current behavior:** `runInNewContext(code, sandbox)` maps own string properties of `sandbox` to `Function` parameters and evaluates `code` as `return (<code>);` inside that function (expression-oriented, not full Node `Script` / VM isolation).
  - **To reach `🟢`:** isolated context guarantees and broader script/module execution parity.
- `node:wasi` — `🔴` Not implemented.
- `node:worker_threads` — `🟡` Baseline compatibility (`Worker` constructor, `postMessage`, `terminate`, `isMainThread`, stub `MessageChannel`/`parentPort`).
  - **To reach `🟢`:** real multithreaded runtime semantics and full worker lifecycle/resource parity.
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
- `Buffer` — `🟡` Global constructor installed with [`core/src/node/buffer.rs`](../core/src/node/buffer.rs) (`Uint8Array` subclass + native helpers). Same caveats as `node:buffer`.
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
- `process` — `🟡` Mostly implemented with compatibility caveats (see `node:process`).
  - **To reach `🟢`:** remaining internal bindings/APIs required by advanced Node package scenarios.
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

Reference scripts under `examples/` were removed. When you add new checks, list them here and in [`docs/NPM_CORPUS.md`](NPM_CORPUS.md).

## Notes

- This status reflects current implementation in this repository and intentionally marks unsupported Node APIs as `🔴`.
- The compact snapshot in `README.md` is a quick overview; **this file is the detailed reference** and should stay aligned with `js_require` / `install_runtime`.
- If a module/global is added or upgraded, update this file and `docs/FEATURE_BASELINE.md` in the same change.

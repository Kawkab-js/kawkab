# Kawkab Node.js Compatibility

Kawkab expands **best-effort** alignment with Node.js **built-in names and rough API shape** over time; this is **not** a promise that every npm package will run unchanged. This page tracks API/module/global compatibility status in one place and is intended to be updated frequently.

Compatibility target in this document is aligned to **Node.js v23** surface area for *naming and expectations*, not byte-for-byte parity with Node or universal registry coverage. For product-level scope, see `[PRODUCT_VISION.md](PRODUCT_VISION.md)`. For **🟢/🟡 definitions**, see `[COMPAT_DEFINITION_OF_DONE.md](COMPAT_DEFINITION_OF_DONE.md)`. For **explicit non-goals / 🔴 stance**, see `[NODE_NON_GOALS.md](NODE_NON_GOALS.md)`.

## What “npm compatibility” means here

There is **no guarantee** that an arbitrary package from the public npm registry will run. Compatibility means:

- **Built-ins:** Only the modules and globals listed below are implemented or shimmed; anything else is `🔴` unless it loads as **user JS** from `node_modules`.
- **Loading:** CommonJS `require()` is implemented for the built-in table plus filesystem resolution (`resolve_module_path` / `resolve_module_path_with_kind` in `[core/src/node/module_loader.rs](../core/src/node/module_loader.rs)`). **Bare specifiers** split package name and subpath (e.g. `lodash/get`, `@scope/pkg/x`) before resolving under `node_modules`. `**package.json`** is parsed as JSON; `**"exports"`** supports the `"."` and subpath keys, conditional objects (including nested `node` groups), string/array targets, and a single `*` per pattern key/target pair; `**"imports"`** resolves internal `#…` specifiers from the nearest `package.json`. `**development` / `production`** conditions follow `**process.env.NODE_ENV`** as visible in JS (refreshed before module resolution / `import` loads), defaulting to `production` when unset. Edge cases not matching Node 23 exactly include some multi-star or highly unusual export maps. ESM `import`/`export` is handled by the QuickJS module loader when the entry or a dependency is ESM (see `[core/src/node/mod.rs](../core/src/node/mod.rs)`).
- **Native addons:** `*.node` / N-API native modules are **not** supported.
- **Policy:** `child_process` is disabled unless `KAWKAB_ALLOW_CHILD_PROCESS=1`.

Use this file plus a quick smoke test for any package you care about.

## Implementation reference (source of truth)


| Area                                                                      | Location                                                                                                                                                                                                |
| ------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `require()` / `require('node:…')` branches                                | `[js_require` in `core/src/node/mod.rs](../core/src/node/mod.rs)` (built-in name matching after stripping the `node:` prefix)                                                                           |
| Runtime bootstrap, primed shims, globals                                  | `[install_runtime` in `core/src/node/mod.rs](../core/src/node/mod.rs)`                                                                                                                                  |
| Web platform globals (`Blob`, streams, `CompressionStream`, messaging, …) | `[core/src/node/web_platform_shim.rs](../core/src/node/web_platform_shim.rs)` + `[web_platform_shim.js](../core/src/node/web_platform_shim.js)` (after `[buffer::install](../core/src/node/buffer.rs)`) |
| Path resolution, `package.json`, ESM/CJS detection                        | `[core/src/node/module_loader.rs](../core/src/node/module_loader.rs)`, `[esm_loader](../core/src/node/)`                                                                                                |
| CLI: order of setup, bytecode CJS wrapper                                 | `[kawkab/src/main.rs](../kawkab/src/main.rs)`                                                                                                                                                           |


**Built-in names handled inside `js_require` today:** `assert`, `buffer`, `console`, `process`, `fs`, `path`, `os`, `punycode`, `events`, `util`, `sys`, `net`, `http`, `https`, `child_process`, `stream`, `crypto`, `url`, `querystring`, `string_decoder`, `dgram`, `diagnostics_channel`, `dns`, `dns/promises`, `readline`, `zlib`, `tls`, `vm`, `worker_threads`, `timers`, `timers/promises`, `perf_hooks`, `module`, `node:test`, `test`. Any other specifier falls through to file / `node_modules` resolution (or fails).

## Status Legend

- `🟢` **Typical npm / corpus coverage** per `[COMPAT_DEFINITION_OF_DONE.md](COMPAT_DEFINITION_OF_DONE.md)` (not byte-for-byte Node v23); each row lists **Remaining vs Node** where relevant.
- `🟡` Partially implemented / simplified / important caveats
- `🔴` Not implemented

## Built-in Node.js Modules

- `node:assert` — `🟢` Core API implemented: `AssertionError` exported as a native constructor (`new assert.AssertionError(...)` matches Node’s shape); failed assertions from native `assert.*` helpers throw `Error` values with `name === 'AssertionError'` and `code === 'ERR_ASSERTION'` (not guaranteed to be `instanceof assert.AssertionError`); `equal`/`notEqual` use real abstract equality (`==` via `__kawkabLooseEq`, plus a nullish fast-path); `deepEqual`/`deepStrictEqual` recurse arrays, plain objects, `Date`, `RegExp`, and detect common cycles; `throws` accepts optional RegExp / validator function / constructor filter; `match` / `rejects` implemented (`rejects` returns a Promise).
  - **Remaining vs Node v23:** full per-method options objects (`message`/`operator`/`stackStartFn` everywhere), `assert.strict` / `CallTracker` / `partialDeepStrictEqual`, and byte-for-byte `util.isDeepStrictEqual` edge-case parity (e.g. `Map`/`Set`/typed arrays).
- `node:buffer` — `🟢` `require('buffer')` re-exports the **global** `Buffer` constructor (and `SlowBuffer`, `kMaxLength`, `constants`) installed from `[core/src/node/buffer.rs](../core/src/node/buffer.rs)`: `Uint8Array` subclass with native helpers (`from`, `alloc`, `concat`, etc.). Automated check: `priority_builtins_green_contract` in `[core/src/node/compat_contract_tests.rs](../core/src/node/compat_contract_tests.rs)`.
  - **Remaining vs Node v23:** full `Buffer` method matrix, `transcode`, pooled `SlowBuffer`, and edge-case encoding / `INSPECT_MAX_BYTES` parity.
- `node:console` — `🟢` `require('console')` / `require('node:console')` resolves to the same object as `globalThis.console` (installed from `[core/src/console.rs](../core/src/console.rs)` on the CLI). Typical logging and the usual method names; not full Node v23 `console` module parity.
  - **Remaining vs Node v23:** full `Console` constructor options, `trace`/`table`/`dir` depth, `inspectOptions`, and `Console` prototype alignment.
- `node:dgram` — `🟡` Real UDP I/O (Rust): `createSocket` supports `bind` / `send` / `close` / `address`, `message` events with `(msg, rinfo)`, and listener helpers (`on`, `addListener`, `once`, `removeListener`, `removeAllListeners`). Multicast / TTL helpers are compatibility no-ops.
  - **Remaining vs Node v23:** full `bind`/`send` overload matrix, membership APIs, error surface, and test-suite parity.
- `node:diagnostics_channel` — `🟢` Runtime surface: `channel(name)`, `hasSubscribers(name)`, `subscribe(name, fn)`, `unsubscribe(name, fn)`, `tracingChannel(nameOrChannels)`, `boundedChannel(nameOrChannels)`; channel objects expose `publish`, `subscribe`, `unsubscribe`, `bindStore`, `unbindStore`, `runStores`, and a `**hasSubscribers` boolean property** (updated when subscribers change; not the same shape as Node’s getter in every edge case).
- `node:dns` — `🟢` Compatibility-oriented resolver: callback APIs `lookup` (numeric/object options and `{ all: true }`), `resolve(host, [rrtype], cb)`, `resolve4`/`resolve6`/`resolveAny`/`resolveCaa`/`resolveCname`/`resolveMx`/`resolveNaptr`/`resolveNs`/`resolvePtr`/`resolveSoa`/`resolveSrv`/`resolveTxt`, `reverse`, `lookupService`, `getServers`/`setServers`, and `Resolver` with matching methods.
  - `**dns/promises`:** `🟢` Promise-based surface for the same family (`lookup`, `resolve*`, `reverse`, `lookupService`, `Resolver`).
- `node:events` — `🟡` Expanded compatibility shim: `EventEmitter` supports `on`/`addListener`, `off`/`removeListener`, `once`, `prependListener`, `prependOnceListener`, `removeAllListeners`, `emit` (variadic args), `listenerCount` (instance + static helper), `listeners`, `rawListeners`, `eventNames` (including `Symbol` keys), and max-listener controls (`setMaxListeners` / `getMaxListeners`) with leak-warning behavior.
  - **To reach `🟢`:** full Node v23 equivalence for nuanced listener lifecycle and warning/error edge-cases (including exact async-iterator semantics, full helper parity, and Node test-suite byte-for-byte behavior).
- `node:fs` — `🟢` Sync APIs: `readFileSync`, `writeFileSync`, `copyFileSync`, `rmSync` (optional `{ recursive, force }`), `existsSync`, `mkdirSync`, `readdirSync`, `unlinkSync`, `rmdirSync`, `statSync`. `**fs.promises`:** `readFile`, `writeFile`, `stat`, `readdir`, `mkdir`, `unlink` (host async path with sync fallback when no Tokio handle). Corpus/contract: `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full async API surface, watch streams, full `open`/`fd` matrix, permission/ACL models, and error `code` parity.
- `node:http` — `🟢` Baseline server APIs (`createServer`/`listen`/`close`) with simplified request/response model. **Client:** blocking `get` / `request` (URL string + callback) via **reqwest**; callback runs synchronously after the full body is read; response is a plain object with `statusCode`, `statusMessage`, `headers`, and `**body`** as `ArrayBuffer` (not a streaming `IncomingMessage`). Contract: live `http.get('http://example.com')` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** streaming `IncomingMessage`/`ClientRequest`, full `request` options object, `Agent`, and WebSocket upgrades.
- `node:https` — `🟢` Same server baseline as `node:http` (**no TLS** on `createServer`). **Client:** HTTPS URLs use **rustls** through reqwest; same non-streaming response shape as `http`. Contract: `https.get('https://example.com')` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** TLS `createServer`, client cert pinning/options matrix, ALPN/h2, streaming parity with `http`.
- `node:os` — `🟢` Baseline for typical npm: `platform`, `tmpdir`, `homedir`, `arch`, `endianness`, `release` (best-effort via `uname` on Unix), `cpus` (length from `available_parallelism`, stub `model`/`speed`/`times`), `totalmem` / `freemem` (Unix `sysconf`; Linux `MemAvailable` when `/proc/meminfo` is readable), `**EOL`**, `**loadavg`** (Linux `/proc/loadavg` or `getloadavg` when available), `**networkInterfaces**` (empty object stub).
  - **Remaining vs Node v23:** real interface listing, full CPU times, signal constants, and byte-exact memory figures.
- `node:path` — `🟢` `join`, `dirname`, `basename`, `extname`, `resolve`, `normalize`, `relative`, `parse`, `sep`, `delimiter`. Contract: `path.join` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** `posix`/`win32` namespaces, `pathToFileURL`, `fileURLToPath`, and full Windows long-path edge cases.
- `node:punycode` — `🟡` `decode`/`encode`: ASCII-oriented baseline (unchanged). `**toASCII` / `toUnicode`:** IDNA via `**idna`** crate (Unicode domains / ACE), closer to Node’s domain handling than the old ASCII-only stub.
  - **Remaining vs Node:** full RFC 3492 punycode module parity for non-domain strings, edge-case errors.
- `node:querystring` — `🟢` `parse` / `stringify` with optional `sep`, `eq`, and `options.maxKeys`; repeated-key arrays; `**escape`** / `**unescape`** (legacy helpers). Automated checks in `web_platform_and_builtins_baseline_contract`.
  - **Remaining vs Node v23:** full option matrix (`decodeURIComponent` override, etc.) and byte-for-byte Node test-suite equivalence.
- `node:readline` — `🟢` Non-interactive stub for CI and feature detection: `createInterface` exposes `input`/`output`/`terminal`, `question` (async empty answer), `prompt`, `on`/`once`/`off`, `close`, `pause`/`resume`, `write`, `setPrompt`. Interactive REPL is not implemented.
  - **Remaining vs Node v23:** real TTY/stream integration, history, and `readline`/`promises` subpath.
- `node:stream` — `🟢` Baseline behavior (`Readable`, `Writable`, `Duplex`, `Transform`) including basic `pipe`/`data`/`end` (primed shim in `[mod.rs](../core/src/node/mod.rs)`). Contract: `Readable` push/end in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full backpressure / object mode / `pipeline` / async iterator parity and Node stream test-suite behavior.
- `node:string_decoder` — `🟢` `StringDecoder` with `**write` / `end`**, encoding from ctor (`utf8` default), Buffer / Uint8Array input via native byte path, carry across chunks for UTF-8 and UTF-16LE / `**ucs2`**, plus **latin1** / **binary** / **ascii**. Contract: UTF-8 `€` reassembly in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full encoding list (`base64`, `hex`, …), `TextDecoder` alignment, and Node test-suite edge cases.
- `node:timers` — `🟢` `require('timers')` exports `setTimeout`, `clearTimeout`, `setInterval`, `clearInterval`, `setImmediate`, `clearImmediate` bound to the same native implementations as globals. Without a Tokio task sender, `setTimeout(fn, 0)` runs the callback inline after a host sleep (see `schedule_timer_inner` in `[mod.rs](../core/src/node/mod.rs)`). Contract: `timers.setTimeout` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full event-loop ordering vs I/O, `ref`/`unref`, and exact delay/immediate ordering under load.
  - **Subpath `timers/promises`:** `🟢` `setTimeout(delay[, value])`, `setImmediate([value])`, and `**setInterval(delay[, value])`** returning an **async iterable** (`next()` / `return()` / `Symbol.asyncIterator`) backed by the same Promise scheduler as `setTimeout`. Contract: export present in `web_platform_and_builtins_baseline_contract`.
  - **Remaining vs Node v23:** ref/unref, scheduler ordering vs I/O, and full async-iterator edge cases.
- `node:tty` — `🟡` Compatibility stub: `isatty(fd)` plus `ReadStream` / `WriteStream` constructors for feature detection and guarded flows.
- `node:url` — `🟢` Baseline `URL`/`URLSearchParams` behavior (`append`, `set`, `get`, `getAll`, `delete`, `toString`, computed `href`) via primed shim. Contract: `URL` + `searchParams` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full WHATWG URL normalization, `pathToFileURL`, legacy `url.parse` / `format` if required by corpus.
- `node:zlib` — `🟡` Native sync helpers: `gzipSync`, `gunzipSync`, `deflateSync`, `inflateSync` (via `flate2`). No streaming API or full deflate/gzip option matrix vs Node.
- `node:async_hooks` — `🟢` Compatibility baseline via shim: `AsyncLocalStorage` (`run`, `enterWith`, `getStore`, `exit`, `bind`, `disable`), `AsyncResource` (constructor + `runInAsyncScope`) and helper exports (`executionAsyncId`, `triggerAsyncId`, `executionAsyncResource`, `createHook`, `AsyncLocalStorage.bind`).
  - **Remaining vs Node v23:** full async context propagation semantics across all host async boundaries, exact hook lifecycle ordering, and byte-for-byte test-suite parity.
- `node:child_process` — `🟢` `execSync` and `spawnSync` available behind policy gate (`KAWKAB_ALLOW_CHILD_PROCESS=1`). Contract: `execSync('echo …')` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** async `spawn`/`exec`, stdio streams, `fork`, `execFile`, signal handling, and shell option parity when policy allows.
- `node:cluster` — `🟡` Compatibility stub for feature detection (`isPrimary`/`isWorker`, `setupPrimary`, `fork`, worker shape/event helpers). No real multi-process orchestration.
- `node:crypto` — `🟢` Loaded via primed JS shim (`CRYPTO_SHIM_SRC`) plus native `__kawkabCrypto*` helpers: `createHash` / `createHmac` with `update` and `digest('hex' | 'base64' | default Buffer)`; `createHash` supports `**sha1`**, `**sha256`**, `**sha384**`, `**sha512**`, `**md5**`, `**blake3**`; `randomBytes` sync and callback/async (Promise-backed) forms returning `Buffer`. Contract: `createHash('sha256')` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** ciphers, `createSign`/`verify`, ECDH, `subtle`, Web Crypto alignment, and full OpenSSL-compatible option matrices.
- `node:domain` — `🟡` Compatibility stub (`create`, `createDomain`, `Domain` with `run`/`bind`/`intercept`/`dispose`) for guarded package init paths.
- `node:http2` — `🟡` Compatibility stub (`createServer`, `createSecureServer`, `connect`) with server/client/request object shapes for feature detection.
- `node:module` — `🟡` `**createRequire(filename | file URL)`** returns a `require` function with resolution rooted at the parent directory of `filename` (after stripping a `file://` prefix). Other `module` APIs (`enableCompileCache`, `Module`, sync hooks, etc.) are not implemented.
- `node:net` — `🟢` Compatibility entrypoint: same `createServer` shape as `http` for baseline usage. Contract: `createServer` export in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** real `Socket`/`Server` TCP lifecycle, `connect`, TLS bridging, and `BlockList`/`SocketAddress` APIs.
- `node:perf_hooks` — `🟢` Exports global `performance` (`now`, `timeOrigin`), `**PerformanceObserver`**, minimal `**PerformanceEntry`**, `**PerformanceMark**`, `**PerformanceMeasure**`, `**PerformanceResourceTiming**`, `**PerformanceObserverEntryList**`, `**constants**`, and `**nodeTiming`{}** for typical feature detection. `globalThis` also defines `**PerformanceEntry`**, `**PerformanceMark`**, `**PerformanceMeasure**` (see Globals).
  - **Remaining vs Node v23:** real timing entries, histograms, full `PerformanceObserver` behavior, and `nodeTiming` metrics.
- `node:process` — `🟢` The `**process` object is installed as a global** during `install_runtime` (`argv`, `env`, `cwd`, `nextTick`, timing helpers, `stdout`/`stderr` via `process::install_stdio`, etc.). `**require('process')` / `require('node:process')`** returns a duplicate of the global `process` binding (`js_dup_value`) for packages that expect the module form. Contract: `process.cwd` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** `emit`, `binding`, full `versions`/`release`, `permission`, `dlopen`, and internal fields used by advanced tooling.
- `node:sys` — `🟢` Legacy alias: same exports as `node:util` (see `node:util`).
- `node:tls` — `🟡` Baseline API surface (`connect`, `createServer`) as compatibility placeholders.
  - **To reach `🟢`:** real TLS handshake/session behavior and Node option parity.
- `node:util` — `🟢` `inspect`, `**types`**: `isDate`, `isArrayBuffer`, `isString`, `isObject`, `isFunction`, `isNumber`, `isBoolean`, `**isNull`**, `**isUndefined**`, `**isRegExp**`, `**isBuffer**` (delegates to `Buffer.isBuffer`). `**promisify**` when `globalThis.__kawkabUtilPromisify` installs at bootstrap. Contract: `web_platform_and_builtins_baseline_contract` / `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full `types` matrix, `debuglog`, `promisify` custom symbols, and complete `inspect` options.
- `node:v8` — `🟡` Compatibility stub (`cachedDataVersionTag`, `getHeapStatistics`, `setFlagsFromString`).
- `node:vm` — `🟡` Baseline API surface (`runInThisContext`, `runInNewContext`, `Script`, `createContext`, `isContext`) with simplified semantics.
  - **Current behavior:** `runInNewContext(code, sandbox)` maps own string properties of `sandbox` to `Function` parameters and evaluates `code` as `return (<code>);` inside that function (expression-oriented, not full Node `Script` / VM isolation).
  - **To reach `🟢`:** isolated context guarantees and broader script/module execution parity.
- `node:wasi` — `🟡` Compatibility stub (`WASI`) with baseline instance methods (`start`, `initialize`) and `wasiImport` preview object shape.
- `node:worker_threads` — `🟢` **Baseline workers on real OS threads:** each `Worker` runs in its own **QuickJS isolate** on a dedicated Rust thread (`core/src/node/worker_threads.rs`). Main thread: `Worker` constructor (script path), `postMessage` / `on('message')`, `terminate`, `isMainThread`, `threadId`. Worker isolate: `parentPort.postMessage` / `parentPort.on('message')`. Messages are **JSON text** (values must round-trip via `JSON.stringify` / `JSON.parse`; otherwise a `TypeError` is thrown). Nested `Worker` from inside a worker is rejected. Contract: `node::compat_contract_tests::worker_threads_roundtrip` (and idle spawn smoke).
  - **Remaining vs Node:** full **structured clone** (including `ArrayBuffer`, `SharedArrayBuffer`, transfer lists), `new Worker(new URL(...))` / `import.meta.url`, `workerData`, wiring **global** `MessageChannel` / `MessagePort` / `BroadcastChannel` to cross-thread semantics (today globals are main-isolate baseline; ports use in-memory synchronous delivery), `resourceLimits`, `markAsUntransferable`, `receiveMessageOnPort`, `setEnvironmentData` / `getEnvironmentData`, `performance` in worker, `eventLoopUtilization`, and full `events`/EventEmitter parity on `Worker`.
- `node:inspector` — `🟡` Compatibility stub (`open`, `close`, `url`, `waitForDebugger`, `Session`) for tooling detection.
- `node:repl` — `🟡` Compatibility stub (`start`) returning a minimal REPL-server shape (`context`, `defineCommand`, `close`, event helpers).
- `node:sqlite` — `🟡` Compatibility stub (`Database`, `Statement`) with baseline `prepare`/`exec`/`run`/`get`/`all`/`finalize` shapes.
- `node:test` / `test` — `🟢` Baseline (`test`, `it`, `describe`, `before`, `after`, `**beforeEach`**, `**afterEach`**) invoking the callback when arity allows (same helper as `it`). Contract: hook exports in `web_platform_and_builtins_baseline_contract`.
  - **Remaining vs Node v23:** runner semantics, mocks/snapshots/timers parity, and full `node:test` API.
- `node:trace_events` — `🟡` Compatibility stub (`createTracing`, `getEnabledCategories`) for feature detection.

## Node.js Globals

- `AbortController` — `🟢` Fully implemented.
- `AbortSignal` — `🟢` Fully implemented.
- `Blob` — `🟡` Baseline (`size`, `type`, `slice`, `arrayBuffer`, `text`) via `[web_platform_shim](../core/src/node/web_platform_shim.js)`; UTF-8 string parts; nested `Blob` parts supported. Contract: `web_platform_and_builtins_baseline_contract`.
  - **To reach `🟢`:** full Web `Blob` / file API parity and stream integration.
- `Buffer` — `🟢` Global constructor installed with `[core/src/node/buffer.rs](../core/src/node/buffer.rs)` (`Uint8Array` subclass + native helpers). Same **Remaining vs Node** as `node:buffer`.
- `ByteLengthQueuingStrategy` — `🟡` Constructor with `highWaterMark` and `size(chunk)` baseline (installed with web streams).
- `__dirname` — `🟢` Fully implemented.
- `__filename` — `🟢` Fully implemented.
- `atob()` — `🟡` Baseline implemented (Web-style base64 decode to binary string).
  - **To reach `🟢`:** full WebIDL error behavior and edge-case parity with Node.
- `Atomics` — `🟡` Deterministic shim installed on `globalThis` for typed-array atomic-style operations (`load`, `store`, `add`, `sub`, `and`, `or`, `xor`, `exchange`, `compareExchange`, `isLockFree`, `notify`). `wait`/`waitAsync` intentionally throw in this embedding.
- `BroadcastChannel` — `🟡` In-process channels by name (`postMessage`, `close`, `EventTarget` listeners); **synchronous** delivery on the same isolate/thread (differs from browser timing).
- `btoa()` — `🟡` Baseline implemented (binary string to base64).
  - **To reach `🟢`:** full WebIDL error behavior and edge-case parity with Node.
- `clearImmediate()` — `🟢` Fully implemented.
- `clearInterval()` — `🟢` Fully implemented.
- `clearTimeout()` — `🟢` Fully implemented.
- `CompressionStream` — `🟡` `gzip` and `deflate` via native `__kawkabWebCompress` (full-stream flush: chunks buffered until writable closes, then one compressed block). Contract: gzip round-trip in `web_platform_and_builtins_baseline_contract`.
  - **To reach `🟢`:** true chunk streaming, `deflate-raw`, and byte-for-byte Web Compression Streams parity.
- `console` — `🟡` On the `**kawkab` CLI**, `[core::console::install](../core/src/console.rs)` runs before runtime bootstrap and wires `**log` / `error` / `warn` / `info` / `debug`** on `globalThis.console`. `require('console')` / `require('node:console')` exposes the same object.
- `CountQueuingStrategy` — `🟡` Constructor with `highWaterMark` and `size()` returning `1`.
- `Crypto` — `🔴` Not implemented.
- `SubtleCrypto (crypto)` — `🔴` Not implemented.
- `CryptoKey` — `🔴` Not implemented.
- `CustomEvent` — `🟡` Extends `Event`; `detail` from init object.
- `DecompressionStream` — `🟡` Same buffering/flushing model as `CompressionStream`, using `__kawkabWebDecompress`.
- `Event` — `🟡` Baseline (`type`, `bubbles`, `cancelable`, `preventDefault`, `stopPropagation`, `stopImmediatePropagation`, `timeStamp`).
- `EventTarget` — `🟡` `addEventListener`, `removeEventListener`, `dispatchEvent` (capture/bubble keys simplified).
- `exports` — `🟢` Fully implemented.
- `fetch` — `🟡` Compatibility shim available (non-standard backend behavior).
  - **To reach `🟢`:** standards-compliant request/response lifecycle and full fetch semantics.
- `FormData` — `🟡` `append`, `delete`, `get`, `getAll`, `has`, `set` (string values for `get`/`getAll`; `Blob` values summarized as strings).
- `global` — `🟢` Implemented.
- `globalThis` — `🟢` Implemented.
- `Headers` — `🟡` Compatibility shim available.
  - **To reach `🟢`:** complete headers normalization/iteration semantics parity.
- `MessageChannel` — `🟡` `port1` / `port2` linked; `postMessage` delivers `MessageEvent` on the peer **synchronously** in this embedding.
- `MessageEvent` — `🟡` `data`, `origin`, `lastEventId` on the event object.
- `MessagePort` — `🟡` extends `EventTarget`; `postMessage`, `start`, `close` (baseline).
- `module` — `🟢` Fully implemented (CommonJS runtime context).
- `PerformanceEntry` — `🟢` Minimal constructor on `globalThis` (feature detection); same shape exposed from `node:perf_hooks`.
- `PerformanceMark` — `🟢` Minimal constructor on `globalThis` and `node:perf_hooks`.
- `PerformanceMeasure` — `🟢` Minimal constructor on `globalThis` and `node:perf_hooks`.
- `PerformanceObserver` — `🟡` Minimal stub from `require('node:perf_hooks')` only (not on `globalThis` by default).
- `PerformanceObserverEntryList` — `🟢` Stub constructor on `node:perf_hooks` (empty implementation).
- `PerformanceResourceTiming` — `🟡` Stub constructor from `node:perf_hooks` only (not on `globalThis`).
- `performance` — `🟢` `performance.now()` and `performance.timeOrigin` plus companion `**PerformanceMark` / `PerformanceMeasure` / `PerformanceEntry`** globals for typical feature detection (not full high-resolution / histogram parity).
  - **Remaining vs Node v23:** full `Performance` object surface, resource timing, and `perf_hooks` integration.
- `process` — `🟢` Mostly implemented with compatibility caveats (see `node:process`).
  - **Remaining vs Node:** same bullets as `node:process`.
- `queueMicrotask()` — `🟢` Implemented.
- `ReadableByteStreamController` — `🟡` Illegal-constructor placeholder (feature detection only).
- `ReadableStream` — `🟡` Queue-based baseline: `start` controller with `enqueue`/`close`/`error`, `getReader()` with `read()` returning Promises, `cancel`/`releaseLock` stubs.
  - **To reach `🟢`:** full WHATWG streams (pull/cancel, BYOB, tee, async iterator).
- `ReadableStreamBYOBReader` — `🟡` Illegal-constructor placeholder.
- `ReadableStreamBYOBRequest` — `🟡` Illegal-constructor placeholder.
- `ReadableStreamDefaultController` — `🟡` Illegal-constructor placeholder.
- `ReadableStreamDefaultReader` — `🟡` Illegal-constructor placeholder.
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
- `DOMException` — `🟡` Minimal (`Error` subclass with `name` / `message`; `code` stub `0`).
- `TextDecoder` — `🟡` Compatibility shim available.
  - **To reach `🟢`:** complete encoding/error-mode parity for all supported encodings.
- `TextDecoderStream` — `🟡` Baseline UTF-8 transform stream available (chunk aggregation + UTF-8 decode on flush).
- `TextEncoder` — `🟡` Compatibility shim available.
  - **To reach `🟢`:** complete byte-level parity in all edge cases.
- `TextEncoderStream` — `🟡` Baseline transform stream available (string chunks to UTF-8 bytes).
- `TransformStream` — `🟡` Default `readable`/`writable` pair; `transform`/`flush` controller hooks; pairs with baseline `WritableStream`/`ReadableStream`.
- `TransformStreamDefaultController` — `🟡` Illegal-constructor placeholder.
- `URL` — `🟡` Compatibility constructor exposed.
  - **To reach `🟢`:** full WHATWG URL conformance and edge-case normalization parity.
- `URLSearchParams` — `🟡` Compatibility constructor exposed.
  - **To reach `🟢`:** full iteration/sorting/encoding parity with Node.
- `WebAssembly` — `🟡` Compatibility baseline available for feature detection and guarded execution paths (`Module`, `Instance`, `Memory`, `Table`, `Global`, `validate`, `compile`, `instantiate`, and error constructors). Not full Node/engine parity.
- `WritableStream` — `🟡` `getWriter()` with `write`/`close`/`abort`/`releaseLock`; underlying sink optional.
- `WritableStreamDefaultController` — `🟡` Illegal-constructor placeholder.
- `WritableStreamDefaultWriter` — `🟡` Illegal-constructor placeholder.

## Compatibility coverage (reference scripts)

Reference scripts under `examples/` were removed. When you add new checks, list them in `[docs/NPM_CORPUS.md](NPM_CORPUS.md)`. Workspace tests and `[scripts/compat_smoke.sh](../scripts/compat_smoke.sh)` provide a repeatable baseline.

## Notes

- This status reflects current implementation in this repository and intentionally marks unsupported Node APIs as `🔴`.
- The compact snapshot in `README.md` is a quick overview; **this file is the detailed reference** and should stay aligned with `js_require` / `install_runtime`.
- If a module/global is added or upgraded, update this file and `docs/FEATURE_BASELINE.md` in the same change.
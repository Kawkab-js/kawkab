# Kawkab Node.js Compatibility

Kawkab expands **best-effort** alignment with Node.js **built-in names and rough API shape** over time; this is **not** a promise that every npm package will run unchanged. This page tracks API/module/global compatibility status in one place and is intended to be updated frequently.

Compatibility target in this document is aligned to **Node.js v23** surface area for *naming and expectations*, not byte-for-byte parity with Node or universal registry coverage. For product-level scope, see `[PRODUCT_VISION.md](PRODUCT_VISION.md)`. For **🟢/🟡 definitions**, see `[COMPAT_DEFINITION_OF_DONE.md](COMPAT_DEFINITION_OF_DONE.md)`. For **explicit non-goals / 🔴 stance**, see `[NODE_NON_GOALS.md](NODE_NON_GOALS.md)`.

Quick navigation:
- [Docs index](INDEX.md)
- [What npm compatibility means here](#what-npm-compatibility-means-here)
- [Implementation reference (source of truth)](#implementation-reference-source-of-truth)
- [Status Legend](#status-legend)
- [Built-in Node.js Modules](#built-in-nodejs-modules)
- [Node.js Globals](#nodejs-globals)
- [Compatibility coverage (reference scripts)](#compatibility-coverage-reference-scripts)
- [Yellow to Green Execution Plan](#yellow-to-green-execution-plan)
- [Notes](#notes)

Related decision docs:
- Central docs index: `INDEX.md`.
- Behavioral/runtime baseline contract: `FEATURE_BASELINE.md`.
- KPI targets and pass semantics: `COMPAT_KPI.md`.
- Scenario corpus and smoke rows: `NPM_CORPUS.md`.
- Promotion semantics (`🟢/🟡/🔴`): `COMPAT_DEFINITION_OF_DONE.md`.
- Release gates and verification flow: `RELEASE_CHECKLIST.md`.
- Explicit non-goals and out-of-scope commitments: `NODE_NON_GOALS.md`.

## What npm compatibility means here

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

- `🟢` **Typical npm / corpus coverage** per `[COMPAT_DEFINITION_OF_DONE.md](COMPAT_DEFINITION_OF_DONE.md)` (not byte-for-byte Node v23); each row lists **Remaining vs Node v23** where relevant.
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
  - Helper APIs `events.once(emitter, event)` and `events.on(emitter, event)` are available for baseline async helper flows.
  - **Remaining vs Node v23:** full equivalence for nuanced listener lifecycle and warning/error edge-cases (including exact async-iterator semantics, full helper parity, and Node test-suite byte-for-byte behavior).
- `node:fs` — `🟢` Sync APIs: `readFileSync`, `writeFileSync`, `copyFileSync`, `rmSync` (optional `{ recursive, force }`), `existsSync`, `mkdirSync`, `readdirSync`, `unlinkSync`, `rmdirSync`, `statSync`. `**fs.promises`:** `readFile`, `writeFile`, `stat`, `readdir`, `mkdir`, `unlink` (host async path with sync fallback when no Tokio handle). Corpus/contract: `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full async API surface, watch streams, full `open`/`fd` matrix, permission/ACL models, and error `code` parity.
- `node:http` — `🟢` Baseline server APIs (`createServer`/`listen`/`close`) with simplified request/response model. **Client:** blocking `get` / `request` via **reqwest**; `request` accepts URL and baseline options-object forms (`protocol`/`hostname`/`host`/`path`/`pathname`/`search`/`method`/`headers`/`auth`/`timeout`) and returns a minimal `ClientRequest` shape (`write`/`end`/`setHeader`/`appendHeader`/`getHeader`/`hasHeader`/`getHeaders`/`getHeaderNames`/`getRawHeaderNames`/`removeHeader`/`flushHeaders`/`setTimeout`/`setNoDelay`/`setSocketKeepAlive`/`cork`/`uncork`/`setDefaultEncoding`/`setMaxListeners`/`getMaxListeners`/`addListener`/`prependListener`/`prependOnceListener`/`off`/`removeAllListeners`/`listenerCount`/`eventNames`/`listeners`/`rawListeners`/`abort`/`destroy`/`on`/`once`) including baseline flags (`aborted`, `destroyed`, `closed`, `errored`, `reusedSocket=false`, `finished`, `headersSent`, `_headerSent`, `_header`, `writableCorked`, `writableEnded`, `writableFinished`, `writableDefaultEncoding='utf8'`, `maxHeadersCount=null`), `agent` (`false` by default unless passed in options), request-side `socket`/`connection` aliases (baseline socket-like object with inspectable state fields `noDelay`, `keepAlive`, `keepAliveInitialDelay`, `timeout` (default `0`, updated by `request.setTimeout()` and `socket.setTimeout()`), `destroyed`, `closed`, `readyState` (`open` -> `closed`), `readable`/`writable` (`true` -> `false` on `end/abort`), `connecting=false`, `pending=false`, `encrypted` (protocol-aware: `false` for `http:`, `true` for `https:`), `authorized=false`, `authorizationError=null`, `alpnProtocol=''`, `servername`, `remoteAddress`, `remoteFamily='IPv4'`, `remotePort`, `localAddress`, `localFamily='IPv4'`, `localPort` placeholders, `bytesRead`, `bytesWritten`, and `address()` returning `{ address, family, port }`; `socket.setNoDelay()` and `socket.setKeepAlive()` now update `noDelay`/`keepAlive`/`keepAliveInitialDelay` directly), and baseline request metadata fields (`protocol`, `hostname`, `host`, `port`, `path`, `method`). `flushHeaders()` marks `headersSent=true` and `_headerSent=true`; `cork()/uncork()` update a baseline `writableCorked` counter. Header helpers remain baseline (`getHeaderNames()`/`getRawHeaderNames()` expose currently configured header keys). `setHeader(name, valueArray)` is baseline-supported by normalizing array values to a comma-separated string; `appendHeader(name, value)` is baseline-supported by comma-appending to existing values. Listener-limit helpers are baseline-supported (`setMaxListeners`/`getMaxListeners`, default 10), and EventEmitter aliases/introspection are baseline-supported (`addListener`, `prependListener`, `prependOnceListener`, `off`, `removeAllListeners`, `listenerCount`, `eventNames`, `listeners`, `rawListeners`). Abort/event semantics are baseline (`abort` + `abort` event), request lifecycle sets `closed=true` in `end()`/`abort()` paths and mirrors socket lifecycle via `socket.destroyed=true`, `socket.closed=true`, `socket.readyState='closed'`, and `socket.readable/socket.writable=false` on `end()`/`abort()`, and `errored` is `null` initially then set to an error string on sync request-path failures. Request body chunks are accumulated from `write`/`end` and sent when `end` is called, with baseline socket counters (`bytesWritten` from request body size and `bytesRead` from response body size). Callback runs after the full body is read; response is a plain object with `statusCode`, `statusMessage`, `url`, `req` (request reference on `request` path), `httpVersion`/`httpVersionMajor`/`httpVersionMinor`, `complete`, `aborted=false`, `headers`, `rawHeaders`, `trailers`, `rawTrailers`, `socket`/`connection` (same baseline socket-like object), and `**body`** as `ArrayBuffer` (not a streaming `IncomingMessage`) and includes baseline `setEncoding()`. Contract: live `http.get('http://example.com')` in `priority_builtins_green_contract`.
  - Quick readability map:
    - **Request surface:** header helpers, lifecycle controls, timeout/socket tuning, EventEmitter aliases/introspection.
    - **Socket surface:** `socket`/`connection` alias with state introspection and chainable control helpers.
    - **Response surface:** plain-object response (non-streaming) with status/header metadata and `ArrayBuffer` body.
    - **Validation strategy:** shape-first contract assertions with safe incremental behavior checks.
  - Contract execution status: `http_client_local_behavior_contract` is currently marked `#[ignore]` (`unstable; pending deeper FFI/runtime hardening`), so compile coverage is green while deep runtime execution remains a dedicated hardening track.
  - Request-side socket also exposes baseline `setEncoding()` as a chainable no-op helper, plus lifecycle helpers `end([data])` and `destroy([err])` that update socket state (`destroyed`/`readyState`/`readable`/`writable`) for compatibility with socket-level control paths.
  - Socket introspection includes baseline `bufferSize` and `writableLength` (both start at `0`, increase with `request.write()`, and reset to `0` on `end()`/`abort()` paths).
  - Socket connection state now follows a baseline transition: `connecting=true` / `pending=true` at creation, then switches to `false` when request dispatch starts (`end()`), on `socket.end()/socket.destroy()`, or when request is aborted.
  - Contract coverage now includes a direct `request.end()` lifecycle assertion baseline (`writableEnded/finished/closed=true`, `headersSent/_headerSent=true`, and socket `connecting/pending=false`).
  - Contract coverage now includes a safe standalone `flushHeaders()` assertion baseline before `end()` (`headersSent/_headerSent=true`) with stable lifecycle continuation.
  - Contract coverage now includes a safe timeout baseline: `request.setTimeout(ms)` reflects onto `request.socket.timeout`, and direct `socket.setTimeout(ms)` updates the same field.
  - Contract coverage now includes safe request-level socket-tuning baselines: `request.setNoDelay(flag)` reflects onto `request.socket.noDelay`, and `request.setSocketKeepAlive(flag, delay)` reflects onto `request.socket.keepAlive`/`keepAliveInitialDelay`.
  - Contract coverage now includes a safe default-encoding baseline: `request.setDefaultEncoding(enc)` updates `request.writableDefaultEncoding`.
  - Contract coverage now includes safe corking baseline transitions: `request.cork()` increments `writableCorked` and `request.uncork()` decrements it back (floored at `0`).
  - Contract coverage now includes a safe listener-limit baseline: `request.setMaxListeners(n)` round-trips via `request.getMaxListeners()`.
  - Contract coverage now includes safe EventEmitter introspection baseline on `ClientRequest`: adding one listener yields `listenerCount(event)===1` and `eventNames()` includes that event.
  - Contract coverage now includes safe `removeAllListeners(event)` baseline on `ClientRequest`: listener count returns to `0` and `eventNames()` no longer includes that event.
  - Contract coverage now includes safe `off/removeListener(event, fn)` baseline on `ClientRequest`: removing the exact callback drops `listenerCount(event)` to `0` and removes the event from `eventNames()`.
  - Contract coverage now includes safe `listeners(event)` / `rawListeners(event)` baseline on `ClientRequest` with expected array shape/length after `on`.
  - Contract coverage now includes safe numeric baseline for `prependListener` / `prependOnceListener` on `ClientRequest` via `listenerCount(event)` growth and cleanup via `removeAllListeners(event)`.
  - Contract coverage now includes safe chainability baseline for key request APIs: `setTimeout`, `setNoDelay`, `setSocketKeepAlive`, `cork`, and `uncork` each return the same `ClientRequest` instance.
  - Contract coverage now includes safe EventEmitter chainability baseline on `ClientRequest`: `setMaxListeners`, `on`, `off`, and `removeAllListeners` return the same request instance.
  - Contract coverage now also verifies chainability for `setDefaultEncoding` and `flushHeaders` on `ClientRequest` (each returns the same request instance).
  - Contract coverage now verifies request-socket chainability baseline for `setTimeout`, `setNoDelay`, `setKeepAlive`, `setEncoding`, `ref`, `unref`, `end`, and `destroy` (each returns the same socket object).
  - Contract coverage now additionally asserts `request.connection` / `request.socket` alias consistency across multiple request scenarios (not just initial shape checks).
  - Request-side socket exposes chainable `ref()` / `unref()` as baseline no-op compatibility helpers.
  - Client response object includes baseline event methods (`on`/`once`/`addListener`/`removeListener`/`emit`) and emits `data` (full `body`) then `end` after callback registration paths.
  - **Remaining vs Node v23:** streaming `IncomingMessage`/`ClientRequest`, full `request` options object, `Agent`, and WebSocket upgrades.
- `node:https` — `🟢` Same server baseline as `node:http` (**no TLS** on `createServer`). **Client:** HTTPS URLs use **rustls** through reqwest; same non-streaming response shape as `http`. Contract: `https.get('https://example.com')` in `priority_builtins_green_contract`.
  - `https.request(...)` mirrors the same baseline `ClientRequest` shape as `http.request` (URL/options forms, header helpers, timeout/abort APIs) over HTTPS transport.
  - Contract coverage includes safe chainability/state baselines on `https.request` matching the HTTP request surface (`setTimeout`, `setNoDelay`, `setSocketKeepAlive`, `setDefaultEncoding`, `cork`/`uncork`, `setMaxListeners`/`getMaxListeners`, `on`/`off`) plus socket chainability (`ref`/`unref`, `setEncoding`, `setTimeout`).
  - Contract coverage additionally includes safe `https.request` baselines for `listeners`/`rawListeners` shape, `removeAllListeners` chainability, and `flushHeaders` (`headersSent/_headerSent` transitions).
  - Quick readability map:
    - **Transport surface:** HTTPS client path uses reqwest/rustls with the same request object contract as HTTP.
    - **Request parity scope:** header/lifecycle/EventEmitter and tuning methods follow HTTP baseline semantics.
    - **Socket parity scope:** socket helpers are chainable with baseline state reflection.
  - **Remaining vs Node v23:** TLS `createServer`, client cert pinning/options matrix, ALPN/h2, streaming parity with `http`.
- `node:os` — `🟢` Baseline for typical npm: `platform`, `tmpdir`, `homedir`, `arch`, `endianness`, `release` (best-effort via `uname` on Unix), `cpus` (length from `available_parallelism`, stub `model`/`speed`/`times`), `totalmem` / `freemem` (Unix `sysconf`; Linux `MemAvailable` when `/proc/meminfo` is readable), `**EOL`**, `**loadavg`** (Linux `/proc/loadavg` or `getloadavg` when available), `**networkInterfaces**` (empty object stub).
  - **Remaining vs Node v23:** real interface listing, full CPU times, signal constants, and byte-exact memory figures.
- `node:path` — `🟢` `join`, `dirname`, `basename`, `extname`, `resolve`, `normalize`, `relative`, `parse`, `sep`, `delimiter`. Contract: `path.join` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** `posix`/`win32` namespaces, `pathToFileURL`, `fileURLToPath`, and full Windows long-path edge cases.
- `node:punycode` — `🟡` `decode`/`encode`: ASCII-oriented baseline (unchanged). `**toASCII` / `toUnicode`:** IDNA via `**idna`** crate (Unicode domains / ACE), closer to Node’s domain handling than the old ASCII-only stub.
  - **Remaining vs Node v23:** full RFC 3492 punycode module parity for non-domain strings, edge-case errors.
- `node:querystring` — `🟢` `parse` / `stringify` with optional `sep`, `eq`, and `options.maxKeys`; repeated-key arrays; `**escape`** / `**unescape`** (legacy helpers). Automated checks in `web_platform_and_builtins_baseline_contract`.
  - **Remaining vs Node v23:** full option matrix (`decodeURIComponent` override, etc.) and byte-for-byte Node test-suite equivalence.
- `node:readline` — `🟢` Non-interactive stub for CI and feature detection: `createInterface` exposes `input`/`output`/`terminal`, `question` (async empty answer), `prompt`, `on`/`once`/`off`, `close`, `pause`/`resume`, `write`, `setPrompt`. Interactive REPL is not implemented.
  - **Remaining vs Node v23:** real TTY/stream integration, history, and `readline`/`promises` subpath.
- `node:stream` — `🟢` Baseline behavior (`Readable`, `Writable`, `Duplex`, `Transform`) including basic `pipe`/`data`/`end` (primed shim in `[mod.rs](../core/src/node/mod.rs)`). Contract: `Readable` push/end in `priority_builtins_green_contract`.
  - Includes baseline `stream.pipeline(...)`, `stream.finished(...)`, and Promise forms under `stream.promises` for common npm control-flow paths.
  - Quick readability map:
    - **Core classes:** `Readable`/`Writable`/`Duplex`/`Transform` present for mainstream package detection paths.
    - **Composition helpers:** `pipeline` and `finished` (plus Promise forms) cover common orchestration flows.
    - **Current depth:** baseline data flow works; advanced pressure/mode semantics are intentionally partial.
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
- `node:module` — `🟡` `**createRequire(filename | file URL | URL)`** returns a `require` function with resolution rooted at the parent directory of `filename`; the returned function also exposes `require.resolve(request)` and `require.resolve.paths(request)` baseline behavior (`null` for built-ins, lookup path array otherwise), plus `require.cache`, `require.main`, and `require.extensions` (`.js`/`.json`/`.node` stubs). `module.isBuiltin(name)` and `module.builtinModules` are available, plus `module.Module` aliases for `createRequire` / `isBuiltin` / `builtinModules` and baseline internals `_load(request[, parent[, isMain]])`, `_resolveFilename(request[, parent])` / `_nodeModulePaths(from)`, plus stubbed `_cache` / `_extensions` (`.js`/`.json`/`.node`) and compatibility no-op stubs for `syncBuiltinESMExports` / `findSourceMap`.
- `node:net` — `🟢` Compatibility entrypoint: same `createServer` shape as `http` for baseline usage. Contract: `createServer` export in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** real `Socket`/`Server` TCP lifecycle, `connect`, TLS bridging, and `BlockList`/`SocketAddress` APIs.
- `node:perf_hooks` — `🟢` Exports global `performance` (`now`, `timeOrigin`), `**PerformanceObserver`**, minimal `**PerformanceEntry`**, `**PerformanceMark**`, `**PerformanceMeasure**`, `**PerformanceResourceTiming**`, `**PerformanceObserverEntryList**`, `**constants**`, and `**nodeTiming`{}** for typical feature detection. `globalThis` also defines `**PerformanceEntry`**, `**PerformanceMark`**, `**PerformanceMeasure**` (see Globals).
  - **Remaining vs Node v23:** real timing entries, histograms, full `PerformanceObserver` behavior, and `nodeTiming` metrics.
- `node:process` — `🟢` The `**process` object is installed as a global** during `install_runtime` (`argv`, `env`, `cwd`, `nextTick`, timing helpers, `stdout`/`stderr` via `process::install_stdio`, etc.). `**require('process')` / `require('node:process')`** returns a duplicate of the global `process` binding (`js_dup_value`) for packages that expect the module form. Contract: `process.cwd` in `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** `emit`, `binding`, full `versions`/`release`, `permission`, `dlopen`, and internal fields used by advanced tooling.
- `node:sys` — `🟢` Legacy alias: same exports as `node:util` (see `node:util`).
- `node:tls` — `🟡` Baseline API surface (`connect`, `createServer`) as compatibility placeholders.
  - **Remaining vs Node v23:** real TLS handshake/session behavior and full Node option parity.
- `node:util` — `🟢` `inspect`, `**types`**: `isDate`, `isArrayBuffer`, `isString`, `isObject`, `isFunction`, `isNumber`, `isBoolean`, `**isNull`**, `**isUndefined**`, `**isRegExp**`, `**isBuffer**` (delegates to `Buffer.isBuffer`). `**promisify**` when `globalThis.__kawkabUtilPromisify` installs at bootstrap. Contract: `web_platform_and_builtins_baseline_contract` / `priority_builtins_green_contract`.
  - **Remaining vs Node v23:** full `types` matrix, `debuglog`, `promisify` custom symbols, and complete `inspect` options.
- `node:v8` — `🟡` Compatibility stub (`cachedDataVersionTag`, `getHeapStatistics`, `setFlagsFromString`).
- `node:vm` — `🟡` Baseline API surface (`runInThisContext`, `runInNewContext`, `Script`, `createContext`, `isContext`) with simplified semantics.
  - **Current behavior:** `runInNewContext(code, sandbox)` maps own string properties of `sandbox` to `Function` parameters and evaluates `code` as `return (<code>);` inside that function (expression-oriented, not full Node `Script` / VM isolation).
  - **Remaining vs Node v23:** isolated context guarantees and broader script/module execution parity.
- `node:wasi` — `🟡` Compatibility stub (`WASI`) with baseline instance methods (`start`, `initialize`) and `wasiImport` preview object shape.
- `node:worker_threads` — `🟢` **Baseline workers on real OS threads:** each `Worker` runs in its own **QuickJS isolate** on a dedicated Rust thread (`core/src/node/worker_threads.rs`). Main thread: `Worker` constructor (script path), `postMessage` / `on('message')`, `terminate`, `isMainThread`, `threadId`. Worker isolate: `parentPort.postMessage` / `parentPort.on('message')`. Messages are **JSON text** (values must round-trip via `JSON.stringify` / `JSON.parse`; otherwise a `TypeError` is thrown). Nested `Worker` from inside a worker is rejected. Contract: `node::compat_contract_tests::worker_threads_roundtrip` (and idle spawn smoke).
  - **Remaining vs Node v23:** full **structured clone** (including `ArrayBuffer`, `SharedArrayBuffer`, transfer lists), `new Worker(new URL(...))` / `import.meta.url`, `workerData`, wiring **global** `MessageChannel` / `MessagePort` / `BroadcastChannel` to cross-thread semantics (today globals are main-isolate baseline; ports use in-memory synchronous delivery), `resourceLimits`, `markAsUntransferable`, `receiveMessageOnPort`, `setEnvironmentData` / `getEnvironmentData`, `performance` in worker, `eventLoopUtilization`, and full `events`/EventEmitter parity on `Worker`.
- `node:inspector` — `🟡` Compatibility stub (`open`, `close`, `url`, `waitForDebugger`, `Session`) for tooling detection.
- `node:repl` — `🟡` Compatibility stub (`start`) returning a minimal REPL-server shape (`context`, `defineCommand`, `close`, event helpers).
- `node:sqlite` — `🟡` Compatibility stub (`Database`, `Statement`) with baseline `prepare`/`exec`/`run`/`get`/`all`/`finalize` shapes.
- `node:test` / `test` — `🟢` Baseline (`test`, `it`, `describe`, `before`, `after`, `**beforeEach`**, `**afterEach`**) invoking the callback when arity allows (same helper as `it`). Contract: hook exports in `web_platform_and_builtins_baseline_contract`.
  - **Remaining vs Node v23:** runner semantics, mocks/snapshots/timers parity, and full `node:test` API.
- `node:trace_events` — `🟡` Compatibility stub (`createTracing`, `getEnabledCategories`) for feature detection.

## Node.js Globals

Quick readability map:
- **Runtime globals (`🟢`)**: core Node runtime anchors are broadly available (`globalThis`, `global`, `process`, timers, CommonJS context globals).
- **Web-platform globals (`🟡/🟢`)**: pragmatic baseline for `fetch`/`Request`/`Response`/`Headers`, streams, messaging, encoding, and URL APIs with explicit “Remaining vs Node v23” caveats.
- **Intentional gaps (`🔴`)**: globals gated by product scope/security/runtime constraints remain explicit and documented (e.g. selected WebCrypto constructors).

- `AbortController` — `🟢` Fully implemented.
- `AbortSignal` — `🟢` Fully implemented.
- `Blob` — `🟡` Baseline (`size`, `type`, `slice`, `arrayBuffer`, `text`) via `[web_platform_shim](../core/src/node/web_platform_shim.js)`; UTF-8 string parts; nested `Blob` parts supported. Contract: `web_platform_and_builtins_baseline_contract`.
  - **Remaining vs Node v23:** full Web `Blob` / file API parity and stream integration.
- `Buffer` — `🟢` Global constructor installed with `[core/src/node/buffer.rs](../core/src/node/buffer.rs)` (`Uint8Array` subclass + native helpers). Same **Remaining vs Node v23** as `node:buffer`.
- `ByteLengthQueuingStrategy` — `🟡` Constructor with `highWaterMark` and `size(chunk)` baseline (installed with web streams).
- `__dirname` — `🟢` Fully implemented.
- `__filename` — `🟢` Fully implemented.
- `atob()` — `🟢` Implemented for typical npm/web usage (ASCII and unpadded base64 decode to binary string, invalid base64/padding rejects with `TypeError`). Contract: `global_base64_ascii_contract`.
  - **Remaining vs Node v23:** exact WebIDL/DOMException class parity and full edge-case matching for every malformed input variant.
- `Atomics` — `🟡` Deterministic shim installed on `globalThis` for typed-array atomic-style operations (`load`, `store`, `add`, `sub`, `and`, `or`, `xor`, `exchange`, `compareExchange`, `isLockFree`, `notify`). `wait`/`waitAsync` intentionally throw in this embedding.
- `BroadcastChannel` — `🟡` In-process channels by name (`postMessage`, `close`, `EventTarget` listeners); **synchronous** delivery on the same isolate/thread (differs from browser timing).
- `btoa()` — `🟢` Implemented for typical npm/web usage (binary string to base64, rejects non-Latin1 input instead of silent truncation). Contract: `global_base64_ascii_contract`.
  - **Remaining vs Node v23:** exact WebIDL/DOMException class parity and byte-for-byte legacy edge-case equivalence.
- `clearImmediate()` — `🟢` Fully implemented.
- `clearInterval()` — `🟢` Fully implemented.
- `clearTimeout()` — `🟢` Fully implemented.
- `CompressionStream` — `🟡` `gzip` and `deflate` via native `__kawkabWebCompress` (full-stream flush: chunks buffered until writable closes, then one compressed block). Contract: gzip round-trip in `web_platform_and_builtins_baseline_contract`.
  - **Remaining vs Node v23:** true chunk streaming, `deflate-raw`, and byte-for-byte Web Compression Streams parity.
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
- `fetch` — `🟡` Compatibility shim available (non-standard backend behavior), including baseline `fetch(Request, init)` override behavior and `Headers` propagation on the returned `Response`.
  - **Remaining vs Node v23:** standards-compliant request/response lifecycle and full fetch semantics.
- `FormData` — `🟡` `append`, `delete`, `get`, `getAll`, `has`, `set` (string values for `get`/`getAll`; `Blob` values summarized as strings).
- `global` — `🟢` Implemented.
- `globalThis` — `🟢` Implemented.
- `Headers` — `🟡` Expanded baseline shim: normalized case-insensitive keys, `append`/`set`/`get`/`has`/`delete`, `forEach`, iterator factories (`entries`/`keys`/`values`), and baseline `getSetCookie()` for common package flows (case-insensitive lookup; returns a new array per call).
  - **Remaining vs Node v23:** complete headers normalization/iteration semantics parity.
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
  - **Remaining vs Node v23:** same bullets as `node:process`.
- `queueMicrotask()` — `🟢` Implemented.
- `ReadableByteStreamController` — `🟡` Illegal-constructor placeholder (feature detection only).
- `ReadableStream` — `🟡` Queue-based baseline: `start` controller with `enqueue`/`close`/`error`, `getReader()` with `read()` returning Promises, `cancel`/`releaseLock` stubs.
  - **Remaining vs Node v23:** full WHATWG streams (pull/cancel, BYOB, tee, async iterator).
- `ReadableStreamBYOBReader` — `🟡` Illegal-constructor placeholder.
- `ReadableStreamBYOBRequest` — `🟡` Illegal-constructor placeholder.
- `ReadableStreamDefaultController` — `🟡` Illegal-constructor placeholder.
- `ReadableStreamDefaultReader` — `🟡` Illegal-constructor placeholder.
- `require()` — `🟢` Implemented for the CommonJS + built-in subset described above.
- `Response` — `🟡` Expanded baseline shim: `status`/`statusText`/`ok`, `text()`, `arrayBuffer()`, `json()`, `clone()` with header copy semantics and clone body-independence, static `Response.json(data, init)` helper (keeps caller-provided `content-type` when present; serializes `undefined` as JSON `null`), plus baseline `bodyUsed` single-consumption behavior (subsequent reads reject; cloning after consumption throws). `json()` rejects invalid JSON inputs. `arrayBuffer()` keeps typed-array view bounds (`byteOffset`/`byteLength`) when body is a `Uint8Array` view.
  - **Remaining vs Node v23:** full body/stream/status/header behavior parity.
- `Request` — `🟡` Expanded baseline shim: constructor from URL or existing `Request`, normalized method casing, copied `Headers`, optional body, and baseline `Request(inputRequest, init)` override behavior for `method`/`headers`/`body`; `clone()` with independent body consumption from the source request; body readers `text()`, `arrayBuffer()`, and `json()` for common request-body flows; plus baseline `bodyUsed` single-consumption behavior. `json()` rejects invalid JSON inputs. `arrayBuffer()` keeps typed-array view bounds (`byteOffset`/`byteLength`) when body is a `Uint8Array` view.
  - **Remaining vs Node v23:** full request cloning/body/headers semantics parity.
- `setImmediate()` — `🟢` Fully implemented (same native path as `setTimeout` in this embedding; see `node:timers`).
- `setInterval()` — `🟢` Fully implemented.
- `setTimeout()` — `🟢` Fully implemented.
- `structuredClone()` — `🟡` Baseline via `JSON.parse(JSON.stringify(value))` when missing (JSON-serializable values only; no `Map`/`Set`/`ArrayBuffer`/full `Date` semantics).
  - **Remaining vs Node v23:** spec-correct structured cloning including transfer lists and non-JSON types.
- `SubtleCrypto` — `🔴` Not implemented.
- `DOMException` — `🟡` Minimal (`Error` subclass with `name` / `message`; `code` stub `0`).
- `TextDecoder` — `🟡` Compatibility shim available.
  - **Remaining vs Node v23:** complete encoding/error-mode parity for all supported encodings.
- `TextDecoderStream` — `🟡` Baseline UTF-8 transform stream available (chunk aggregation + UTF-8 decode on flush).
- `TextEncoder` — `🟡` Compatibility shim available.
  - **Remaining vs Node v23:** complete byte-level parity in all edge cases.
- `TextEncoderStream` — `🟡` Baseline transform stream available (string chunks to UTF-8 bytes).
- `TransformStream` — `🟡` Default `readable`/`writable` pair; `transform`/`flush` controller hooks; pairs with baseline `WritableStream`/`ReadableStream`.
- `TransformStreamDefaultController` — `🟡` Illegal-constructor placeholder.
- `URL` — `🟡` Compatibility constructor exposed.
  - **Remaining vs Node v23:** full WHATWG URL conformance and edge-case normalization parity.
- `URLSearchParams` — `🟡` Compatibility constructor exposed.
  - **Remaining vs Node v23:** full iteration/sorting/encoding parity with Node.
- `WebAssembly` — `🟡` Compatibility baseline available for feature detection and guarded execution paths (`Module`, `Instance`, `Memory`, `Table`, `Global`, `validate`, `compile`, `instantiate`, and error constructors). Not full Node/engine parity.
- `WritableStream` — `🟡` `getWriter()` with `write`/`close`/`abort`/`releaseLock`; underlying sink optional.
- `WritableStreamDefaultController` — `🟡` Illegal-constructor placeholder.
- `WritableStreamDefaultWriter` — `🟡` Illegal-constructor placeholder.

## Compatibility coverage (reference scripts)

Reference scripts under `examples/` were removed. When you add new checks, list them in `[docs/NPM_CORPUS.md](NPM_CORPUS.md)`. Workspace tests and `[scripts/compat_smoke.sh](../scripts/compat_smoke.sh)` provide a repeatable baseline.

## Yellow to Green Execution Plan

This section defines how to move all current `🟡` items to `🟢` with measurable gates. The goal is practical npm compatibility parity for the documented surface, not byte-for-byte internal parity.

### Delivery principles

- Upgrade by **coherent capability slices** (not isolated symbols), so package ecosystems unlock together.
- Every promoted item must satisfy `COMPAT_DEFINITION_OF_DONE.md` and include: implementation, contract tests, corpus impact, and docs updates.
- No symbol moves to `🟢` without at least one reproducible integration check in `compat_smoke.sh` or workspace tests.
- Keep `README.md` snapshot, this file, and `FEATURE_BASELINE.md` synchronized in the same PR.

### Wave 1 (highest npm impact)

1. **Events + Streams reliability**
   - Close `node:events` edge cases: listener ordering, leak warnings, `once/on` helper semantics.
   - Raise web/Node stream interoperability: backpressure, async iterator behavior, `pipeline`/`finished` parity.
   - Exit criteria: event-heavy libraries pass targeted corpus samples without behavior shims.
2. **HTTP/Web primitives**
   - Promote `fetch`, `Request`, `Response`, `Headers`, `URL`, `URLSearchParams`, `TextEncoder`, `TextDecoder`.
   - Align semantics for cloning, header normalization, body lifecycle, and encoding corner cases.
   - Exit criteria: modern HTTP client libs and SDKs run with no local patches.
3. **Compression + text streaming**
   - Upgrade `CompressionStream`, `DecompressionStream`, `TextEncoderStream`, `TextDecoderStream`.
   - Replace full-buffer flush behavior with true chunk-stream processing.
   - Exit criteria: stream-based pipelines produce equivalent outputs to Node for compatibility fixtures.

### Wave 2 (runtime/runtime-tooling parity)

1. **Net/TLS/HTTP2 foundations**
   - Promote `node:tls`, `node:net` lifecycle behavior, and practical `node:http2` interoperability surface.
   - Exit criteria: baseline secure client/server flows and feature-detection libraries work without stubs.
2. **VM and module fidelity**
   - Upgrade `node:vm` toward true context isolation semantics.
   - Expand `node:module` parity for resolution edge behavior relied on by tooling.
   - Exit criteria: common build/test tooling no longer requires compatibility guards.
3. **Worker + messaging semantics**
   - Move from JSON-only messaging toward structured clone + transfer-list semantics.
   - Align `MessageChannel`/`MessagePort`/`BroadcastChannel` timing and cross-thread behavior.
   - Exit criteria: worker-based libraries pass structured clone and lifecycle fixtures.

### Wave 3 (long-tail and platform completeness)

1. **WebCrypto surface**
   - Implement `Crypto`, `SubtleCrypto`, `CryptoKey` with practical algorithm coverage for npm ecosystem usage.
   - Exit criteria: common JWT/signature/hash workflows run through standards-aligned APIs.
2. **Remaining compatibility stubs**
   - Evaluate and either fully implement or explicitly keep non-goals for: `cluster`, `domain`, `inspector`, `repl`, `sqlite`, `trace_events`, `v8`, `wasi`, `tty`, and related globals.
   - Exit criteria: each item is either `🟢` with tests or explicitly documented as a product-level non-goal.

### Promotion checklist (`🟡` -> `🟢`)

For each module/global:

1. Implement missing runtime behavior with documented caveats removed or reduced.
2. Add contract tests under `core/src/node/compat_contract_tests.rs` (or nearest existing suite) that fail before and pass after.
3. Add/extend corpus coverage in `docs/NPM_CORPUS.md` with at least one real package signal.
4. Add/extend `compat_smoke.sh` (or equivalent) for deterministic regression detection.
5. Update this file, `FEATURE_BASELINE.md`, and `README.md` snapshot together.
6. Record any remaining gaps under **Remaining vs Node v23** before promotion, then remove stale caveats after proof.

### Quality gates per PR

- All tests pass locally and in CI.
- No regression in existing `🟢` contract suites.
- At least one new compatibility signal is added (contract, corpus, or smoke).
- Documentation is updated in the same change.
- Reviewer can reproduce the promoted behavior from commands and fixtures in the PR description.

## Notes

- This status reflects current implementation in this repository and intentionally marks unsupported Node APIs as `🔴`.
- The compact snapshot in `README.md` is a quick overview; **this file is the detailed reference** and should stay aligned with `js_require` / `install_runtime`.
- If a module/global is added or upgraded, update this file and `docs/FEATURE_BASELINE.md` in the same change.
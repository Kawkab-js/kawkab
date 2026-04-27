# Reference corpus (npm / Node parity smoke)

Use this list to **measure** compatibility improvements (Kawkab vs Node). It is **not** a full registry sweep. **KPI targets** and pass semantics: `[COMPAT_KPI.md](COMPAT_KPI.md)`. Frozen Top 100 package basket: [`data/top100-packages.txt`](data/top100-packages.txt) (repo path `docs/data/top100-packages.txt`).

Quick navigation:
- [Docs index](INDEX.md)
- [How to run](#how-to-run)
- [Pass criteria (summary)](#pass-criteria-summary)
- [Express ecosystem (KPI: 100% of rows below)](#express-ecosystem-kpi-100-of-rows-below)
- [NestJS (KPI: 100% of rows below)](#nestjs-kpi-100-of-rows-below)
- [Prisma (KPI: 100% of rows below)](#prisma-kpi-100-of-rows-below)
- [Next.js custom server (KPI: 90% of rows below)](#nextjs-custom-server-kpi-90-of-rows-below)
- [Top 100 basket automated smoke (revision 2, KPI: 100% QuickJS target)](#top-100-basket-automated-smoke-revision-2-kpi-100-quickjs-target)
- [General packages / scenarios (seed list)](#general-packages--scenarios-seed-list)
- [Adding entries](#adding-entries)

## How to run

- **Rust API / resolution:** `RUST_TEST_THREADS=1 cargo test -p kawkab-core` (includes `module_loader` tests; serial threads avoid rare `worker_threads` harness `SIGABRT` when other workspace test binaries overlap locally).
- **CLI smoke:** run the same small scripts with `kawkab` and with `node` and diff stdout/exit code. Automated runner: `scripts/kpi_smoke.sh` with modes (`http`, `express`, `express-json`, `express-static`, `top100-sample`, `all`).
- **Optional:** port selected cases from the [Node.js test suite](https://github.com/nodejs/node/tree/main/test) (e.g. `test/parallel/test-fs-`*) — track filenames here when adopted.

## Pass criteria (summary)

For each row: **exit code 0** for both Node and Kawkab (unless “intentional difference” is noted), and **matching stdout** per the row’s tolerance. Full rules: `[COMPAT_KPI.md](COMPAT_KPI.md#pass-criteria)`.

---

## Express ecosystem (KPI: 100% of rows below)


| Scenario                                    | Node cmd                                                    | Kawkab cmd                                 | Success criterion                                                     |
| ------------------------------------------- | ----------------------------------------------------------- | ------------------------------------------ | --------------------------------------------------------------------- |
| Minimal app + `GET /` + `listen`            | `node server.js` (fixture: `createServer` on port from env) | `kawkab run server.js` (or documented CLI) | HTTP client receives `200` and expected body; clean exit on shutdown. |
| JSON body via `express.json()`              | `node server-json.js`                                       | same with Kawkab                           | POST with JSON returns parsed payload echo; exit 0.                   |
| Single static middleware (`express.static`) | `node server-static.js`                                     | same                                       | `GET /file.txt` returns file contents; 404 for missing path.          |


---

## NestJS (KPI: 100% of rows below)


| Scenario                                  | Node cmd                                                      | Kawkab cmd                | Success criterion                                                                |
| ----------------------------------------- | ------------------------------------------------------------- | ------------------------- | -------------------------------------------------------------------------------- |
| Bootstrap + one module + one `GET` route  | `node dist/main.js` (or `nest start` if using CLI in fixture) | documented `kawkab` entry | `GET /` (or `/health`) returns expected string; exit 0 after test harness stops. |
| DI: one provider injected into controller | fixture with `NestFactory` + provider token                   | same                      | Response reflects injected value; no bootstrap exception.                        |


*Scope:* decorators and patterns **not** listed here are out of scope for the KPI until a row is added.

---

## Prisma (KPI: 100% of rows below)


| Scenario                          | Node cmd                               | Kawkab cmd                     | Success criterion                         |
| --------------------------------- | -------------------------------------- | ------------------------------ | ----------------------------------------- |
| `prisma generate` + client import | `npx prisma generate && node query.js` | same toolchain where supported | Client loads; no generate error.          |
| SQLite: `findMany` on empty table | `node query.js`                        | same                           | Returns `[]` or documented shape; exit 0. |


*If* a scenario requires a **native** Prisma engine binary incompatible with current Kawkab N-API policy, the row must state **“skipped / requires native”** and is excluded from the 100% denominator until documented in scope.

---

## Next.js custom server (KPI: 90% of rows below)


| Scenario                                | Node cmd         | Kawkab cmd | Success criterion                                                   |
| --------------------------------------- | ---------------- | ---------- | ------------------------------------------------------------------- |
| `next()` + `prepare()` + `createServer` | `node server.js` | same       | Server listens; `GET /` returns `200` after build step if required. |
| One static page + custom server         | `node server.js` | same       | Page HTML contains expected marker.                                 |
| API route hit through custom server     | `node server.js` | same       | `GET /api/hello` returns JSON body.                                 |


*Scope:* Edge runtime, Image Optimization internals, and middleware edge cases are **excluded** unless a row is added. The **90%** applies to the **enumerated** rows only; add rows as coverage grows.

---

## Top 100 basket automated smoke (revision 2, KPI: 100% QuickJS target)

- **Frozen list:** [`data/top100-packages.txt`](data/top100-packages.txt) on disk as [`docs/data/top100-packages.txt`](../docs/data/top100-packages.txt). `revision: 2` swaps a few packages that required native binaries or default ESM-only installs for **CJS-friendly** alternatives while keeping 100 rows (see header comments in the list file).
- **Fixture:** [`fixtures/kpi/top100-qjs/`](../fixtures/kpi/top100-qjs/) — `package.json` + `package-lock.json`, `smokes.cjs` (one deterministic `JSON.stringify` line per package), `check-one.cjs` runner, `.current-pkg` marker (gitignored) for the active package name.
- **Node matrix (CI):** [`scripts/top100_node_matrix.sh`](../scripts/top100_node_matrix.sh) — validates the 100 smokes under Node after `npm ci` in the fixture (guards the corpus and dependency pins).
- **QuickJS matrix (CI):** [`scripts/top100_qjs_matrix.sh`](../scripts/top100_qjs_matrix.sh) — same `check-one.cjs` loop with `kawkab --engine quickjs`. The script runs in CI after the Node matrix; the step is configured so a non-100/100 result is visible in logs without failing the job until runtime parity catches up.
- **Runtime:** `require()` applies a small source normalizer for QuickJS lexer quirks around `/**` / `/*!` block comment openers (`sanitize_cjs_body_for_quickjs_block_comment_openers` in `kawkab-core`). The CLI passes a **synthetic eval filename** into QuickJS so absolute paths under `…/projects/…` do not break `JS_Eval` parsing. Regression: `cargo test -p kawkab-core require_merge_descriptors_express_fixture` (needs `npm install` under `fixtures/kpi/express-minimal`).

---

## General packages / scenarios (seed list)


| Area          | Scenario                                     | Node cmd                              | Kawkab cmd |
| ------------- | -------------------------------------------- | ------------------------------------- | ---------- |
| Modules       | `exports` / `imports` / bare subpath         | small local `package.json` fixtures   | same       |
| `process.env` | `NODE_ENV` affects `package.json` conditions | set env + `require()` package         | same       |
| `http` client | GET HTTPS endpoint                           | script using `http.get` / `https.get` | same       |
| `fs`          | `copyFileSync` / `rmSync`                    | temp dir script                       | same       |


## Adding entries

For each new row: link or paste a **minimal repro** (≤ ~30 lines where possible), expected exit code, and note any intentional Kawkab difference. If the row belongs to a **KPI tier**, align the scenario wording with `[COMPAT_KPI.md](COMPAT_KPI.md)`.
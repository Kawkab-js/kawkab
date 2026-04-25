# KPI fixtures (ecosystem smoke)

Targets and pass rules: [`docs/COMPAT_KPI.md`](../../docs/COMPAT_KPI.md).

## Layout

| Directory | Purpose |
|-----------|---------|
| `http-minimal/` | Node core `http` only (no `npm` deps). Default for QuickJS KPI smoke. |
| `express-minimal/` | Minimal Express + `http.createServer(app)`. On **QuickJS**, current `negotiator` / RegExp edge cases block loading Express; use **`--engine node`** for a real run. |
| `express-json/` | Express + `express.json()` POST echo scenario (`/echo`). |
| `express-static/` | Express static middleware scenario (`/file.txt`). |
| `top100-sample/` | Top100 seed sample: `lodash` + `semver` deterministic smoke. |
| `top100-qjs/` | Full **Top 100** basket (`docs/data/top100-packages.txt`, `revision: 2`): `smokes.cjs` + `check-one.cjs`. CI runs [`scripts/top100_node_matrix.sh`](../../scripts/top100_node_matrix.sh) (required) then [`scripts/top100_qjs_matrix.sh`](../../scripts/top100_qjs_matrix.sh) (QuickJS; logs only until 100/100). |

## NestJS, Prisma, Next.js

Full **NestJS**, **Prisma Client** (native engine), and **Next.js** dev/build pipelines assume **Node.js + V8** and often **native addons**. Kawkab’s baseline is **QuickJS**; see [`docs/NODE_NON_GOALS.md`](../../docs/NODE_NON_GOALS.md).

- **Prisma:** `prisma generate` / query paths that need the Rust or binary engine are **out of scope** until N-API / engine policy is extended; track scenarios in [`docs/NPM_CORPUS.md`](../../docs/NPM_CORPUS.md) with explicit “skipped / requires native”.
- **NestJS / Next custom server:** Use **`kawkab --engine node`** (or CLI `auto` fallback) for real framework runs; QuickJS-backed smoke tests should target **reduced** scenarios only when added to the corpus.

## Running http minimal (QuickJS)

```bash
# From repo root (after cargo build -p kawkab-cli)
./target/debug/kawkab --file fixtures/kpi/http-minimal/server.js
# In another terminal:
curl -s "http://127.0.0.1:<port>/"
```

Expected: body `kawkab_http_ok` and stdout contains `listening <port>`.

## Running Express minimal (Node engine)

```bash
cd express-minimal
npm install
cd ../..
./target/debug/kawkab --engine node --file fixtures/kpi/express-minimal/server.js
curl -s "http://127.0.0.1:<port>/"
```

Expected: body `kawkab_express_ok` and stdout contains `listening <port>`.

## Automated smoke

[`../../scripts/kpi_smoke.sh`](../../scripts/kpi_smoke.sh) defaults to **`KAWKAB_KPI_MODE=http`**.  
Default `all` mode runs stable required set (`http` + `express`).  
Extended set (`express-json`, `express-static`, `top100-sample`):  
`KAWKAB_KPI_MODE=all KAWKAB_KPI_INCLUDE_EXTENDED=1 ./scripts/kpi_smoke.sh`.

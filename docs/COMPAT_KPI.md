# Compatibility KPIs (ecosystem-first)

This document is the **product-facing compatibility contract**: we optimize for **real stacks and registry usage**, not 100% Node.js built-in API parity. Module-level status remains in [`NODE_COMPATIBILITY.md`](NODE_COMPATIBILITY.md); **prioritization** when they conflict should follow this file and [`NPM_CORPUS.md`](NPM_CORPUS.md).

## Target table

| Tier | Goal | Meaning |
|------|------|--------|
| **Top 100 npm packages** | **80%** | Of the frozen list in [`docs/data/top100-packages.txt`](data/top100-packages.txt), 80% pass the [pass criteria](#pass-criteria) for their **declared smoke scenario** (see corpus). |
| **Express ecosystem** | **100%** | All **Express tier** scenarios in [`NPM_CORPUS.md`](NPM_CORPUS.md) pass (minimal app + common middleware + `listen` + HTTP request). |
| **NestJS** | **100%** | All **NestJS tier** scenarios in the corpus pass (bootstrap + module + at least one HTTP route in the **defined scope**). |
| **Prisma** | **100%** | All **Prisma tier** scenarios in the corpus pass (`generate` + simple query against SQLite or another **explicitly documented** backend). |
| **Next.js custom server** | **90%** | 90% of **Next custom server** scenarios in the corpus pass (`server.js` / `app.prepare()` + `createServer` path; **not** full Edge/SSR matrix). |

## Pass criteria

A scenario **passes** when, on a supported Linux environment (see [`FEATURE_BASELINE.md`](FEATURE_BASELINE.md)):

1. Running the documented **Kawkab command** (typically `kawkab` on the fixture entry) exits with code **0**.
2. **Stdout/stderr** and **exit code** match the documented **Node reference command** within the tolerances noted for that row (or the row states intentional differences).
3. Any **network**, **filesystem**, or **policy** prerequisites in the row are satisfied (e.g. `KAWKAB_ALLOW_CHILD_PROCESS` where applicable).

**Does not** require: byte-identical stack traces, identical timing, or unsupported features listed in [`NODE_NON_GOALS.md`](NODE_NON_GOALS.md) (e.g. native addons unless explicitly in scope for that tier).

## Frozen Top 100 list

- **File:** [`docs/data/top100-packages.txt`](data/top100-packages.txt)
- **Purpose:** Auditable denominator for the **80%** KPI. Update the file only with an intentional **revision** (bump the `generated_on` / `revision` header inside the file) and adjust corpus rows accordingly.
- **Not** a claim that every line is “#1 by downloads this week”; it is a **versioned basket** for regression tracking. Replace or extend via PR with rationale.

## Tier scope (what “100%” / “90%” includes)

### Express ecosystem

- Minimal `express()` app, `GET` route, `app.listen`, and at least one **documented** middleware pattern (e.g. `express.json()` or static) as listed in the corpus.
- **Out of scope** for the initial definition row unless added to the corpus: every third-party middleware in npm.

### NestJS

- Application bootstrap (`NestFactory.create`), one module, one HTTP controller route returning a fixed body.
- **Out of scope** unless explicitly added: every decorator, microservices, GraphQL, WebSockets, full DI edge cases.

### Prisma

- `prisma generate` and a **minimal** runtime query path documented in the corpus (e.g. SQLite file DB).
- **Out of scope** unless N-API / native engine support is documented as in-scope: binary engines that require `*.node` behavior not implemented in Kawkab (see [`NODE_NON_GOALS.md`](NODE_NON_GOALS.md)).

### Next.js custom server

- Custom Node server that calls `next({ ... })`, `app.prepare()`, and `http.createServer` / HTTPS equivalent as in the corpus fixture.
- **90%** applies to the **enumerated** Next scenarios in the corpus (e.g. specific pages/API routes), not every Next feature (Edge, Image Optimization internals, etc.).

## Relationship to the compatibility matrix

- [`NODE_COMPATIBILITY.md`](NODE_COMPATIBILITY.md) remains the **technical** breakdown (built-ins, globals).
- **Shipping and roadmap tradeoffs:** if a 🟢 module improvement conflicts with moving a **KPI tier** forward, prefer the tier **unless** security or baseline contracts in [`FEATURE_BASELINE.md`](FEATURE_BASELINE.md) forbid it.

## Automation (optional follow-up)

A future `scripts/kpi_smoke.sh` (or CI job) can run corpus fixtures under `fixtures/kpi/`; this document does not require that script to exist for the KPI definitions to be valid.

## Related docs

- [`COMPAT_DEFINITION_OF_DONE.md`](COMPAT_DEFINITION_OF_DONE.md) — 🟢/🟡 meanings
- [`NPM_CORPUS.md`](NPM_CORPUS.md) — scenario tables per tier
- [`PRODUCT_VISION.md`](PRODUCT_VISION.md) — positioning vs full Node parity

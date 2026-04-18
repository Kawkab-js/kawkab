# Definition of Done — Node compatibility (Kawkab)

This document defines what **🟢** and **🟡** mean in [`NODE_COMPATIBILITY.md`](NODE_COMPATIBILITY.md) so roadmap work stays measurable. It does **not** promise byte-for-byte parity with Node.js or V8.

**Prioritization:** The 🟢/🟡 matrix describes **module-level** readiness. When roadmap work conflicts (e.g. a rare built-in vs an Express/Nest/Prisma/Next smoke path), **product priority** follows ecosystem KPIs in [`COMPAT_KPI.md`](COMPAT_KPI.md) and scenarios in [`NPM_CORPUS.md`](NPM_CORPUS.md).

## Legend (contract)

| Mark | Meaning |
|------|--------|
| **🟢** | The documented surface is implemented for **typical npm usage**: common method shapes, stable error *kinds* where scripts branch, and behavior good enough for mainstream packages in the reference corpus. Known gaps vs Node v23 are listed in a short **“Remaining vs Node”** bullet on the same row. |
| **🟡** | Intentionally **partial** or **simplified**: enough for many apps, but important overloads, edge cases, or subsystem semantics differ from Node. The row must name the main caveats. |
| **🔴** | Not implemented **by product choice** or not started; may stay 🔴 with rationale in [`NODE_NON_GOALS.md`](NODE_NON_GOALS.md) (or the compatibility matrix). |

## Scope boundaries

- **Engine:** QuickJS — language timing, GC, and some builtins differ from V8; “🟢” does not mean identical micro-benchmarks or internal object layout.
- **Native addons:** `*.node` / N-API are **out of scope** unless the product explicitly adds them.
- **Tests:** Closing a 🟡 → 🟢 bump should include **automated tests** in `kawkab-core` (Rust unit/integration) and/or a **corpus check** (see [`NPM_CORPUS.md`](NPM_CORPUS.md)).

## Review cadence

When a phase in the roadmap lands, update in the **same change**:

1. [`NODE_COMPATIBILITY.md`](NODE_COMPATIBILITY.md) — status + “Remaining vs Node”.
2. [`FEATURE_BASELINE.md`](FEATURE_BASELINE.md) — summary line if behavior is user-visible.
3. Optional corpus entry in [`NPM_CORPUS.md`](NPM_CORPUS.md).
4. If the change affects a **KPI tier** (Top 100 basket, Express, NestJS, Prisma, Next custom server), update [`COMPAT_KPI.md`](COMPAT_KPI.md) and/or the relevant rows in [`NPM_CORPUS.md`](NPM_CORPUS.md).

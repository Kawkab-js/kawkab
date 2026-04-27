# Non-goals and 🔴 modules (product stance)

Some Node.js subsystems are **not targeted for full 🟢 parity** in the near term, or are blocked by engine/policy choices. This table records the **default stance**; it can change if product scope changes.

Quick navigation:
- [Docs index](INDEX.md)
- [Default stance table](#default-stance-table)
- [Scope note](#scope-note)

Related decision docs:
- Compatibility matrix and status details: `NODE_COMPATIBILITY.md`.
- Shipped behavioral baseline: `FEATURE_BASELINE.md`.
- Promotion semantics (`🟢/🟡/🔴`): `COMPAT_DEFINITION_OF_DONE.md`.
- KPI-first prioritization scope: `COMPAT_KPI.md`.
- Release gating and verification flow: `RELEASE_CHECKLIST.md`.

## Default stance table

| Module / area          | Stance        | Reason                                                                                  |
| ---------------------- | ------------- | --------------------------------------------------------------------------------------- |
| `*.node` / N-API       | Out of scope  | No native addon loader; documented in `[NODE_COMPATIBILITY.md](NODE_COMPATIBILITY.md)`. |
| `inspector`            | 🔴 / deferred | Debugger protocol and V8 integration are not aligned with QuickJS embedding goals.      |
| `repl`                 | 🔴 / deferred | Interactive REPL is a separate product surface; CLI may grow its own.                   |
| `v8`                   | 🔴            | Engine is QuickJS, not V8.                                                              |
| `wasi`                 | 🔴 / deferred | No WASI host integration in current architecture.                                       |
| `sqlite` (node:sqlite) | 🔴 / deferred | Requires bundling SQLite and API surface commitment.                                    |
| `trace_events`         | 🔴 / deferred | Low priority vs core I/O and module loading.                                            |
| `cluster`              | 🔴 / deferred | Multi-process clustering unlike single-process embed default.                           |
| `domain`               | 🔴            | Legacy; low demand vs maintenance.                                                      |
| `async_hooks`          | 🔴 / deferred | High complexity; consider only if ecosystem demand is proven.                           |
| `http2`                | 🔴 / deferred | Large spec surface; HTTP/1.1 client/server baseline first.                              |
| `tty`                  | 🔴 / partial  | Terminal integration depends on host; may stay stubbed.                                 |

## Scope note

Modules marked **🟡** in the matrix are **in scope** for incremental improvements unless explicitly moved here.
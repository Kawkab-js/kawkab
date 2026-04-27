# Node Parity Epics (Execution Backlog)

This backlog maps technical parity work to ownership and measurable acceptance.
Phase mapping references [ROADMAP_NODE_V23_MASTER.md](ROADMAP_NODE_V23_MASTER.md).

Quick navigation:
- [Docs index](INDEX.md)
- [Priority Legend](#priority-legend)
- [Epic Table](#epic-table)
- [Epic Details](#epic-details)
- [Dependency Critical Path](#dependency-critical-path)

## Priority Legend

- P0: Critical for ecosystem compatibility KPI.
- P1: Important for framework/runtime reliability.
- P2: Security/operational maturity.
- P3: Optimization and leadership improvements.

## Epic Table

| Epic ID | Name | Priority | Owner Team | Target Phase | Difficulty |
| --- | --- | --- | --- | --- | --- |
| EPIC-01 | HTTP/HTTPS parity core | P0 | Runtime Core | Phase 1 | High |
| EPIC-02 | Streams unification (Node + WHATWG) | P0 | Runtime Core | Phase 1-2 | Extreme |
| EPIC-03 | FS async and fd semantics | P0 | Runtime Core | Phase 1 | High |
| EPIC-04 | CJS/ESM resolver parity | P0 | Module Platform | Phase 1 | High |
| EPIC-05 | Fetch and Web API compliance | P0 | Web API | Phase 1 | High |
| EPIC-06 | Net/TLS/DNS production stack | P1 | Runtime Core | Phase 2 | Extreme |
| EPIC-07 | Crypto + WebCrypto parity baseline | P0 | Security Runtime | Phase 1-2 | Extreme |
| EPIC-08 | Worker threads full semantics | P1 | Concurrency | Phase 2 | High |
| EPIC-09 | Async hooks and ALS parity | P1 | Concurrency | Phase 2 | Extreme |
| EPIC-10 | Compatibility QA program | P0 | Compatibility QA | All | High |
| EPIC-11 | Policy engine and sandboxing | P2 | Security Runtime | Phase 3 | High |
| EPIC-12 | Performance acceleration | P3 | Performance | Phase 4 | High |

## Epic Details

## EPIC-01 HTTP/HTTPS parity core

Scope:
- Streaming `IncomingMessage` and `ClientRequest`.
- Agent keepalive baseline and connection reuse.
- Request lifecycle events parity for common npm paths.

Acceptance:
- Express and Next custom-server corpus rows stable.
- No regression in current `http/https` compatibility contracts.

Dependencies: EPIC-02.

## EPIC-02 Streams unification (Node + WHATWG)

Scope:
- Shared backpressure semantics.
- `pipeline` correctness under load and error paths.
- Bridge between Node streams and WHATWG streams.

Acceptance:
- Stream contract tests pass across happy/error/backpressure scenarios.
- Memory growth remains bounded in soak tests.

Dependencies: foundational for EPIC-01, EPIC-05, EPIC-07.

## EPIC-03 FS async and fd semantics

Scope:
- Async core (`open`, `read`, `write`, `close`, `stat`, `readdir`, `mkdir`).
- File descriptor lifecycle correctness and cleanup.
- Better error code parity for common paths.

Acceptance:
- Node fs selected tests green on approved subset.
- Top package smoke scenarios using async fs pass.

Dependencies: none.

## EPIC-04 CJS/ESM resolver parity

Scope:
- `exports`/`imports` edge cases.
- Conditional resolution ordering.
- Interop behavior for CJS <-> ESM.

Acceptance:
- Top100 resolution-related blockers reduced to near zero.
- No regressions in existing module loader tests.

Dependencies: none.

## EPIC-05 Fetch and Web API compliance

Scope:
- `fetch`, `Request`, `Response`, `Headers` semantics uplift.
- `Blob`, `FormData`, body stream behavior improvements.
- URL and encoding compatibility uplift on critical paths.

Acceptance:
- WPT subset for fetch/url/body behavior reaches target in phase gate.

Dependencies: EPIC-02.

## EPIC-06 Net/TLS/DNS production stack

Scope:
- Real socket lifecycle and connect behavior.
- TLS option baseline (SNI/ALPN/session basics).
- Resolver behavior alignment for common Node usage.

Acceptance:
- Stable long-run socket stress tests.
- TLS client/server smoke scenarios pass.

Dependencies: EPIC-01.

## EPIC-07 Crypto + WebCrypto parity baseline

Scope:
- Add missing Node crypto primitives used by ecosystem.
- Implement pragmatic WebCrypto baseline.
- Error semantics alignment for critical algorithms.

Acceptance:
- Crypto corpus and selected Node crypto tests pass.

Dependencies: EPIC-02.

## EPIC-08 Worker threads full semantics

Scope:
- Structured clone and transfer list behavior.
- Better `MessagePort` and channel semantics.
- Worker lifecycle hardening.

Acceptance:
- Concurrency stress tests pass repeatedly.

Dependencies: EPIC-09 recommended.

## EPIC-09 Async hooks and ALS parity

Scope:
- Context propagation across timers, promises, io, workers.
- Hook lifecycle consistency for practical instrumentation usage.

Acceptance:
- Async context fixtures pass across mixed async boundaries.

Dependencies: event-loop contract stability.

## EPIC-10 Compatibility QA program

Scope:
- Node core selected suite ingestion.
- WPT subset ingestion.
- Top1000 smoke automation and regression tracking.

Acceptance:
- CI parity dashboard reports stable trend and blocker ownership.

Dependencies: none.

## EPIC-11 Policy engine and sandboxing

Scope:
- Capability model for fs/net/child_process.
- Policy profiles for dev/ci/prod.
- Safe defaults and docs alignment.

Acceptance:
- Security profile e2e tests pass.

Dependencies: EPIC-03 and EPIC-06 hooks.

## EPIC-12 Performance acceleration

Scope:
- Lazy built-in loading.
- Bytecode/module cache enhancements.
- Buffer and scheduling optimization.

Acceptance:
- Phase 4 performance targets met without parity regressions.

Dependencies: EPIC-10 (regression safety net).

## Dependency Critical Path

1. EPIC-02 -> EPIC-01 / EPIC-05 / EPIC-07
2. EPIC-01 + EPIC-03 + EPIC-04 -> Phase 1 gate
3. EPIC-06 + EPIC-08 + EPIC-09 -> Phase 2 gate
4. EPIC-11 -> Phase 3 gate
5. EPIC-12 -> Phase 4 gate

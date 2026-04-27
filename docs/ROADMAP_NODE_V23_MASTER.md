# Kawkab Node v23 Master Roadmap (Execution Edition)

This document converts the compatibility vision into an engineering delivery program.
Primary technical references remain:
- [NODE_COMPATIBILITY.md](NODE_COMPATIBILITY.md)
- [COMPAT_KPI.md](COMPAT_KPI.md)
- [NPM_CORPUS.md](NPM_CORPUS.md)
- [FEATURE_BASELINE.md](FEATURE_BASELINE.md)

Quick navigation:
- [Docs index](INDEX.md)
- [Program Objective](#program-objective)
- [Program Principles](#program-principles)
- [Phase Plan](#phase-plan)
- [Governance](#governance)
- [Linked Operational Docs](#linked-operational-docs)

## Program Objective

Turn Kawkab into a serious lightweight runtime that can run the vast majority of practical npm workloads unmodified, with highest feasible parity to Node.js v23 under Rust + QuickJS constraints.

## Program Principles

- Behavior parity before API count parity.
- Ecosystem KPI impact over module vanity.
- Security defaults stay restrictive unless explicitly allowed.
- Performance optimization follows semantic correctness.
- Every compatibility upgrade ships with automated coverage.

## Phase Plan

## Phase 1 - npm Ecosystem Compatibility (0-4 months)

Goal: remove top blockers for Express/Nest/Prisma/Next custom server and Top100 basket.

Milestones:
1. Streaming `http/https` client/server baseline parity (`IncomingMessage`, `ClientRequest`, keepalive baseline).
2. `fs` async/fd lifecycle uplift (`open/read/write/close`, error codes baseline).
3. CJS/ESM resolver hardening (`exports`, `imports`, conditions, edge package boundaries).
4. Fetch stack upgrade (`fetch`, `Request`, `Response`, `Headers`) with body/stream semantics.
5. Streams parity baseline (`pipeline`, object mode safety, backpressure contract).

Exit criteria:
- Top100 QuickJS pass rate >= 95%.
- Express/Nest/Prisma KPI rows at 100%.
- No regressions in existing compatibility contracts.

Difficulty: High.

## Phase 2 - Advanced Node Parity (4-9 months)

Goal: close deep behavioral gaps that affect framework internals and high-concurrency apps.

Milestones:
1. Real `net` sockets and production `tls` behavior (ALPN/SNI/session options baseline).
2. `worker_threads` structured clone + transfer list + improved messaging semantics.
3. `async_hooks` / `AsyncLocalStorage` propagation parity across timers/io/workers.
4. Crypto expansion (`sign/verify/ciphers`) and WebCrypto baseline.
5. `node:test` runner semantics (timers/mocks behavior uplift).

Exit criteria:
- Top1000 smoke >= 85%.
- Node selected core suite >= 85% pass on approved subset.
- No critical mismatch in P0 runtime behavior contracts.

Difficulty: Extreme.

## Phase 3 - Enterprise Runtime Maturity (9-14 months)

Goal: production hardening and operational confidence.

Milestones:
1. Permission model GA for fs/network/child_process.
2. Security sandbox profiles (`dev`, `ci`, `prod`).
3. Observability improvements (`perf_hooks` useful metrics, trace output baseline).
4. Long-duration stability and leak hardening.

Exit criteria:
- 7-day soak tests pass with stable memory bounds.
- Security checklist complete for default profile.
- Regression escape rate near zero over two release cycles.

Difficulty: High.

## Phase 4 - Performance Leadership (14-18 months)

Goal: achieve best-in-class startup and strong throughput without parity regressions.

Milestones:
1. Built-in lazy init + module/bytecode cache v1.
2. Zero-copy buffer path adoption in high-traffic boundaries.
3. Worker pool tuning for crypto/zlib heavy paths.
4. Throughput and memory optimization against stable benchmark suite.

Exit criteria:
- Startup latency reduced by 30% from program baseline.
- Memory footprint reduced by 25% in representative workloads.
- HTTP throughput improved by 40% in keepalive benchmark profile.

Difficulty: High.

## Governance

- Weekly parity council: runtime, module loader, web API, and QA leads.
- Backlog triage driven by KPI impact and blocker count.
- Release gate requires KPI + reliability + security checks, not feature completion alone.

## Linked Operational Docs

- [EPICS_NODE_PARITY.md](EPICS_NODE_PARITY.md)
- [SPRINT_PLAN_Q1_Q2.md](SPRINT_PLAN_Q1_Q2.md)
- [WEEKLY_STATUS_TEMPLATE_NODE_PARITY.md](WEEKLY_STATUS_TEMPLATE_NODE_PARITY.md)

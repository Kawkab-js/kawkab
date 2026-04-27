# Sprint Plan Q1/Q2 - Node Parity Program

This plan translates epics into 12 execution sprints.
Each sprint is two weeks by default. Teams can overlap tracks when dependencies allow.

References:
- [ROADMAP_NODE_V23_MASTER.md](ROADMAP_NODE_V23_MASTER.md)
- [EPICS_NODE_PARITY.md](EPICS_NODE_PARITY.md)
- [COMPAT_KPI.md](COMPAT_KPI.md)

Quick navigation:
- [Docs index](INDEX.md)
- [Planning Assumptions](#planning-assumptions)
- [Sprint Waves](#sprint-waves)
- [Cross-Sprint Streams](#cross-sprint-streams)
- [Quality Gates by Quarter](#quality-gates-by-quarter)
- [Escalation Rules](#escalation-rules)

## Planning Assumptions

- Single parity release train with weekly integration branch.
- Compatibility QA works in parallel from Sprint 1.
- Critical regressions preempt feature work.

## Sprint Waves

| Sprint | Focus | Related Epics | Main Deliverable | Acceptance Criteria | Difficulty |
| --- | --- | --- | --- | --- | --- |
| Sprint 1 | Event loop contract and `nextTick` ordering | EPIC-02, EPIC-09 | Deterministic scheduling baseline | Selected ordering contracts green | High |
| Sprint 2 | HTTP streaming request/response baseline | EPIC-01 | `IncomingMessage` + `ClientRequest` MVP | Express/minimal smoke passes | High |
| Sprint 3 | Async fs core and fd lifecycle | EPIC-03 | Async fs path for common operations | fs selected subset >= 80% | High |
| Sprint 4 | Module resolver hard cases | EPIC-04 | `exports`/`imports` parity uplift | Top100 resolver blockers reduced by 40% | Medium |
| Sprint 5 | Fetch body/headers semantics | EPIC-05 | Better fetch body stream lifecycle | WPT fetch subset >= 85% | High |
| Sprint 6 | Crypto baseline expansion | EPIC-07 | Node crypto common algorithms parity | Crypto corpus rows pass | High |
| Sprint 7 | Net socket production behavior | EPIC-06 | Real `Socket` connect lifecycle | Socket stress 24h pass | Extreme |
| Sprint 8 | Worker transfer + structured clone | EPIC-08 | Transfer list for common payloads | Worker stress suite stable | High |
| Sprint 9 | Async hooks/ALS propagation | EPIC-09 | Propagation across io/timers/workers | Async hooks fixtures pass | Extreme |
| Sprint 10 | Permissions model GA path | EPIC-11 | fs/net/process policy profile baseline | Security e2e profile suite pass | High |
| Sprint 11 | Perf cache wave | EPIC-12 | Module + bytecode cache v1 | Startup latency trend improves by >= 20% | Medium |
| Sprint 12 | Hardening and phase gate | EPIC-10, EPIC-12 | Full release candidate gate run | Top100 >= 98%, soak 7d clean | High |

## Cross-Sprint Streams

## Stream A - Runtime Core

- Carries EPIC-01, EPIC-02, EPIC-03, EPIC-06.
- Owns runtime-level incident triage.

## Stream B - Module/Web

- Carries EPIC-04 and EPIC-05.
- Owns package-resolution regressions and WPT spec drift.

## Stream C - Concurrency/Security

- Carries EPIC-07, EPIC-08, EPIC-09, EPIC-11.
- Owns async correctness and capability model.

## Stream D - Compatibility QA and Perf

- Carries EPIC-10 and EPIC-12 with all streams.
- Owns dashboards, flaky quarantine, and gate quality.

## Quality Gates by Quarter

Q1 gate (after Sprint 6):
- Top100 QuickJS >= 95%.
- Express/Nest/Prisma KPI rows all green.
- No unresolved P0 parity blocker older than one sprint.

Q2 gate (after Sprint 12):
- Top100 QuickJS >= 98% stable.
- Top1000 smoke >= 85%.
- Reliability soak 7 days without crash or unbounded memory growth.

## Escalation Rules

- Any P0 compatibility regression blocks merge train until triaged.
- Any crash in runtime core or worker path triggers hotfix lane.
- Performance optimization is paused when parity gates are red.

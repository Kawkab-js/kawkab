# Documentation Index

Central navigation for Kawkab runtime and compatibility docs.

Quick navigation:
- [Core compatibility docs](#core-compatibility-docs)
- [Release and quality gates](#release-and-quality-gates)
- [Product planning and direction](#product-planning-and-direction)
- [Maintenance workflow](#maintenance-workflow)
- [Notes](#notes)

## Core compatibility docs

- Compatibility matrix (modules/globals): `NODE_COMPATIBILITY.md`
- Feature/runtime baseline contract: `FEATURE_BASELINE.md`
- Definition of done (`🟢/🟡/🔴` semantics): `COMPAT_DEFINITION_OF_DONE.md`
- Explicit non-goals and deferred areas: `NODE_NON_GOALS.md`

## Release and quality gates

- Release checklist and gates: `RELEASE_CHECKLIST.md`
- KPI targets and pass criteria: `COMPAT_KPI.md`
- Scenario corpus and smoke rows: `NPM_CORPUS.md`

## Product planning and direction

- Product vision and trade-offs: `PRODUCT_VISION.md`
- Node parity execution plan: `ROADMAP_NODE_V23_MASTER.md`
- Sprint planning: `SPRINT_PLAN_Q1_Q2.md`
- Epics overview: `EPICS_NODE_PARITY.md`
- Weekly status template: `WEEKLY_STATUS_TEMPLATE_NODE_PARITY.md`

## Quick start paths by role

- Maintainer (behavior change): `NODE_COMPATIBILITY.md` -> `FEATURE_BASELINE.md` -> `RELEASE_CHECKLIST.md`
- Reviewer (compat impact): `COMPAT_DEFINITION_OF_DONE.md` -> `NODE_COMPATIBILITY.md` -> `COMPAT_KPI.md`
- Release manager: `RELEASE_CHECKLIST.md` -> `COMPAT_KPI.md` -> `NPM_CORPUS.md`

## Maintenance workflow

When behavior or compatibility scope changes, update docs in this order:
1. `NODE_COMPATIBILITY.md` (status + "Remaining vs Node v23").
2. `FEATURE_BASELINE.md` (effective shipped behavior and constraints).
3. `COMPAT_KPI.md` and/or `NPM_CORPUS.md` (if KPI rows, denominators, or scenarios changed).
4. `RELEASE_CHECKLIST.md` (new/updated gate requirements).
5. `COMPAT_DEFINITION_OF_DONE.md` (only if policy/semantics changed).

## Documentation validation checks

Run these quick checks after doc edits:

- Terminology consistency:
  - `rg "Remaining vs Node" docs` (review results; only `Remaining vs Node v23` should remain)
- Link and anchor sanity (spot-check):
  - `rg "^Quick navigation:|Docs index|INDEX\\.md" docs/*.md`
- Unified docs consistency script:
  - `./scripts/docs_consistency_check.sh`
- Lint/diagnostics in editor:
  - Run workspace markdown lint/diagnostics and ensure no new warnings in edited files.

## Notes

- Main repository entry point: `../README.md`.
- Keep `NODE_COMPATIBILITY.md`, `FEATURE_BASELINE.md`, and `RELEASE_CHECKLIST.md` synchronized in the same change when behavior or gates change.
- Prefer `Remaining vs Node v23` wording for consistency across compatibility-related documents.

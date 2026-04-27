# Kawkab product vision

This document states **where Kawkab aims to win**, what is **explicitly out of scope** as a near-term promise, and **engineering themes** that matter more than chasing a single headline metric. For the contractual feature list and build targets, see `[FEATURE_BASELINE.md](FEATURE_BASELINE.md)`. For module-by-module Node alignment, see `[NODE_COMPATIBILITY.md](NODE_COMPATIBILITY.md)`.

Documentation map:
- Product direction and trade-offs: `PRODUCT_VISION.md` (this file).
- Runtime/platform contract and shipped behavior: `FEATURE_BASELINE.md`.
- Module/global compatibility matrix: `NODE_COMPATIBILITY.md`.
- Status semantics and promotion policy (`🟢/🟡/🔴`): `COMPAT_DEFINITION_OF_DONE.md`.
- Release gates and verification flow: `RELEASE_CHECKLIST.md`.
- Ecosystem KPI targets: `COMPAT_KPI.md`.

Quick navigation:
- [Docs index](INDEX.md)
- [Positioning](#positioning)
- [Engineering reality (QuickJS vs JIT engines)](#engineering-reality-quickjs-vs-jit-engines)
- [Platform](#platform)
- [Node.js compatibility stance](#nodejs-compatibility-stance)
- [Security (today vs direction)](#security-today-vs-direction)
- [Priority themes (not a dated roadmap)](#priority-themes-not-a-dated-roadmap)
- [Related documentation](#related-documentation)

## Positioning

Kawkab is a **Rust-centered** JavaScript runtime and toolchain that optimizes for:

- **Fast cold start** and low overhead via **QuickJS**, with **SWC** for TypeScript / JSX at runtime (no separate transpile step for typical scripts).
- **Integrated package management** (`pm`): install, lockfile, registry cache, and developer-facing commands such as `why` and `doctor`.
- **On-disk bytecode cache** (e.g. `.lbc`) keyed by content fingerprints (**Blake3**), so repeat runs can skip parse/transpile work.
- **Strong Linux I/O** when built with `**tokio-uring`** (primary development and performance path).
- **Native offload** for numeric workloads via `**kawkab.vec.`*** and related APIs, so hot paths need not stay in pure JS.

**Sweet spots:** CLI tools, short-lived scripts, edge- or serverless-style workloads with bursty I/O, and workflows that benefit from **immediate TS** plus a **small runtime footprint**.

## Engineering reality (QuickJS vs JIT engines)

QuickJS prioritizes **startup, memory, and embeddability**. It is **not** a JIT compiler like **V8**. Long-running, **CPU-bound** pure JavaScript will generally be **slower** than on Node.js for the same algorithmic code.

The CLI `**auto`** engine mode (try QuickJS, then **fall back to Node** if needed) is an **intentional bridge**: it preserves productivity for ecosystems that assume Node while still defaulting to the lightweight path when it works. Details of engine selection live in the CLI and baseline docs; see `[FEATURE_BASELINE.md](FEATURE_BASELINE.md)` and the root `[README.md](../README.md)`.

## Platform

The workspace’s **primary target** is **Linux** (including **WSL2**), with `**tokio-uring`** as the best-effort fast path where the kernel supports it. Other environments may build or run with reduced or alternate I/O behavior; authoritative platform notes are in `[FEATURE_BASELINE.md](FEATURE_BASELINE.md)` and the `io` crate rather than duplicated here.

## Node.js compatibility stance

Compatibility with Node is **best-effort** and **incremental**:

- This repo tracks **Node.js v23-oriented naming and expectations** in `[NODE_COMPATIBILITY.md](NODE_COMPATIBILITY.md)`, not a guarantee that **every** public npm package behaves identically.
- **Ecosystem-first targets** (e.g. Express, NestJS, Prisma, Next custom server, Top 100 package basket) are defined in `[COMPAT_KPI.md](COMPAT_KPI.md)` so roadmap work optimizes for **real stacks**, not a flat “every built-in” parity score.
- **No support** for native addons (`*.node` / N-API) unless and until explicitly documented otherwise.
- The hardest, most time-consuming gaps are usually **deep behavioral parity**: **streams**, **backpressure**, **timers/microtasks**, and **event-loop ordering** matching ECMAScript and Node semantics.

For “will my package run?”, use the matrix plus a **smoke test** on your own code paths; treat registry-wide compatibility as a **non-goal** unless stated otherwise.

## Security (today vs direction)

Today, sensitive host capabilities are **policy-gated** (for example `child_process` behind an explicit environment flag). `[FEATURE_BASELINE.md](FEATURE_BASELINE.md)` remains the **source of truth** for what is actually enforced in tree.

A **Deno-style** explicit permission model for network, filesystem, and subprocesses is a **plausible future direction**, not a committed shipping promise in this document.

## Priority themes (not a dated roadmap)

Work that tends to compound value for this stack:

1. **Conformance and tests** around the embedded **event loop**: microtasks, timers, and observable ordering—reducing subtle race conditions matters more than API surface sprawl alone.
2. **Clear documentation** of **per-platform** behavior (uring vs fallbacks, recommended environments).
3. **Native surface expansion** where Rust clearly wins (I/O-adjacent helpers, numeric kernels, crypto/codec paths) instead of reimplementing all of Node in JS shims.

## Related documentation

- **Feature and platform contract:** `[FEATURE_BASELINE.md](FEATURE_BASELINE.md)`
- **Node built-in / global matrix:** `[NODE_COMPATIBILITY.md](NODE_COMPATIBILITY.md)`
- **Ecosystem KPIs:** `[COMPAT_KPI.md](COMPAT_KPI.md)`
- **Release process:** `[RELEASE_CHECKLIST.md](RELEASE_CHECKLIST.md)`
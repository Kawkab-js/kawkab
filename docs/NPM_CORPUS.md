# Reference corpus (npm / Node parity smoke)

Use this list to **measure** compatibility improvements (Kawkab vs Node). It is **not** a full registry sweep.

## How to run

- **Rust API / resolution:** `cargo test -p kawkab-core` (includes `module_loader` tests).
- **CLI smoke:** run the same small scripts with `kawkab` and with `node` and diff stdout/exit code (add scripts under a future `fixtures/` tree as needed).
- **Optional:** port selected cases from the [Node.js test suite](https://github.com/nodejs/node/tree/main/test) (e.g. `test/parallel/test-fs-*`) — track filenames here when adopted.

## Packages / scenarios (seed list)

Expand as you harden each subsystem:

| Area | Scenario | Node cmd | Kawkab cmd |
|------|-----------|----------|------------|
| Modules | `exports` / `imports` / bare subpath | small local `package.json` fixtures | same |
| `process.env` | `NODE_ENV` affects `package.json` conditions | set env + `require()` package | same |
| `http` client | GET HTTPS endpoint | script using `http.get` / `https.get` | same |
| `fs` | `copyFileSync` / `rmSync` | temp dir script | same |

## Adding entries

For each new row: link or paste a **minimal repro** (≤ ~30 lines), expected exit code, and note any intentional Kawkab difference.

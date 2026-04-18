# PM diagnostics fixtures

This directory holds **optional stable fixtures** for package-manager diagnostics (for example JSON output from `kawkab why --json` or `kawkab doctor --json`).

## Policy

- Add a fixture only when the CLI output shape is considered a **supported contract** for tools or CI.
- Prefer small, hand-curated graphs (minimal `package.json` / lock snippets) over full registry trees.
- When intentionally changing output, update fixtures in the **same commit** as the code and document the change in the PR/release notes.
- If no fixtures exist yet, this README defines the convention for when they are introduced.

## Layout (convention)

Use subfolders by command when you add files, for example:

- `why/` — expected JSON or text snapshots for `kawkab why`
- `doctor/` — expected JSON for `kawkab doctor`

File names should describe the scenario (e.g. `peer_conflict.json`).

# AGENTS.md

Guidance for AI agents and maintainers working in this workspace. Read this
before editing.

## Workspace layout

This is a Rust workspace split along a deliberate dependency boundary:

- `argtuner` (root crate, `src/`) — the **tuner CLI**: project orchestration,
  the Watch TUI, PTY subprocess supervision, SQLite persistence, samplers and
  schedulers, `inspect`/`find`/`plan`/`rebuild-csv` subcommands.
- `crates/argtuner-sdk/` — the **lightweight training-side binding** that
  training/model binaries link: declaring parameters, parsing argv, emitting
  `::ARGTUNER::` telemetry, and generating the argtuner command template and
  search space via `#[tuner_params]`.
- `crates/argtuner-derive/` — the `#[tuner_params]` proc-macro.
- `crates/argtuner-common/` — shared wire types, constants, protocol schema.
- `crates/mock-bin/`, `crates/deprecated/` — test binaries and migration shims.

## Hard rules — keep the SDK out of the tuner crate

- **Never add `argtuner-sdk` as a dependency of the `argtuner` crate.**
- **Never move SDK code into `argtuner`** and never re-export SDK items from it
  (no `argtuner::init`, `argtuner::emit_*`, `argtuner::ParamRole`,
  `argtuner::tuner_params`, `argtuner::IpcChannel`, etc.).
- **Never add heavy dependencies to `argtuner-sdk`** (term-wm/ratatui,
  rusqlite, argmin, portable-pty, or anything from the tuner side). The SDK
  stays limited to small, ubiquitous crates: `clap`, `serde`, `serde_json`,
  `toml_edit`, `argtuner-common`, `argtuner-derive`.
- Keep `argtuner`'s dependency on `argtuner-sdk` strictly `dev`-only (tests and
  examples).

**Why this matters:** training binaries link `argtuner-sdk`. If SDK code ever
lands back in the `argtuner` crate, every user's model would transitively
compile and ship the entire tuner package (TUI, database, solvers) — exactly
the coupling this split exists to prevent. This is a deliberate, documented
"clean break" (see `CHANGELOG.md`).

## Conventions

- Training-side code imports the SDK through its single import surface:
  `use argtuner_sdk::prelude::*;` and writes roles as enum paths
  (`#[param(role = ParamRole::Tune)]`), which keeps IDE autocompletion and
  hover docs working.
- SDK and tuner crates are versioned and published together from the workspace
  root (`Cargo.toml`). Deprecated shims in `crates/deprecated/` only re-export
  from `argtuner-sdk` — do not move logic there.
- Before committing, verify the boundary holds:
  `cargo tree -i argtuner-sdk` must show the SDK is pulled in only by
  examples/tests/dev-deps, never by the `argtuner` library. (Enforced in CI by
  the "Check SDK boundary" step in `.github/workflows/rust-lint.yml`.)

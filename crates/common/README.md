# argtuner-common

Shared constants and protocol types for the [argtuner](https://crates.io/crates/argtuner) workspace. `argtuner` is a
black-box hyperparameter optimization CLI; this crate defines the wire format of
the "ipc" protocol a trial command speaks on stdout (`::ARGTUNER::`-prefixed
JSON lines).

> **Heads up:** not stable yet. The API is still settling, so expect breaking changes.

Highlights:

- `IpcMessage` — the single wire type; `protocol_schema()` generates the
  JSON Schema from it so the schema can never drift from the emitter.
- `RESULT_PREFIX` — the `::ARGTUNER::` line prefix the tuner parses.
- `EventKind` / event-name constants (`model.epoch_end`, `model.early_stopped`, ...).
- `render_starter_toml` / `STARTER_TEMPLATE_TOML` — starter `argtuner.toml` skeleton
  shared by all bindings.
- `STEP_PUBLISHER_PORT` — real-time step publisher for the TUI.

The canonical schema is committed at `assets/protocol.schema.json`; the full
protocol is documented in the [repository root README](https://github.com/jzombie/rust-argtuner/blob/main/README.md).

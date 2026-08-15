# argtuner-derive

Procedural macro helpers for [`argtuner-sdk`](https://crates.io/crates/argtuner-sdk) —
the lightweight client crate a training script uses to report metrics to
[`argtuner`](https://crates.io/crates/argtuner) (a black-box hyperparameter
optimization CLI) over stdout.

> **Heads up:** not stable yet. The API is still settling, so expect breaking changes.

`#[tuner_params]` — attribute macro for a plain struct of fields. It generates
the `argtuner_sdk::TunerParams` implementation, turning the struct into both a
production `clap` CLI and an argtuner template/search-space definition.

The SDK injects three flags:

- `--print-template` — print the rendered command template and exit.
- `--print-template-toml` — print a starter `argtuner.toml` and exit.
- `--print-protocol-schema` — print the ipc protocol JSON Schema and exit.

Requires a struct with named fields. Full API docs:
[`argtuner-sdk`](https://github.com/jzombie/rust-argtuner/blob/main/crates/argtuner-sdk/README.md).

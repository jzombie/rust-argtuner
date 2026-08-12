# argtuner-talkback-derive

Procedural macro helpers for `argtuner-talkback` — the client-side crate a
training script uses to report metrics to [`argtuner`](https://crates.io/crates/argtuner) (a black-box
hyperparameter optimization CLI) over stdout.

`#[talkback_args]` — attribute macro placed above `#[derive(Parser)]` on a clap
struct. It injects three `--arg(long, ...)` fields unless already present:

- `--print-template` — print the rendered command template and exit.
- `--print-template-toml` — print a starter `argtuner.toml` and exit.
- `--print-protocol-schema` — print the talkback protocol JSON Schema and exit.

Requires a struct with named fields. Full API docs:
[`argtuner-talkback`](https://github.com/jzombie/rust-argtuner/blob/main/bindings/rust/README.md).

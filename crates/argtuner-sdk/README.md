# argtuner-sdk

A lightweight Rust binding for [`argtuner`](https://crates.io/crates/argtuner), the
black-box hyperparameter optimization CLI. It lets your training program declare
its parameters once, parse its argv, and report metrics back to the tuner — all
over plain stdio, with no dependency on the argtuner CLI/TUI crates.

## Why a separate crate?

`argtuner` (the tuner binary) pulls in a large dependency tree: a terminal UI,
PTY process supervision, SQLite logging, and optimization algorithms. When the
SDK lived inside that crate, every machine-learning workload that only wanted to
*talk to* argtuner had to compile all of it. `argtuner-sdk` is the split: the
SDK is its own crate so training apps link only what they need.

## Overhead

The per-project overhead of depending on `argtuner-sdk` is **extremely low**:

- It depends only on a handful of small, ubiquitous crates (`clap`, `serde`,
  `serde_json`, `toml_edit`, `argtuner-common`, and the `argtuner-derive`
  proc-macro). No terminal, database, or process-supervision crates.
- At runtime it does nothing unless argtuner actually invoked your binary. Every
  `emit_*` helper and the whole binding no-ops when the `ARGTUNER_TUNING`
  environment variable is absent, so a standalone run of your training CLI keeps
  its `stdout` perfectly clean.

If performance is paramount you can skip the SDK **entirely**: the talkback
protocol is a documented, line-framed JSON schema and any language can emit
`::ARGTUNER::`-prefixed lines directly (see the argtuner README's protocol
section and `argtuner_common::protocol_schema_string()`). The SDK exists purely
for convenience and type safety.

## Usage

Declare your algorithm's parameters once as a plain struct with
`#[talkback_args]`; the derive generates a production `clap` CLI, the argtuner
command template, and a real search space:

```rust,no_run
use argtuner_sdk::{emit_metrics, init, talkback_args};

fn train(lr: f64, steps: usize) -> f64 {
    0.0 // your training logic
}

#[talkback_args]
struct Params {
    /// Learning rate
    #[param(default = 0.001, min = 0.0001, max = 0.1, log = true)]
    lr: f64,
    /// Training steps
    #[param(default = 100, min = 10, max = 1000)]
    steps: usize,
}

fn main() {
    let (_talkback, params) = init::<Params>();

    let val_loss = train(params.lr, params.steps);
    let _ = emit_metrics!("val_loss" => val_loss, "epoch" => params.steps);
}
```

Run it with `--print-template-toml` to generate a starter `argtuner.toml`
(`--print-template` prints the command template, `--print-protocol-schema` the
protocol JSON Schema). Without argtuner, the same binary is a normal CLI:
emission no-ops and `stdout` stays clean.

## Protocol

The SDK writes JSON lines to stdout, each prefixed with the literal
`::ARGTUNER::` marker. The tuner spawns your binary with `ARGTUNER_TUNING` set,
parses those lines, and uses the last `model.epoch_end` event for scoring. The
wire format is versioned by a handshake event and self-describes via a JSON
Schema; see `argtuner_common` for the canonical message types.

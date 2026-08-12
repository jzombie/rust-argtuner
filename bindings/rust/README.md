# argtuner-talkback

[`argtuner`](https://crates.io/crates/argtuner) is a black-box hyperparameter optimization CLI that reads trial metrics from a command's stdout. `argtuner-talkback` is the client-side crate that turns your **existing `clap` CLI** into an argtuner-compatible tuning target. Declare your algorithm's parameters once as a `clap` struct; the same definition becomes both a production command-line tool and a zero-touch target for argtuner.

It is the canonical reference for the protocol and is intentionally small so that other language bindings can mirror it easily. The protocol is self-describing: the wire shapes are defined once in
`argtuner-common` (`argtuner_common::TalkbackMessage`) and a JSON Schema is
generated from that single type, so the schema can never drift from what the
emitter writes or the tuner parses. The canonical schema is committed at
[`crates/common/assets/protocol.schema.json`](https://github.com/jzombie/rust-argtuner/blob/main/crates/common/assets/protocol.schema.json).

## Quickstart

Add the crates (note the `clap` feature — `init` requires it; `clap` and `serde`
are needed because the derive/emitters are built on them) to your `Cargo.toml`:

```toml
[dependencies]
argtuner-talkback = { version = "0.1.1-alpha", features = ["clap"] }
argtuner-talkback-derive = "0.1.1-alpha"
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
```

Declare your parameters once, then run your algorithm:

```rust
use argtuner_talkback_derive::talkback_args;
use clap::Parser;
use serde::Serialize;

#[derive(Serialize)]
struct TrialMetrics {
    loss: f64,
    epoch: usize,
}

#[talkback_args]
#[derive(Debug, Parser)]
struct Args {
    #[arg(long, default_value_t = 0.01)]
    /// Learning rate for the model
    lr: f64,

    #[arg(long, default_value_t = 100)]
    steps: usize,

    #[arg(long)]
    checkpoint_dir: Option<String>,
}

fn main() {
    let (talkback, args) = argtuner_talkback::init::<Args>();

    // ... run training using args.lr and args.steps ...

    let _ = talkback.emit_epoch_end(&TrialMetrics {
        loss: 0.042,
        epoch: args.steps,
    });
}
```

Run it two ways:

- **Standalone**: `./my_model --lr 0.001 --steps 500` behaves like a normal CLI
  with `--help`, defaults, and validation. `emit_*` calls silently no-op, so
  stdout stays clean.
- **Under argtuner**: point `argtuner run <project>` at it; argtuner drives the
  same binary as a trial job and reads the emitted metrics.

If you already depend on the `argtuner` crate, the same entry points are
re-exported from its root: `argtuner::init::<P>()`, `argtuner::Talkback`, and
`argtuner::talkback_args`.

## Execution lifecycle & flag interception

`init::<T>()` handles the talkback protocol transparently, before your
training logic runs:

- **Flag interception**: `#[talkback_args]` injects the
  `--print-template`, `--print-template-toml`, and `--print-protocol-schema`
  flags into your `clap` schema. `init` catches them, prints the generated
  template (or starter `argtuner.toml`, or the protocol JSON Schema) to
  `stdout`, and exits immediately — so argtuner can generate a project from
  your CLI definition.
- **Protocol handshake**: otherwise it emits the `::ARGTUNER::` binding-version
  event to `stdout` when running under argtuner, which lets the tuner verify
  protocol compatibility.
- **Argument parsing**: it delegates the remaining flags to `clap` and returns
  `(Talkback, T)`.

Emission is gated on an env marker the tuner always exports
(`ARGTUNER_TUNING=1`). `is_tuning_active()` reports the state; when false,
every `emit_*` helper (free functions and `Talkback` methods alike) returns
`Ok(())` without writing, so standalone runs stay clean.

## Core API reference

- **Initialization**:
  - `init::<T>()` — the unified entry point (intercept flags, handshake, parse args).
  - `Talkback::init()` — capture argv and emit the version handshake; works without `clap`.
  - `parse_args::<T>()` / `args_map()` — deserialize raw CLI flags into your struct.
- **Metric emitters** (write `::ARGTUNER::`-prefixed JSON to `stdout`; no-op when not under argtuner):
  - `emit_event(kind, &payload)` — the unified emitter.
    - `EventKind::Result`: emits a result payload.
    - `EventKind::EpochEnd`: emits an epoch end event.
    - `EventKind::EarlyStopped`: emits an early stop event.
  - `emit_epoch_end(&payload)` / `emit_step_end(&payload)` / `emit_result(&payload)`.
  - Emitters take a `Serialize` **struct** (or map) — the metric key must match
    the `metric_key` in `argtuner.toml`, so pass named fields, not a bare scalar.
- **Template utilities** (require the `clap` feature):
  - `render_template_command::<T>()` / `render_template_toml::<T>()`.
  - `maybe_print_template_and_exit::<T>()` / `maybe_print_protocol_schema_and_exit()` / `print_protocol_schema()`.

### Without `clap`

`Talkback::init()`, `args_map()`, `parse_args()`, and all `emit_*` helpers work
without the `clap` feature — only the template/auto-generation helpers
(`init`, `render_template_*`, `maybe_print_*`) are clap-gated. Omit
`features = ["clap"]` (and the `clap` dependency) if you parse your own flags:

```rust
let tb = argtuner_talkback::Talkback::init();
let args = tb.parse_args::<MyArgs>()?; // deserialize from raw argv
tb.emit_epoch_end(&my_metrics)?;       // emit a typed result (no-op standalone)
```

### Derive helper

`argtuner-talkback-derive` provides `#[talkback_args]` to inject
`--print-template`, `--print-template-toml`, and `--print-protocol-schema`
into a clap `Parser` struct. Put it above `#[derive(Parser)]`.

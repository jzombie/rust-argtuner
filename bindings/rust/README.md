# argtuner-talkback

[`argtuner`](https://crates.io/crates/argtuner) is a black-box hyperparameter optimization CLI that reads trial metrics from a command's stdout. `argtuner-talkback` turns a **plain Rust struct** into both a production command-line tool and an argtuner tuning target: declare your algorithm's parameters once, and the `#[talkback_args]` derive generates the `clap` interface (help, defaults, validation), the argtuner command template, and a real search space for you.

It is the canonical reference for the protocol and is intentionally small so that other language bindings can mirror it easily. The protocol is self-describing: the wire shapes are defined once in
`argtuner-common` (`argtuner_common::TalkbackMessage`) and a JSON Schema is
generated from that single type, so the schema can never drift from what the
emitter writes or the tuner parses. The canonical schema is committed at
[`crates/common/assets/protocol.schema.json`](https://github.com/jzombie/rust-argtuner/blob/main/crates/common/assets/protocol.schema.json).

## Quickstart

Add only these two crates — `clap` and `serde` come along as dependencies of the
binding, so your `Cargo.toml` stays minimal:

```bash
cargo add argtuner-talkback --features clap
cargo add argtuner-talkback-derive
```

Declare your parameters once, then run your algorithm:

```rust
use argtuner_talkback_derive::talkback_args;

#[talkback_args]
struct ModelParams {
    /// Learning rate for optimization
    #[param(default = 0.001, min = 0.0001, max = 0.1, log = true)]
    lr: f64,
    /// Number of training steps
    #[param(default = 100, min = 10, max = 1000)]
    steps: usize,
    /// Optimizer implementation
    #[param(default = "adamw", choices = ["adam", "adamw", "sgd"])]
    optimizer: String,
    /// Checkpoint directory (reserved: trial_dir)
    #[param(value_name = "trial_dir")]
    checkpoint_dir: Option<String>,
}

fn main() {
    let (talkback, params) = argtuner_talkback::init::<ModelParams>();

    // ... run training using params.lr, params.steps, &params.optimizer ...

    argtuner_talkback::emit_metrics! { "loss" => 0.042, "epoch" => params.steps };
}
```

The same binary works two ways:

- **Standalone**: `./my_model --lr 0.001 --steps 500` behaves like a normal CLI
  with `--help`, defaults, and validation. Emission silently no-ops, so stdout
  stays clean.
- **Under argtuner**: point `argtuner run <project>` at it; argtuner drives the
  same binary as a trial job and reads the emitted metrics.

If you already depend on the `argtuner` crate, everything is re-exported from
its root: `argtuner::init::<P>()`, `argtuner::talkback_args`,
`argtuner::Params`, `argtuner::emit_metrics!`.

## How the derive maps your struct

- **CLI**: each field becomes a `--flag <name>` argument. `f64`/integer/`String`
  fields use typed value parsers; `bool` fields become flags; `Option<T>` fields
  are optional. Doc comments become `--help` text.
- **Defaults**: `#[param(default = ...)]` sets the clap default. Fields without
  a default (and not `Option`) are required — clap errors cleanly if omitted.
- **Choices**: `#[param(choices = [...])]` on a `String` field validates values
  at the CLI and generates a `Choice` entry in the search space.
- **Search space**: fields with `min`/`max` (and optional `log`/`step`) generate
  `Float`/`Int` entries; `choices` generate `Choice` entries; boolean flags,
  reserved `value_name` placeholders (`trial_dir`/`trial_id`), and unannotated
  scalars are excluded (fixed CLI args, baked into the template as their
  default).

## Execution lifecycle & flag interception

`init::<T>()` handles the talkback protocol transparently, before your training
logic runs:

- **Flag interception**: it pre-inspects raw `std::env::args()` for
  `--print-template`, `--print-template-toml`, and `--print-protocol-schema`,
  printing the generated template (or starter `argtuner.toml` with a populated
  `[space]`, or the protocol JSON Schema) to `stdout` and exiting 0 — before any
  clap parsing runs.
- **Protocol handshake**: otherwise it emits the `::ARGTUNER::` binding-version
  event to `stdout` when running under argtuner, which lets the tuner verify
  protocol compatibility.
- **Argument parsing**: it parses the remaining flags via the generated `clap`
  command and returns `(Talkback, T)`.

Emission is gated on an env marker the tuner always exports
(`ARGTUNER_TUNING=1`). `is_tuning_active()` reports the state; when false,
every `emit_*` helper (free functions, `Talkback` methods, and the
`MetricsBuilder`) returns `Ok(())` without writing, so standalone runs stay clean.

## Emission

- **Serde-free** (no derive needed): `emit_metrics! { "loss" => loss, "epoch" => e }`
  (keys are expressions, so variables/`format!(…)` work), or a builder:
  ```rust
  talkback
      .metrics()
      .record("loss", loss)
      .record("epoch", epoch)
      .emit()?;          // model.epoch_end
      // .emit_step()?    // model.step_end
      // .emit_result()? // flat result payload
  ```
- **Serde-based** (for complex payloads): `emit_event(kind, &struct)`,
  `emit_epoch_end(&struct)`, `emit_step_end(&struct)`, `emit_result(&struct)`,
  where the struct implements `argtuner_talkback::serde::Serialize`
  (re-exported — no direct serde dependency).

## Core API reference

- **Initialization**: `init::<T: Params>()` (unified entry; `init_with_args` is
  an alias), `Talkback::init()` (works without the `clap` feature),
  `parse_args::<T>()` / `args_map()`.
- **Template utilities** (require the `clap` feature):
  `render_template_command::<T>()`, `render_template_toml::<T>()`,
  `maybe_print_template_and_exit::<T>()`, `maybe_print_protocol_schema_and_exit()`,
  `print_protocol_schema()`.
- **Derive**: `argtuner-talkback-derive` provides `#[talkback_args]` and the
  `#[param(...)]` helper attribute.

### Without `clap`

`Talkback::init()`, `args_map()`, `parse_args()`, and all `emit_*` helpers work
without the `clap` feature — only the derive and the template/auto-generation
helpers (`init`, `render_template_*`, `maybe_print_*`) are clap-gated.

# argtuner-sdk

`argtuner-sdk` provides lightweight, type-safe Rust bindings for [`argtuner`](https://crates.io/crates/argtuner), a black-box hyperparameter optimization CLI. It enables training programs to define search spaces, parse CLI arguments, and emit evaluation telemetry over line-framed `stdio`.

## Architecture & Dependency Isolation

The main `argtuner` package houses the complete orchestration engine: terminal UI components, process supervision, SQLite persistence, and optimization algorithms.

`argtuner-sdk` decouples target training workloads from that execution engine. Target binaries link exclusively to standard serialization and argument parsing primitives (`clap`, `serde`), completely omitting heavy UI and database crates (`ratatui`, `rusqlite`, `argmin`, `portable-pty`).

## Runtime Design & Performance

* **Zero-Cost Standalone Execution:** Telemetry handlers remain completely inert unless the process is spawned within an active tuning session (`ARGTUNER_TUNING=1`). Standard invocation generates zero output overhead and preserves clean stdout streams.
* **Minimal Footprint:** Transitive dependencies are strictly scoped to ubiquitous serialization and utility crates (`clap`, `serde`, `serde_json`, `toml_edit`, `argtuner-common`, and `argtuner-derive`).
* **Direct Protocol Autonomy:** The SDK wraps `argtuner`'s line-framed JSON stdio protocol. Workloads requiring zero external dependencies can omit the SDK entirely and write `::ARGTUNER::`-prefixed wire frames directly (documented in `argtuner-common`).

## Usage

Annotate hyperparameter definitions with `#[talkback_args]`. The macro derives the CLI parser, command template generator, and search space AST:

```rust,no_run
use argtuner_sdk::{emit_metrics, init, talkback_args};

fn train(lr: f64, steps: usize) -> f64 {
    0.0 // Training loop logic
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

### Built-in Introspection

Target binaries expose self-inspection flags for automated setup:

* `--print-template-toml`: Generates a fully configured `argtuner.toml` project manifest.
* `--print-template`: Outputs the CLI command invocation template.
* `--print-protocol-schema`: Exports the talkback protocol JSON Schema.

## Wire Protocol

The SDK emits line-delimited JSON frames to `stdout`, identified by the literal `::ARGTUNER::` marker prefix. During an active session, `argtuner` ingests these events to track evaluation scoring via `model.epoch_end` payloads. Protocol contracts are versioned via initial handshake frames and formally defined in `argtuner-common`. 

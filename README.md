# argtuner

[![macOS][macos-badge]][ci] [![Linux][linux-badge]][ci] [![Windows][windows-badge]][ci]
<br>
[![Made with Rust][rust-logo]][rust-src-page] [![crates.io][crates-badge]][crates-page] [![MIT licensed][mit-license-badge]][mit-license-page] [![Apache 2.0 licensed][apache-2.0-license-badge]][apache-2.0-license-page] [![Coverage][coveralls-badge]][coveralls-page] [![CodeQL][codeql-badge]][codeql-page]

[`argtuner`](https://crates.io/crates/argtuner) is a language-agnostic CLI tool for black-box hyperparameter optimization. It repeatedly executes a target script or binary while systematically varying its command-line arguments to find the optimal configuration.

By defining a search space in `argtuner.toml` and templating your command (e.g., `--lr {lr}`), `argtuner` orchestrates trials using algorithms like Particle Swarm Optimization (PSO) or Successive Halving. It reads trial metrics directly from the process `stdout` and logs results to a local SQLite/CSV database.

## When it fits

Use argtuner when:
- You already have a runnable training command and just need structured, repeatable search.
- You want a simple, project-based workflow that logs trials and lets you re-run exact commands.
- You want black-box tuning: `argtuner` never instruments your model's internals (no gradients, no layer access). Integration happens strictly at the process boundary—your app simply emits `::ARGTUNER::` JSON events on stdout, manually or via the optional [`argtuner-sdk`](https://crates.io/crates/argtuner-sdk) Rust binding.
- Your command's result is reproducible for a given configuration: argtuner re-invokes it with fresh arguments each trial and compares the reported results, so the same arguments should return close to the same result. State you manage is fine — successive halving even copies the previous rung's `{trial_dir}` artifacts into the next rung's directory so a trial can resume from checkpoints. What skews the search is *uncontrolled* state: files written outside `{trial_dir}`, randomness that isn't seeded, or network calls that change results run to run.

## Quickstart

**1. Declare your parameters once, in Rust.** Add the lightweight
`argtuner-sdk` crate to your app (it also depends on the `argtuner-derive`
proc-macro, so you only need to name one crate):

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
    /// Checkpoint directory (reserved: trial_dir)
    #[param(value_name = "trial_dir")]
    checkpoint_dir: Option<String>,
}

fn main() {
    let (_talkback, params) = init::<Params>();

    // ... your training logic using params.lr, params.steps ...
    let val_loss = train(params.lr, params.steps);

    let _ = emit_metrics!("val_loss" => val_loss, "epoch" => params.steps);
}
```
The derive generates the full CLI (`--help`, defaults, validation) and the
argtuner command template.

**2. Auto-generate your project configuration.** Run your binary with
`--print-template-toml` — argtuner writes the command template and a populated
`[space]` for you:

```bash
./my_model --print-template-toml > argtuner.toml
# set [project] metric_key to the key your app emits, e.g. "val_loss"
```

**3. Run the optimization campaign.**

```bash
argtuner run .
```

**Use it directly, too.** The same binary is a normal CLI — run it for
single-shot training, evaluation, or inference without argtuner:

```bash
./my_model --lr 0.002 --steps 200
```

`--help`, defaults, and validation all work; emission silently no-ops when not
running under argtuner, so `stdout` stays clean.

## Declaring parameters

Each field of your `#[talkback_args]` struct becomes a `--flag <name>` CLI
argument and a template placeholder; its doc comment becomes the `--help` text.
`#[param(...)]` hints control the rest:

- `default = 0.001` — CLI default; fields without one are required (unless `Option<T>`).
- `min = …` / `max = …` — search-space bounds → `Float`/`Int` `[space]` entry.
- `log = true` — log-scale range (floats).
- `step = …` — stepped (linear) range.
- `choices = ["a", "b"]` — categorical → `Choice` `[space]` entry + CLI validation.
- `skip = true` — exclude the parameter from the search space while keeping its
  CLI argument (e.g. an operational `--verbose` bool you don't want tuned).
- `value_name = "trial_dir"` — reserved placeholder, injected by argtuner.
- `long = "checkpoint-dir"` — override the `--flag` name.

Supported field types: `f64`/`f32`, integers (`usize`, `i64`, …), `String`,
`bool` (a tunable `Bool` `[space]` entry, parsed flag-style: `--use-dropout`
means `true`, `--use-dropout false` means `false`), and `Option<T>` (optional);
other `FromStr` types work too. Fields with no `min`/`max`/`choices` are fixed
CLI args — baked into the generated template as their default and excluded from
`[space]`.

- **TUI & progress bars:** if your application uses interactive terminal
  loggers (e.g. `burn-rs` or `indicatif`), gate them on
  [`is_tuning_active()`](#headless-execution--tui-frameworks) to disable UI
  rendering during tuning runs.

### Headless execution & TUI frameworks

On non-Windows platforms, `argtuner` spawns child trials inside a
pseudo-terminal (PTY). Interactive loggers and progress-bar frameworks (such as
`burn-rs`'s `LearnerBuilder` display or `indicatif`) will detect a TTY and
attempt to render.

While `argtuner`'s line parser ANSI-strips `stdout` and successfully extracts
`::ARGTUNER::` protocol events without breaking, streaming full TUI redraw
frames into captured logs generates unnecessary IO overhead. Disable interactive
UI rendering when tuning is active:

```rust,ignore
use argtuner_sdk::{init, is_tuning_active};
use burn::train::LearnerBuilder;

fn main() {
    let (_talkback, params) = init::<Params>();

    let mut builder = LearnerBuilder::new(&params.checkpoint_dir);

    // Skip TUI rendering during argtuner campaigns
    if !is_tuning_active() {
        builder = builder.log_to_terminal();
    }

    let learner = builder.build(...);
}
```

Standalone runs keep the native interactive TUI; during `argtuner run` the
binary runs headless and you monitor live via `argtuner watch`.

## Installation

Building from source requires a **stable** Rust toolchain (1.85+ for edition 2024) and a C compiler for the bundled SQLite (`rusqlite` builds it from source): Xcode command-line tools on macOS, `build-essential` on Linux, or the MSVC Build Tools on Windows.

From a checkout of this repository:

```bash
# Build the debug binary (target/debug/argtuner)
cargo build

# Build an optimized release binary (target/release/argtuner)
cargo build --release

# Install the argtuner binary into ~/.cargo/bin
cargo install --path .
```

Verify the install:

```bash
argtuner --help
```

During development you can run it in-place without installing:

```bash
cargo run -p argtuner -- --help
```

Run the test suite (matches CI):

```bash
cargo test --workspace --all-features
```

> Note: `.cargo/config.toml.example` is only needed when hacking on the
> extracted `term-wm` UI crates locally; a normal build resolves them from
> crates.io and needs no extra configuration.

## Configuration showcase

A single worked example ties the whole template/config model together. It lives
at [`examples/config_showcase/`](https://github.com/jzombie/rust-argtuner/blob/main/examples/config_showcase/README.md)
and is fully runnable (`argtuner run examples/config_showcase`).

Each trial runs a command rendered from the `template` field. Use
`{placeholder}` tokens for tunable values; double braces `{{`/`}}` escape
literal braces. Here is the showcase's `argtuner.toml` in full:

<!-- config.example -->
```toml
template = '''
  cargo run -p argtuner --example loss_pattern_generator -- \
    --pattern noisy \
    --steps {steps} \
    --noise {noise} \
    --metric-key val_loss \
    --checkpoint-dir {trial_dir} \
    --epoch-time 1
'''

[project]
metric_key = "val_loss"
goal = "min"
inject_trial_placeholders = true

[sampler]
type = "random"

[scheduler]
type = "successive_halving"
n_trials = 30
seed = 42

[scheduler.successive_halving]
budget_placeholder = "steps"
min_epochs = 20
max_epochs = 300
eta = 3

[space]
[[space.params]]
type = "Float"
name = "noise"
min = 0.01
max = 0.5
log = true

[[space.params]]
type = "Int"
name = "steps"
min = 20
max = 300
step = 20
```

A few things to notice:
- `{steps}` and `{noise}` are sampled from the search space (`[space]`).
- `{steps}` is also the successive-halving budget placeholder, so each rung
  overrides it (`[scheduler.successive_halving]`).
- `{trial_dir}` is a reserved placeholder injected automatically.

Reserved placeholders injected automatically:
- `{trial_id}`: the numeric trial id.
- `{trial_dir}`: per-trial artifacts directory, always `argtuner/<project>/artifacts/trial_{trial_id}`.
  Successive-halving promotions copy the parent rung's artifacts into the child
  trial's directory rather than sharing a directory across rungs.

To see how argtuner interprets that file — the rendered template, every
`{placeholder}` resolved against the search space (or auto-injected), and a
one-glance execution summary — run:

```bash
argtuner inspect examples/config_showcase
```

<!-- config.inspect.output -->
```text
project: examples/config_showcase

template:
  cargo run -p argtuner --example loss_pattern_generator -- --pattern noisy --steps {steps} --noise {noise} --metric-key val_loss --checkpoint-dir {trial_dir} --epoch-time 1

placeholders:
  {noise}:     space param Float in [0.01, 0.5], log-scale
  {steps}:     space param Int in [20, 300], step 20; scheduler budget placeholder (overridden per rung)
  {trial_dir}: reserved: per-trial artifact directory (auto-injected)

execution:
  metric:    val_loss (minimize)
  sampler:   random
  scheduler: successive_halving (30 trials)
```

The execution flow is:
 - Render command template with sampled hyperparameters (and optional trial
   placeholders).
 - Spawn the command directly (program + args) with stdout/stderr streamed.
 - Parse stdout lines that start with the crate prefix `::ARGTUNER::` as JSON events.
 - Extract `metric_key` from the last `model.epoch_end` event to compute the `score`.
 - Write/update `trials.csv` with:
   - core fields (`trial_id`, `status`, `elapsed_ms`, `error`, `metric`, `score`)
   - `metric.*` fields from the last `model.epoch_end` event
   - `trial.*` fields (budget/rung/bracket, trial_dir, etc.)
   - `hp.*` fields from the search space

Notes:
- `[project]` holds run-wide metadata (metric parsing, goal, pruner, and trial placeholder injection).
- `[sampler]` names the sampler (`pso` or `random`). Sampler-specific knobs live under `[sampler.<sampler_name>]`, e.g. `pso.iters` and `pso.particles`.
- `[scheduler]` picks `fixed` or `successive_halving`. Scheduler knobs (like `n_trials`, `seed`, and halving budgets) stay inside `[scheduler]` and its child tables.
- `n_trials` now belongs to the scheduler because the `fixed` and `successive_halving` schedulers manage the evaluation budget.
- `trial_timeout_s` (optional, default `0` = disabled) sets a hard per-trial deadline in seconds. When a trial command exceeds it, its whole process group is terminated and the trial is recorded as an error, so a hung training run can't stall the search.
- Scheduler type names always match their child tables: `type = "successive_halving"` pairs with `[scheduler.successive_halving]`, so you never need to memorize separate spellings.
- `[space]` is still a top-level table describing the search space; it intentionally sits alongside the other sections for clarity.
- `argtuner inspect <dir>` shows how argtuner interprets a project's config: the rendered template, each `{placeholder}` resolved against the search space (or reserved/injected), and an execution summary (metric, goal, sampler, scheduler).
- When the `random` sampler hits a duplicate in a fully discrete space (choices/ints/stepped floats), it will try unused configs up to the exhaustive cap before continuing with random sampling.

## Multi-objective optimization (Pareto frontier)

Declare several objectives under `[project]`; each is an independent metric the
search optimizes against a Pareto frontier instead of a single scalar:

```toml
[project]
metric_key = "loss"
goal = "min"

[[project.objectives]]
name = "loss"
goal = "min"
primary = true

[[project.objectives]]
name = "latency_ms"
goal = "min"
```

Rules:

- `name` is the metric key the trial payload emits (a `metric.<name>` field).
- `goal` is `min` or `max` per objective. Direction is normalized internally
  (maximize objectives are negated for dominance) — stored and displayed values
  stay raw.
- Exactly one objective must set `primary = true`. The primary drives
  successive-halving rung truncation and the scalar `score`/`metric` columns.
- Multi-objective requires the `random` sampler (`pso` is single-objective).
- Without `[[project.objectives]]`, behavior is unchanged: `metric_key` +
  `goal` drive a single-objective run.

Runs report the non-dominated frontier at the end (e.g. `Pareto frontier (2 of
3 trials)`), and `argtuner inspect` lists the objectives.

## CLI subcommands

argtuner ships six subcommands:

- `run` — run an optimization campaign against a project.
- `inspect` — show what argtuner parsed from a project's config: the
  deserialized structs, the template, and the placeholder analysis (see
  [Configuration showcase](#configuration-showcase)).
- `rebuild-csv` — rebuild `trials.csv` from `trials.sqlite` (for example, after
  manual edits or a corrupt CSV). Tuning runs automatically rebuild this after
  each trial.
- `watch` — live TUI dashboard for monitoring trials and metrics as they run
  (see [Watch](#watch-live-tui) below).
- `find` — recursively locate argtuner projects.
- `plan` — show the scheduler plan for a project (optionally a specific config id).

```bash
argtuner run ./argtuner/my-project
argtuner inspect examples/config_showcase
argtuner rebuild-csv ./argtuner/my-project
argtuner plan ./argtuner/my-project
argtuner plan ./argtuner/my-project --config-id 3
```

### Watch (live TUI)

`argtuner watch` opens a live terminal dashboard while a campaign is running.
It monitors the project's `trials.sqlite` and prints the schedule/status of each
trial. `--project` is required.

```bash
argtuner watch --project ./argtuner/my-project
argtuner watch --project ./argtuner/my-project --poll-ms 5000
```

- `--poll-ms` controls how often the dashboard re-reads the database
  (default **5000 ms**). The first poll fires immediately on launch; later
  polls re-run on that interval.
- The dashboard has five windows:
  - **Trials** — the trial list (id, status, metric). Title is `Trials`.
  - **Charts** — metric curves for the selected trial. Title is
    `Trial {id} - Metric Curves` (`Trial {id} - Metric Curve {n}/{total}` while
    a single curve is focused, or `Trial {id} - Hyperparameter Space` in
    hyperparameter mode).
  - **Trial Details** — per-trial fields/epochs. Title is `Trial {id} Details`.
    Text is selectable/copyable: drag to select, release to copy.
  - **Hyperparameters** / **Metrics** — toggled in hyperparameter mode (see
    `h` below).
- Keybindings:
  - `q` — quit.
  - `h` — toggle between Metric-curves mode and Hyperparameter-space mode.
  - `f` / `Enter` / `Space` — toggle between the chart list and a single
    focused metric curve.
  - `+` / `=` — zoom in on a metric curve; `-` — zoom out; `0` — reset zoom.
  - `Left` / `Right` — pan hyperparameter axes (hyperparameter mode).
  - `Up` / `k` and `Down` / `j` — move the chart selection; `PageUp`/`PageDown`,
    `Home`/`End` — scroll.
- The bottom hint bar in the Charts window always shows the zoom/view keys.
- The **Debug Log** is available from the command palette: press `Ctrl+A`, then
  select "≣ Debug Log". It streams timestamped logs from the running campaign
  (including each poll tick).

## Protocol

### The `argtuner-sdk` Rust binding
If your application is written in Rust, you do not need to format these JSON strings manually. Add the **`argtuner-sdk`** crate and declare your algorithm's parameters once as a **plain struct** with `#[talkback_args]`; the derive generates the `clap` CLI, the command template, and a real search space (`argtuner_sdk::init::<P>()`, `argtuner_sdk::talkback_args`, `argtuner_sdk::Params`, and `argtuner_sdk::emit_metrics!` come from that crate's root; no `clap`/`serde` needed in your `Cargo.toml`). It provides:
* **Type-safe emission:** `emit_metrics!` / `talkback.metrics()` and the serde-backed `emit_epoch_end()`, `emit_step_end()`, and `emit_result()` methods write `::ARGTUNER::` JSON to stdout (silently no-op when the binary runs standalone, so the same CLI doubles as a clean production tool).
* **CLI auto-generation:** `--print-template-toml` prints a starter `argtuner.toml` — populated `[space]` included — directly from your struct definition.
* **Version handshake:** Ensures compatibility between your app and the CLI.
* **Typed argv parsing:** The derive parses flags for you and returns `(Talkback, Params)` via `init()`.

`argtuner-sdk` is a deliberately tiny crate: it pulls in only a handful of
small, common dependencies (`clap`, `serde`, `serde_json`, `toml_edit`,
`argtuner-common`, `argtuner-derive`) — **none** of the CLI/TUI machinery
(terminal UI, PTY supervision, SQLite, optimization solvers) that ships with
the `argtuner` binary. The per-project overhead of using it is therefore
**extremely low**, and at runtime it does nothing unless argtuner exported the
`ARGTUNER_TUNING` environment variable to your process. If critical performance
is paramount, the SDK can be **skipped entirely**: the protocol below is a
plain, documented stdio format, and any language can emit the raw
`::ARGTUNER::` JSON lines directly.

For all other languages, simply emit the following raw JSON strings to standard output:

### Result fields / event protocol
- Any stdout line can emit an ARGTUNER message using the fixed `::ARGTUNER::` prefix; the tuner uses the last `model.epoch_end` event for scoring.
- Messages are JSON after the prefix. Supported payloads are:
  - `{"type":"event","name":"model.epoch_end","fields":{...}}` (each event appends a trial row; the last one drives scoring)
  - `{"type":"event","name":"model.early_stopped","fields":{...}}`
  - `{"type":"event","name":"model.step_end","fields":{...}}` (per-step metrics streamed live to the TUI)
  - `{"type":"event","name":"model.invalid_config","fields":{"error":"..."}}` (marks the trial `error`)
  - `{"type":"event","name":"tuner.binding_version","fields":{"version":"..."}}` (version handshake at startup)
  - `{"type":"result","fields":{...}}` (a flat result dump; each field becomes a top-level trial field)
- argtuner exports environment variables to the command:
  - `ARGTUNER_TRIAL_ID` / `ARGTUNER_TRIAL_DIR`

The protocol is **self-describing**: a JSON Schema generated from the shared
`argtuner_common::TalkbackMessage` type is committed at
[`crates/common/assets/protocol.schema.json`](https://github.com/jzombie/rust-argtuner/blob/main/crates/common/assets/protocol.schema.json), and any talkback binary can print
the current schema with `--print-protocol-schema`. The schema validates the
JSON document after the prefix; the prefix/ANSI line framing is documented in
its `x-argtuner` extension object.

The canonical schema is echoed below, collapsed to keep this doc skimmable;
`tests/readme_assertions.rs` asserts it is byte-identical to the generated
schema, so it cannot go stale:

<!-- protocol.schema.json -->
<details>
<summary>Canonical protocol JSON schema</summary>

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "argtuner talkback protocol",
  "description": "Line-framed JSON protocol spoken over subprocess stdout. Each stdout line is ANSI-stripped; the first occurrence of the literal prefix `::ARGTUNER::` marks the start of a message, and the JSON document after the prefix must match this schema. Field values are always strings on the wire.",
  "oneOf": [
    {
      "description": "A typed protocol event, e.g. `model.epoch_end`.",
      "type": "object",
      "properties": {
        "fields": {
          "type": "object",
          "additionalProperties": {
            "type": "string"
          },
          "default": {}
        },
        "name": {
          "title": "event name",
          "description": "Canonical event name, or a legacy un-namespaced alias.",
          "type": "string",
          "enum": [
            "binding_version",
            "early_stopped",
            "epoch_end",
            "invalid_config",
            "model.early_stopped",
            "model.epoch_end",
            "model.invalid_config",
            "model.step_end",
            "step_end",
            "tuner.binding_version"
          ]
        },
        "type": {
          "type": "string",
          "const": "event"
        }
      },
      "required": [
        "type",
        "name"
      ]
    },
    {
      "description": "A flat result dump. No `name`; each field becomes a top-level trial field on the tuner side.",
      "type": "object",
      "properties": {
        "fields": {
          "type": "object",
          "additionalProperties": {
            "type": "string"
          },
          "default": {}
        },
        "type": {
          "type": "string",
          "const": "result"
        }
      },
      "required": [
        "type"
      ]
    }
  ],
  "x-argtuner": {
    "linePattern": "^.*::ARGTUNER::.*$",
    "namespaces": [
      "metric",
      "model",
      "tuner"
    ],
    "prefix": "::ARGTUNER::",
    "protocol": "argtuner.talkback",
    "stripAnsi": true
  }
}
```

</details>

For example:
```text
::ARGTUNER::{"type":"event","name":"model.epoch_end","fields":{"metric":"0.123","aux":"42","epoch":"1"}}
::ARGTUNER::{"type":"event","name":"model.early_stopped","fields":{}}
```

Extracting a payload from a stdout feed:
- Tap the protocol by scanning each stdout line for the literal `::ARGTUNER::`
  prefix: strip ANSI escape codes, find the first occurrence of the prefix, and
  JSON-parse everything after it. Lines without the prefix are ignored.
- Take this feed (`\x1b[` sequences are ANSI color codes):

<!-- protocol.example.feed -->
```text
Epoch 1/10: train loss 0.5234, val loss 0.6102
\x1b[36m::ARGTUNER::\x1b[0m{"type":"event","name":"model.epoch_end","fields":{"metric":"0.6102","epoch":"1"}}
```

- The second line, ANSI-stripped, is the prefix followed by one JSON message:

<!-- protocol.example.parsed -->
```json
{"type":"event","name":"model.epoch_end","fields":{"metric":"0.6102","epoch":"1"}}
```

- `tests/readme_assertions.rs` runs the real parser on this exact feed and
  asserts it produces exactly that message.

## Optimization

Define a search space inside `argtuner.toml`:

```toml
[space]
[[space.params]]
name = "lr"
min = 1e-5
max = 1e-2
log_scale = true

[[space.params]]
name = "steps"
min = 50
max = 200
step = 10

Note: `step` is only supported for linear ranges; it cannot be combined with `log_scale = true`.

[[space.params]]
name = "kernel"
values = ["3", "5", "7"]
```

Run particle swarm optimization (PSO) with argmin:

```bash
argtuner run ./argtuner/my-project
```

## Schedulers (budgeted runs)

argtuner can run trials with a fixed budget or use Successive Halving to
allocate more epochs to the most promising configs.

To use epoch budgeting, include a placeholder in your template (default
`{epochs}`) and set `[scheduler] type = "successive_halving"` in `argtuner.toml`:

```text
my-train --lr {lr} --epochs {epochs} --out {trial_dir}
```

Add a `[scheduler.successive_halving]` table to specify the scheduler-specific
settings:

```toml
[scheduler]
type = "successive_halving"

[scheduler.successive_halving]
budget_placeholder = "epochs"
min_epochs = 2
max_epochs = 100
eta = 3
```

`min_epochs`, `max_epochs`, and `eta` decide the per-rung budgets while
`budget_placeholder` tells argtuner which template placeholder to override.

Successive halving relies on CLI placeholders for budgets and resume. Include
`--epochs {epochs}` and `--checkpoint-dir {trial_dir}` in your template so each
promotion continues from the same checkpoint directory instead of starting over.
If your training script uses a different checkpoint flag, set it under `[project]`:

```toml
[project]
checkpoint_arg = "--checkpoint_dir"
```

The command is executed directly (program + args) with stdout/stderr streamed
to your terminal. Any stdout line can emit an ARGTUNER event with the configured
prefix; the tuner parses the last `model.epoch_end` event. For example:

```text
::ARGTUNER::{"type":"event","name":"model.epoch_end","fields":{"metric":"0.123","aux":"42","epoch":"1"}}
```

Each evaluation is recorded in the CSV with all parameter values, the parsed
metric, and the score. The message prefix is fixed to `::ARGTUNER::` (see the `RESULT_PREFIX` constant); you can change `metric_key` in `argtuner.toml`.

## Command flow (tuner ↔ app)

The tuner communicates with your app via two channels:
1) **Template placeholders**  
   - Your template includes `{name}` placeholders (e.g., `{lr}`, `{epochs}`).
   - Each trial renders the template with sampled values from the `[space]`
     section inside `argtuner.toml`.
   - If `inject_trial_placeholders` is true, `{trial_id}` and `{trial_dir}` are
     populated and available in the template.

2) **Environment variables**  
   - The tuner always exports:
     - `ARGTUNER_TRIAL_ID`
     - `ARGTUNER_TRIAL_DIR`
   - Use these if you prefer reading values from the environment instead of
     the command line.

Windows note:
- Command templates are split into program + args without a shell, so use
  explicit arguments and quote paths with spaces. To run shell built-ins, wrap
  them in `cmd /C ...` or `powershell -Command ...`.

If the app exits non‑zero or emits an invalid config payload, the trial is
marked `error` and the tuner may retry based on scheduler policy.

Duplicate configs are treated as invalid. If a newly sampled config matches an
existing trial’s `hp.*` values, the tuner retries with a new sample. If it
cannot find a unique config after a fixed number of retries, the run stops
with an error indicating the search space may be exhausted.

## Hyperparameter impact & CSV behavior

### Hyperparameter impact report

When trials complete, argtuner prints a heuristic Hyperparameter Impact report.
It shows Pearson correlation vs. score for each hyperparameter and adds coarse
range bins with best/median scores, highlighting an estimated elbow where improvements flatten. Metrics and ordering follow the project goal (goal=min: lower is better; goal=max: higher is better), and it includes a small histogram of bin counts for each parameter.

### CSV behavior

- The SQLite database is the source of truth; the CSV is a mirrored snapshot.
  Trial resumes and command reconstruction use the recorded `hp.*` values from
  the database, which are treated as immutable for that trial.
- Columns are grouped as core trial fields, then `metric.*`, then `trial.*`,
  then `hp.*`.
- Core fields are unprefixed for readability/stability:
  `trial_id`, `status`, `elapsed_ms`, `error`, `metric`, `score`.
- Hyperparameters are stored under `hp.` and result payload fields use `metric.`.
- If new keys appear, the CSV is rewritten with a superset header and all
  existing rows preserved.
- The rendered command is not stored; it is reconstructed from the template and
  the recorded `hp.*` values at run time.

## License

`term-wm` is primarily distributed under the terms of both the MIT license and the Apache License (Version 2.0).

See [LICENSE-APACHE](./LICENSE-APACHE) and [LICENSE-MIT](./LICENSE-MIT) for details.

[ci]: https://github.com/jzombie/argtuner/actions
[macos-badge]: https://img.shields.io/badge/macOS-000000?style=flat-square&logo=apple&logoColor=white
[linux-badge]: https://img.shields.io/badge/Linux-FCC624?style=flat-square&logo=linux&logoColor=black
[windows-badge]: https://img.shields.io/badge/Windows-0078D6?style=flat-square&logo=windows&logoColor=white
[rust-src-page]: https://www.rust-lang.org/
[rust-logo]: https://img.shields.io/badge/Made%20with-Rust-orange?style=flat-square
[crates-page]: https://crates.io/crates/argtuner
[crates-badge]: https://img.shields.io/crates/v/argtuner.svg?style=flat-square
[mit-license-page]: ./LICENSE-MIT
[mit-license-badge]: https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square
[apache-2.0-license-page]: ./LICENSE-APACHE
[apache-2.0-license-badge]: https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=flat-square
[codeql-page]: https://github.com/jzombie/rust-argtuner/actions/workflows/github-code-scanning/codeql
[codeql-badge]: https://img.shields.io/github/actions/workflow/status/jzombie/rust-argtuner/github-code-scanning/codeql?style=flat-square
[coveralls-page]: https://coveralls.io/github/jzombie/rust-argtuner?branch=main
[coveralls-badge]: https://coveralls.io/repos/github/jzombie/rust-argtuner/badge.svg?branch=main&style=flat-square

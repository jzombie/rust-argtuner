# argtuner

[![macOS][macos-badge]][ci] [![Linux][linux-badge]][ci] [![Windows][windows-badge]][ci]
<br>
[![Made with Rust][rust-logo]][rust-src-page] [![crates.io][crates-badge]][crates-page] [![MIT licensed][mit-license-badge]][mit-license-page] [![Apache 2.0 licensed][apache-2.0-license-badge]][apache-2.0-license-page] [![Coverage][coveralls-badge]][coveralls-page] [![CodeQL][codeql-badge]][codeql-page]

[`argtuner`](https://crates.io/crates/argtuner) is a language-agnostic CLI tool for black-box hyperparameter optimization. It repeatedly executes a target script or binary while systematically varying its command-line arguments to find the optimal configuration.

By defining a search space in `argtuner.toml` and templating your command (e.g., `--lr {lr}`), `argtuner` orchestrates trials using algorithms like Particle Swarm Optimization (PSO) or Successive Halving. It reads trial metrics directly from the process `stdout` and logs results to a local SQLite/CSV database.

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

## When it fits

Use argtuner when:
- You already have a runnable training command and just need structured, repeatable search.
- You want a simple, project-based workflow that logs trials and lets you re-run exact commands.
- You are okay with black-box tuning (no gradients, no internal hooks) and want it to work across different models/tools.

## CLI subcommands

argtuner ships five subcommands:

- `run` — run an optimization campaign against a project.
- `rebuild-csv` — rebuild `trials.csv` from `trials.sqlite` (for example, after
  manual edits or a corrupt CSV). Tuning runs automatically rebuild this after
  each trial.
- `watch` — live TUI dashboard for monitoring trials and metrics as they run
  (see [Watch](#watch-live-tui) below).
- `find` — recursively locate argtuner projects.
- `plan` — show the scheduler plan for a project (optionally a specific config id).

```bash
argtuner run ./argtuner/my-project
argtuner rebuild-csv ./argtuner/my-project
argtuner plan ./argtuner/my-project
argtuner plan ./argtuner/my-project --config-id 3
```

## Command templates

Use `{placeholder}` tokens for tunable values. Double braces `{{`/`}}` escape
literal braces.

```text
cargo run -p my-app -- --lr {lr} --steps {steps}
```

Reserved placeholders injected automatically:
- `{trial_id}`: the numeric trial id.
- `{trial_dir}`: per-trial artifacts directory, always `argtuner/<project>/artifacts/trial_{trial_id}`.
  Successive-halving promotions copy the parent rung's artifacts into the child
  trial's directory rather than sharing a directory across rungs.

For a realistic, runnable config that ties all of this together — a log-scale
float search, a successive-halving budget placeholder, and a `{trial_dir}`
checkpoint dir — see [`examples/config_showcase/`](https://github.com/jzombie/rust-argtuner/blob/main/examples/config_showcase/README.md)
and its [`argtuner inspect` output](#config-layout-argtunertoml) below.

- Example template with artifacts:
```text
my-train --lr {lr} --steps {steps} --output {trial_dir}
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


## Example: linear regression

There is a runnable, self-contained example in [`examples/linear_regression/`](https://github.com/jzombie/rust-argtuner/blob/main/examples/linear_regression/README.md)
(main.rs + argtuner.toml + README).

Run directly:

```bash
cargo run -p argtuner --example linear_regression -- --lr 0.01 --steps 50
```

Tune it:

```bash
cargo run -p argtuner -- run examples/linear_regression
```

The bundled `argtuner.toml` searches `lr` (log-scale) and `steps` over a small
random budget.

## Example: guitar tuning demo

The [guitar tuning demo](https://github.com/jzombie/rust-argtuner/blob/main/examples/guitar_tuning/README.md) keeps a
simple CLI, its `argtuner.toml`, and the generated `trials.csv` in the same
directory. Each placeholder represents a candidate string frequency (E2 through
E4). The CLI computes the mean absolute detuning, prints helpful diagnostics,
and emits ARGTUNER JSON events (for example `{"type":"event","name":"model.epoch_end","fields":{"mean_abs":"0.123"}}`) that PSO minimizes. The template
includes `--checkpoint-dir {trial_dir}` (the CLI accepts and ignores it) so the
demo also demonstrates the resume-friendly placeholder pattern.

Run the CLI directly:

```bash
cargo run -p argtuner --example guitar_tuning -- \
  --e2 83.0 --a2 109.5 --d3 147.0 --g3 196.0 --b3 246.8 --e4 329.6
```

Or launch the bundled argtuner project (uses PSO with six float parameters):

```bash
cargo run -p argtuner -- run examples/guitar_tuning
```

## Example: interactive probe

The [interactive probe](https://github.com/jzombie/rust-argtuner/blob/main/examples/interactive_probe/README.md) is a REPL that
lets you manually emit ARGTUNER event lines to drive the tuner and validate
logging/UX (e.g. `result 0.42`, `event model.early_stopped`,
`invalid <reason>`).

```bash
cargo run -p argtuner -- run examples/interactive_probe
```

## Example: loss-pattern generator

The [loss-pattern generator](https://github.com/jzombie/rust-argtuner/blob/main/examples/loss_pattern_generator/README.md)
simulates synthetic training runs (smooth decay, overfitting, spikes, noisy)
and emits per-step and per-epoch loss events, so you can explore the tuner's
visualization and `watch` TUI without training a real model.

```bash
cargo run -p argtuner -- run examples/loss_pattern_generator
cargo run -p argtuner -- watch --project examples/loss_pattern_generator
```

## Config layout (`argtuner.toml`)

`argtuner.toml` has a strict set of top-level sections so sampler and scheduler
settings are never interleaved. The bundled
[`examples/config_showcase/`](https://github.com/jzombie/rust-argtuner/blob/main/examples/config_showcase/README.md)
project shows a realistic config — log-scale floats, a successive-halving budget
placeholder, and the resume-friendly `{trial_dir}` checkpoint pattern:

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

To see exactly what argtuner deserializes from that file — the parsed config
structs (with defaults applied), the template, and how every `{placeholder}`
resolves against the search space — run:

```bash
argtuner inspect examples/config_showcase
```

<!-- config.inspect.output -->
```text
project: examples/config_showcase

[project]
metric_key = "val_loss"
goal = "min"
pruner = "none"
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
log_scale = true

[[space.params]]
type = "Int"
name = "steps"
min = 20
max = 300
step = 20

template:
  cargo run -p argtuner --example loss_pattern_generator -- \
      --pattern noisy \
      --steps {steps} \
      --noise {noise} \
      --metric-key val_loss \
      --checkpoint-dir {trial_dir} \
      --epoch-time 1

placeholders:
  {noise}: space param Float in [0.01, 0.5], log-scale
  {steps}: space param Int in [20, 300], step 20; scheduler budget placeholder (overridden per rung)
  {trial_dir}: reserved: per-trial artifact directory (auto-injected)
```

Notes:
- `[project]` holds run-wide metadata (metric parsing, goal, pruner, and trial placeholder injection).
- `[sampler]` names the sampler (`pso` or `random`). Sampler-specific knobs live under `[sampler.<sampler_name>]`, e.g. `pso.iters` and `pso.particles`.
- `[scheduler]` picks `fixed` or `successive_halving`. Scheduler knobs (like `n_trials`, `seed`, and halving budgets) stay inside `[scheduler]` and its child tables.
- `n_trials` now belongs to the scheduler because the `fixed` and `successive_halving` schedulers manage the evaluation budget.
- Scheduler type names always match their child tables: `type = "successive_halving"` pairs with `[scheduler.successive_halving]`, so you never need to memorize separate spellings.
- `[space]` is still a top-level table describing the search space; it intentionally sits alongside the other sections for clarity.
- `argtuner inspect <dir>` parses the project's config and prints the deserialized structs (defaults applied, e.g. `pruner = "none"` above was omitted in the file) plus a placeholder analysis — handy for checking what argtuner actually reads from a config.
- When the `random` sampler hits a duplicate in a fully discrete space (choices/ints/stepped floats), it will try unused configs up to the exhaustive cap before continuing with random sampling.

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

```
::ARGTUNER::{"type":"event","name":"model.epoch_end","fields":{"metric":"0.123","aux":"42","epoch":"1"}}
```

Each evaluation is recorded in the CSV with all parameter values, the parsed
metric, and the score. The message prefix is fixed to `::ARGTUNER::` (see the `RESULT_PREFIX` constant); you can change `metric_key` in `argtuner.toml`.

## Watch (live TUI)

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
`tests/protocol_schema.rs` asserts it is byte-identical to the generated
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
```
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

- `tests/protocol_schema.rs` runs the real parser on this exact feed and
  asserts it produces exactly that message.

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

## Hyperparameter impact report

When trials complete, argtuner prints a heuristic Hyperparameter Impact report.
It shows Pearson correlation vs. score for each hyperparameter and adds coarse
range bins with best/median scores, highlighting an estimated elbow where improvements flatten. Metrics and ordering follow the project goal (goal=min: lower is better; goal=max: higher is better), and it includes a small histogram of bin counts for each parameter.

## CSV behavior

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

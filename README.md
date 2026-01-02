TODO: Consider comparing prior states before determining new state to ensure new configs are always evaluated, or to stop early if search space is exhausted
TODO: Add example showing how to use with [Burn](https://crates.io/crates/burn) and also use this as an integration test.

TODO: Mention in README that `argtuner` expects a stateless environment for command execution. Arguments sent to it should return as close to a deterministic result as possible.

# argtuner

Project-based command-template tuner CLI.

## When it fits

Use argtuner when:
- You already have a runnable training command and just need structured, repeatable search.
- You want a simple, project-based workflow that logs trials and lets you re-run exact commands.
- You are okay with black-box tuning (no gradients, no internal hooks) and want it to work across different models/tools.

## Command templates

Use `{placeholder}` tokens for tunable values. Double braces `{{`/`}}` escape
literal braces.

```text
cargo run -p my-app -- --lr {lr} --steps {steps}
```

Reserved placeholders injected automatically:
- `{trial_id}`: the numeric trial id.
- `{trial_dir}`: per-trial artifacts directory under `argtuner/<project>/artifacts/`.
  When `trial.config_id` is present, argtuner uses `trial_<config_id>_b<bracket>` so
  successive-halving rungs share the same directory.

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

Result fields / event protocol:
- Any stdout line can emit an ARGTUNER message using the fixed `::ARGTUNER::` prefix; the tuner uses the last `model.epoch_end` event for scoring.
- Messages are JSON after the prefix. Supported payloads are:
  - `{"type":"event","name":"model.epoch_end","fields":{...}}` (each event appends a trial row; the last one drives scoring)
  - `{"type":"event","name":"model.early_stopped","fields":{...}}`
- argtuner exports environment variables to the command:
  - `ARGTUNER_TRIAL_ID` / `ARGTUNER_TRIAL_DIR`

For example:
```
::ARGTUNER::{"type":"event","name":"model.epoch_end","fields":{"metric":"0.123","aux":"42","epoch":"1"}}
::ARGTUNER::{"type":"event","name":"model.early_stopped","fields":{}}
```

```bash
 [project]
 metric_key = "metric"
 goal = "min"
 pruner = "none"
 inject_trial_placeholders = true
- `trials.csv`
- `artifacts/`

Show the project config and template:

```bash
argtuner show ./argtuner/my-project
```

Rebuild `trials.csv` from `trials.sqlite` (for example, after manual edits or a corrupt CSV):

```bash
argtuner rebuild-csv ./argtuner/my-project
```

*Note: Tuning runs will automatically rebuild this after each trial.*

Show the scheduler plan for a project (optionally a specific config id):

```bash
argtuner plan ./argtuner/my-project
argtuner plan ./argtuner/my-project --config-id 3
```

## Example: linear regression

There is a runnable example in `crates/tuner/examples/linear_regression.rs`:

```bash
cargo run -p argtuner --example linear_regression -- --lr 0.01 --steps 50
```

You can tune it by creating a project with a template like:

```text
cargo run -p argtuner --example linear_regression -- --lr {lr} --steps {steps}
```

Run optimization:

```bash
argtuner run ./argtuner/my-project
```

Record a running trial:

```bash
argtuner trial ./argtuner/my-project start \
  --trial-id 5 \
  --value lr=0.001 \
  --value steps=100
```

Finish a trial and update the row:

```bash
argtuner trial ./argtuner/my-project finish \
  --trial-id 5 \
  --status ok \
  --elapsed-ms 1234 \
  --value score=0.42
```

Show the rendered command for a trial:

```bash
argtuner trial ./argtuner/my-project command --trial-id 5
```

Show the rendered command with resolved placeholder values:

```bash
argtuner trial ./argtuner/my-project command --trial-id 5 --show-values
```

## Example: guitar tuning demo

The [guitar tuning demo](crates/tuner/examples/guitar_tuning/README.md) keeps a
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

If the app exits non‑zero or emits an invalid config payload, the trial is
marked `error` and the tuner may retry based on scheduler policy.

## Hyperparameter impact report

When trials complete, argtuner prints a heuristic Hyperparameter Impact report.
It shows Pearson correlation vs. score for each hyperparameter and adds coarse
range bins with best/median scores, highlighting an estimated elbow where improvements flatten. Metrics and ordering follow the project goal (goal=min: lower is better; goal=max: higher is better), and it includes a small histogram of bin counts for each parameter.

## Config layout (`argtuner.toml`)

`argtuner.toml` now has a strict set of top-level sections so sampler and
scheduler settings are never interleaved:

```toml
template = "cargo run ..."

[project]
metric_key = "metric"
goal = "min"
pruner = "none"
inject_trial_placeholders = true

[sampler]
type = "pso"

[sampler.pso]
iters = 10
particles = 5

[scheduler]
type = "successive_halving"
n_trials = 50
seed = 42

[scheduler.successive_halving]
budget_placeholder = "epochs"
min_epochs = 1
max_epochs = 10
eta = 3

[space]
[[space.params]]
# ...
```

Notes:
- `[project]` holds run-wide metadata (metric parsing, goal, pruner, and trial placeholder injection).
- `[sampler]` names the sampler (`pso` or `random`). Sampler-specific knobs live under `[sampler.<sampler_name>]`, e.g. `pso.iters` and `pso.particles`.
- `[scheduler]` picks `fixed` or `successive_halving`. Scheduler knobs (like `n_trials`, `seed`, and halving budgets) stay inside `[scheduler]` and its child tables.
- `n_trials` now belongs to the scheduler because the `fixed` and `successive_halving` schedulers manage the evaluation budget.
- Scheduler type names always match their child tables: `type = "successive_halving"` pairs with `[scheduler.successive_halving]`, so you never need to memorize separate spellings.
- `[space]` is still a top-level table describing the search space; it intentionally sits alongside the other sections for clarity.

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
- The rendered command is not stored; use `argtuner trial <project> command --trial-id N`
  to reconstruct it from the template and recorded values.

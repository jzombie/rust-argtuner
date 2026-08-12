# Guitar Tuning Demo

`argtuner` is a black-box hyperparameter optimization CLI; this demo maps that model onto guitar tuning. The six string frequencies (`{e2}`..`{e4}`) are the search-space parameters and the mean absolute frequency error is the metric `argtuner` minimizes with Particle Swarm Optimization (PSO). The target frequencies follow standard tuning (E2–A2–D3–G3–B3–E4). Each trial provides candidate frequencies
via CLI flags, and the example binary emits ARGTUNER JSON events (for example `::ARGTUNER::{"type":"event","name":"model.epoch_end","fields":{"mean_abs":"0.123","epoch":"1"}}`); the mean absolute error is its primary metric.

## Layout

- `main.rs`: CLI that computes the tuning loss and prints helpful diagnostics.
  It accepts `--checkpoint-dir {trial_dir}` to satisfy argtuner’s template
  requirements, but the demo ignores the directory because it runs single-epoch
  trials.
- `argtuner.toml`: Template, project settings (PSO sampler), the scheduler
  (`type = "fixed"`, `n_trials = 60`, `seed = 7`), and the six-parameter search
  space bounds.

## Running the demo

From the repository root:

```bash
cargo run -p argtuner --example guitar_tuning -- \
  --e2 83.0 --a2 109.5 --d3 147.0 --g3 196.1 --b3 247.0 --e4 329.4
```

To let argtuner search for a tuning automatically, point the runner at the
example directory:

```bash
cargo run -p argtuner -- run examples/guitar_tuning
```

Trials write their CSV and artifacts inside this directory, so everything stays
self-contained.

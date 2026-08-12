# Linear Regression Example

[`argtuner`](https://crates.io/crates/argtuner) is a black-box hyperparameter optimization CLI that runs a command repeatedly with varying arguments. This example is the minimal integration: a gradient-descent linear regression that emits a single `model.epoch_end` metric event on stdout for `argtuner` to score.

## Run directly

```bash
cargo run -p argtuner --example linear_regression -- --lr 0.01 --steps 50
```

## Tune it

```bash
cargo run -p argtuner -- run examples/linear_regression
```

The bundled `argtuner.toml` searches `lr` (log-scale) and `steps` over a small
random budget. The example's `--checkpoint-dir {trial_dir}` placeholder is
accepted and ignored (single-epoch run).

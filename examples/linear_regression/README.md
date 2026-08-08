# Linear Regression Example

A minimal gradient-descent linear regression that emits one `model.epoch_end`
event, demonstrating the simplest possible ARGTUNER integration.

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

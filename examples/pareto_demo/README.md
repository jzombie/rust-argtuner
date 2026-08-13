# Multi-objective (Pareto) demo

Shows a two-objective run where `loss` and `latency_ms` trade off against each
other, and the Pareto frontier of non-dominated trials at the end of the run.

The tuned executable is the example binary in this directory
(`examples/pareto_demo/main.rs`, built via
`cargo run -p argtuner --example pareto_demo -- ...`), which computes a
deterministic trade-off: smaller `--batch-size` values lower `loss` but raise
`latency_ms`.

Run the tuning session (from this directory):

```bash
argtuner run .
```

The end-of-run summary prints the non-dominated frontier, e.g.:

```text
Pareto frontier (3 of 6 trials):
Trial 2  loss=0.084503  latency_ms=37.230000
  Hyperparameters:
    batch_size           32
    learning_rate        0.0011713471788705548
...
```

Watch it live in another terminal while a run is active (the `Pareto Frontier`
panel lists non-dominated trials and the Trials table tags them `[nd]`):

```bash
argtuner watch --project .
```

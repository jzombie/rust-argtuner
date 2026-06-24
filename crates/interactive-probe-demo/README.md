# argtuner-interactive-probe-demo

Interactive CLI that emits ARGTUNER event lines so you can manually
drive the tuner and validate logging/UX.

Primary usage (run the bundled probe project):

```bash
cargo run -- run crates/interactive-probe-demo/probe-tuning-project
```

Commands:

- `result`/`r <value> [k=v...]` emits a `model.epoch_end` event with the metric key plus extra fields.
- `result`/`r k=v [k=v...]` emits a `model.epoch_end` event with explicit fields.
- `event`/`e <name> [k=v...]` emits an `EVENT` line with optional fields.
- `invalid`/`i <reason>` emits a `model.invalid_config` event with `error=...`.
- `help`/`h` prints the command list.
- `quit`/`q` exits.

Example session:

```
r 0.42 last_epoch=3
r metric=0.42 aux=1
e model.early_stopped
i bad%20config
```

## Wiring into a tuning loop

1) Create or pick an argtuner project directory (with `argtuner.toml`).

2) Use the probe as the template command so the tuner launches it for each trial:

```toml
[project]
metric_key = "metric"

[sampler]
type = "random"

[scheduler]
type = "fixed"
n_trials = 1

[space]
[[space.params]]
name = "lr"
min = 0.001
max = 0.01
```

Template file (or `template` value in `argtuner.toml`):

```text
cargo run -q -p argtuner-interactive-probe-demo -- --metric-key metric --checkpoint-dir {trial_dir}
```

3) Run the tuner from the repo root:

```bash
cargo run -p argtuner -- run path/to/project
```

4) When the probe starts, type commands in that terminal to emit events
back to the tuner (e.g., `result 0.42`, `event model.early_stopped`).

Notes:
- The tuner consumes the last `model.epoch_end` event it sees. You can emit multiple
  epoch_end events; the final one wins.
- Use `event model.invalid_config` or `invalid <reason>` to force a trial failure.

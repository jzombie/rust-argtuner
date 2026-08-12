# config_showcase

A configuration showcase for the argtuner template system.

Unlike the runnable examples (`linear_regression`, `guitar_tuning`, ...), this
directory is **not** a Cargo example target — there is no `main.rs`. It exists
purely to demonstrate a realistic `argtuner.toml`:

- a multi-argument template command
- log-scale `Float` parameters (`noise`)
- an `Int` budget parameter (`steps`) that successive halving overrides per rung
- the resume-friendly `{trial_dir}` checkpoint placeholder

See the `argtuner inspect` output for this project in the
[main README](../../README.md#config-layout-argtunertoml).

Try it:

```bash
argtuner inspect examples/config_showcase
argtuner run examples/config_showcase
```

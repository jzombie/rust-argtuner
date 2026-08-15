# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/) and this project adheres to
(or is loosely based on) Semantic Versioning.

## Unreleased

### Added

- **Const-path `default`/`min`/`max`/`step` hints:** `#[param(default = crate::DEFAULT_EPOCHS)]` (and `min`/`max`/`step`) now accept arbitrary const expressions, not just literals, so defaults can reference named constants instead of duplicating values. Non-literal numeric/bool defaults are stringified at runtime into a generated per-field `static OnceLock<String>` anchor (`__ARGTUNER_DEFAULT_<struct>_<field>`, mangled with the struct ident to avoid module-scope collisions) that yields a `&'static str` with exactly one bounded allocation per process — **no `.leak()`, no `Cow`**. `TunerParam.default` stays `Option<&'static str>` and `TunerParam` keeps its `Copy` impl.
- **`#[tuner_params]` auto-derives `Debug`, `Clone`, and `serde::Serialize`:** the macro injects `#[derive(Debug, Clone, ::argtuner_sdk::serde::Serialize)]` plus `#[serde(crate = "::argtuner_sdk::serde")]` on the struct (serde resolves through the SDK, so consumers need no direct serde dependency). Consumers must not derive these themselves.
- **`Vec<T>` repeatable flags:** a `Vec<String>` field becomes a repeatable `--flag` (each occurrence appends; `from_matches` collects via `get_many`). The flag is never `required` and is excluded from the template/search space.

### Changed

- **`default` on a `Fixed` `Option<T>` field is rejected at compile time:** clap would always yield `Some(default)`, making the `Option` meaningless; the derive now emits a `syn::Error` telling the user to omit the default or drop the `Option`.
- **`#[tuner_params]` parameters now use an explicit `role` (breaking):** each
  field declares who supplies its value via `#[param(role = ...)]` instead of
  relying on type- and bounds-inference. `role = ParamRole::Fixed` is the
  uniform default for every type (a bare `bool` is no longer silently tunable).
  Roles: `Fixed` (constant baked into the template), `Tune` (sampled from
  `[space]`; requires `min`/`max` or `choices`, or a bool), `Injected` (argtuner
  supplies the value; `value_name` must be `trial_dir`/`trial_id`), and `Cli`
  (operational flag, excluded from template and space). The derive now rejects
  constraint hints on non-`tune` roles, `role = ParamRole::Tune` without the
  bounds its kind requires, numeric bounds on bools, and arbitrary `value_name`s
  at compile time. `Option<T>` fields classify by their inner type
  (`Option<f64>` is a `Float`), and all bools — plain and `Option` — parse
  flag-style. The `skip = true` hint was removed in favor of
  `role = ParamRole::Cli`; old hints produce a compile error pointing at the
  migration. `role` takes an **enum variant path** — the canonical
  `role = ParamRole::Tune`, or a fully-qualified
  `role = argtuner_sdk::ParamRole::Tune` — not a string literal (bare
  `role = tune` still parses as a fallback, but the string form `role = "tune"`
  is rejected with a pointer to the migration).
- **`argtuner_sdk::prelude` — single import surface for training binaries:**
  `use argtuner_sdk::prelude::*;` brings in `tuner_params`, `init`,
  `init_with_args`, `is_tuning_active`, `emit_metrics`, `TunerParams`, `ParamRole`,
  `ParamKind`, `TunerParam`, `IpcChannel`, `MetricsBuilder`, and `EventKind`, so
  `#[param(role = ParamRole::Tune)]` resolves in the IDE with autocompletion and
  hover docs. The SDK/tuner boundary is unchanged: `argtuner` (the CLI crate)
  does **not** depend on `argtuner-sdk`, so training binaries never pull in the
  tuner package.

## [0.1.2-alpha] - 2026-08-13

### Added

- **`argtuner-sdk` — the training-side binding extracted into its own lightweight crate:** the SDK that training programs link (declare parameters, parse argv, emit `::ARGTUNER::` telemetry) moved out of the `argtuner` crate root into `crates/argtuner-sdk`. It depends only on a handful of small, ubiquitous crates (`clap`, `serde`, `serde_json`, `toml_edit`, `argtuner-common`, `argtuner-derive`) — no terminal, database, or process-supervision crates — so ML workloads never compile the CLI/TUI. Every `emit_*` helper no-ops unless the `ARGTUNER_TUNING` environment variable is set, so the same binary stays a clean standalone CLI. (`bindings/rust` → `crates/argtuner-sdk`.)
- **`argtuner-derive` — the `#[talkback_args]` proc-macro crate:** a plain struct becomes both a production `clap` CLI, the argtuner command template, and a real search space. `#[param(...)]` hints cover `min`/`max`, `log`, `step`, `choices`, `skip`, `value_name` (reserved `trial_dir`/`trial_id`), and conditional `parent`/`parent_values`. Re-exported as `argtuner_sdk::talkback_args`. (`bindings/talkback-derive` → `crates/argtuner-derive`.)
- **`argtuner-common` — shared protocol types:** the canonical `TalkbackMessage` wire types, event names, and constants shared by the SDK and the tuner, plus the self-describing talkback protocol JSON Schema.
- **Conditional hyperparameters (`#[param(parent = "...", parent_values = [...])]`):** a parameter is active only when its parent samples an allowed value. `validate_specs` enforces the dependency DAG (parent exists, declared before its child, yields a finite value set, no permanently unreachable children); sampling and rendering omit inactive parameters (no value, no `hp.*` field); and `CommandTemplate::strip_inactive_flags` removes both `--flag {value}` and `--flag={value}` segments from the rendered command. The random sampler's `DiscreteConfigPool` enumerates only parent-valid combinations so duplicate detection stays correct.
- **Multi-objective optimization (Pareto frontier):** declare `[[project.objectives]]` (name / goal / primary) to optimize several objectives at once. A new Pareto engine (`src/sampler/pareto.rs`: `fast_nondominated_sort`, zero-variance-guarded `crowding_distance`, capacity-bounded `ParetoFront`) drives `run_pareto`; per-objective scores persist as `score.<name>` so the frontier is reconstructable on resume/rebuild; `argtuner run` prints the end-of-run frontier table. PSO remains single-objective (configs with objectives reject `Sampler::Pso`).
- **Process-tree hygiene + per-trial timeouts:** piped trials spawn as a process group (`command-group`) and PTY trials rely on portable-pty's `setsid()`; aborting, Ctrl-C, or a timeout kills the whole group (`kill(-pgid, SIGKILL)`) with a reap-synchronized poll loop so a recycled PGID is never signaled. New `trial_timeout_s` on `[scheduler]` marks timed-out trials `error` ("timed out after Ns").
- **Cross-platform self-invoking subprocess test harness:** `argtuner::test_support::self_invoking_command` re-executes the test binary (libtest `--exact` filter + role env vars) instead of `sh`/`sleep`, and `assert_no_longer_running` proves group-kill via a frozen heartbeat file — so the process-hygiene tests run on Windows CI too.
- **Watch TUI:** `argtuner watch` gained a full dashboard — seven panes (Trials, Charts, Trial Details, Hyperparameters, Metrics, Project Info, Pareto Frontier), live `model.step_end` streaming via `StepSubscriber`, focused/zoomed metric curves, a hyperparameter-space mode, drag-select-copy in the text panes, and adaptive "Pareto Frontier"/"Best Trials" titles with a live run-status header.
- **`inspect` and `find` subcommands:** `argtuner inspect <project>` renders a project/space inspection (including conditional-parameter dependency lines), and `argtuner find [DIR]` recursively locates argtuner projects.
- **Self-describing talkback protocol:** the protocol JSON Schema is generated from `argtuner_common::TalkbackMessage`, committed at `crates/common/assets/protocol.schema.json`, printed via `--print-protocol-schema`, and asserted byte-identical by `tests/readme_assertions.rs`. A `tuner.binding_version` handshake gate rejects trials whose SDK version mismatches the CLI.
- **TOML-based project config:** the hand-rolled config parser was replaced with `toml`/`toml_edit`, including escaped command strings and `--print-template-toml` starter generation.
- **Bool parameters:** `bool` fields are tunable `Bool` `[space]` entries (flag-style `--use-dropout` / `--use-dropout false`), and `#[param(skip = true)]` keeps an operational flag off the search space while retaining the CLI argument.
- **Examples:** `examples/pareto_demo` (multi-objective run emitting real per-epoch training curves for the TUI charts) and `examples/config_showcase` (reference project feeding the README self-assertions).

### Changed

- **`argtuner` no longer re-exports the SDK (clean break):** the CLI crate is now just the tuner; `argtuner::init` / `argtuner::emit_*` users migrate to `argtuner-sdk`. The deprecated `argtuner-talkback` / `argtuner-talkback-derive` crates now re-export from `argtuner-sdk`.
- **`#[talkback_args]` expansion targets `argtuner_sdk`:** the derive now generates `::argtuner_sdk::` paths so SDK-only consumers compile without the CLI crate.
- **CLI restructured into `src/cli/`:** the binary is a thin shim; `src/cli/mod.rs` dispatches subcommands and `src/cli/tui/mod.rs` hosts the Watch TUI (moved from `src/tui/`).
- **term-wm bumped to 0.9.25-alpha:** the Watch TUI uses the `impl_view_component!` `child:` delegation form — the component lifecycle forwards to a per-frame `view()`, while `desired_height`/selection/hitbox delegate to the window's child field — so the copy-on-release pipeline reads selection off the focused window root.
- **Removed `examples/guitar_tuning`** and the old `bindings/` tree in favor of the `crates/` layout.
- **README overhauled** to cover the SDK, conditional parameters, multi-objective runs, and the Watch TUI.

### Fixed

- **Watch TUI charts showed a single dot per trial:** the demo emitted one `model.epoch_end` per trial, so each trial contributed a single point; examples now emit a real per-epoch training curve so every trial renders a visible segment in the charts.
- **Single-objective runs mislabeled every trial `[nd]`:** rank-0 of a 1-D vector tagged each trial as non-dominated; `[nd]` tags now apply only to multi-objective runs, and the frontier panel adapts its title ("Pareto Frontier" vs "Best Trials").
- **Run status could stay "in progress" after a run finished:** the header now uses activity hysteresis (10s of silence flips to "Run complete — final results") instead of socket presence, so a finished campaign reports final results rather than live values.

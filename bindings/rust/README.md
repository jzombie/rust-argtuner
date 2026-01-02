# argtuner-talkback

Tiny Rust bindings for emitting ARGTUNER JSON event lines. This crate is
the canonical reference for the protocol and is intentionally small so that
other language bindings can mirror it easily.

Core API:

- `emit_event(EventKind::EarlyStopped, &fields)` (or `&my_struct`)
- `emit_epoch_end(&my_struct)`
- `emit_result(&fields)` (or `&my_struct`)
- `emit_version_event()` (emits a JSON event with name `tuner.binding_version`)
- `args_map()` (parsed map of CLI flags to values)
- `parse_args::<T>()` (deserialize args into your struct)
- `maybe_print_template_and_exit::<T>()` (prints a template when `--print-template` or TOML for `--print-template-toml`)
- `init_with_args::<T>()` (print template if requested, emit version, parse clap args)
- `render_template_command::<T>()` / `render_template_toml::<T>()` (clap-based templates)

Convenience wrapper:

- `let tb = Talkback::init();` emits the version event and captures
  `tb.args_map()` from the raw CLI argv.
  `tb.parse_args::<MyArgs>()` deserializes into your own struct.
  `tb.emit_result(&my_struct)` emits a result payload.

Derive helper:

- `argtuner-talkback-derive` provides `#[talkback_args]` to inject
  `--print-template` and `--print-template-toml` into a clap `Parser` struct.
  Put it above `#[derive(Parser)]`.

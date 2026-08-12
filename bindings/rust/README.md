# argtuner-talkback

`argtuner` is a black-box hyperparameter optimization CLI that reads trial metrics from a command's stdout. `argtuner-talkback` is the client-side crate your training script uses to emit those protocol messages (the "talkback" lines). It is the canonical reference for the protocol and is intentionally small so that other language bindings can mirror it easily.

The protocol is self-describing: the wire shapes are defined once in
`argtuner-common` (`argtuner_common::TalkbackMessage`) and a JSON Schema is
generated from that single type, so the schema can never drift from what the
emitter writes or the tuner parses. The canonical schema is committed at
[`crates/common/assets/protocol.schema.json`](https://github.com/jzombie/rust-argtuner/blob/main/crates/common/assets/protocol.schema.json).

Core API:

- `emit_event(kind, &payload)` - The unified emitter.
  - `EventKind::Result`: Emits a result payload.
  - `EventKind::EpochEnd`: Emits an epoch end event.
  - `EventKind::EarlyStopped`: Emits an early stop event.
- `args_map()` (parsed map of CLI flags to values)
- `parse_args::<T>()` (deserialize args into your struct)
- `maybe_print_protocol_schema_and_exit()` (prints the protocol JSON Schema
  when `--print-protocol-schema` is passed, then exits)
- `print_protocol_schema()` (prints the protocol JSON Schema)
- `maybe_print_template_and_exit::<T>()` (prints a template when `--print-template` or TOML for `--print-template-toml`)
- `init_with_args::<T>()` (print schema/template if requested, emit version, parse clap args)
- `render_template_command::<T>()` / `render_template_toml::<T>()` (clap-based templates)

Convenience wrapper:

- `let tb = Talkback::init();` emits the version event and captures
  `tb.args_map()` from the raw CLI argv.
  `tb.parse_args::<MyArgs>()` deserializes into your own struct.
  `tb.emit_event(EventKind::Result, &my_struct)` emits a result payload.

Derive helper:

- `argtuner-talkback-derive` provides `#[talkback_args]` to inject
  `--print-template`, `--print-template-toml`, and `--print-protocol-schema`
  into a clap `Parser` struct. Put it above `#[derive(Parser)]`.

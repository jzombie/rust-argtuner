//! Mock subprocess for the binding-version handshake.
//!
//! Emits `tuner.binding_version` (default `0.0.0`, override with `--version`)
//! plus a `model.epoch_end`, so tests can exercise the tuner's version gate
//! (see `tests/binding_version_mismatch.rs`).

use std::collections::BTreeMap;

#[derive(serde::Serialize)]
struct EvaluationResult {
    value: String,
    epoch: usize,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut version = "0.0.0".to_string();
    let mut result_value = "1.0".to_string();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => {
                if let Some(val) = args.next() {
                    version = val;
                }
            }
            "--metric" => {
                if let Some(val) = args.next() {
                    result_value = val;
                }
            }
            "--checkpoint-dir" => {
                let _ = args.next();
            }
            _ => {}
        }
    }
    let mut fields = BTreeMap::new();
    fields.insert(argtuner_common::BINDING_VERSION_FIELD.to_string(), version);
    let _ = argtuner::emit_event(argtuner_common::EventKind::TunerApiVersion, &fields);
    let _ = argtuner::emit_epoch_end(&EvaluationResult {
        value: result_value,
        epoch: 1,
    });
}

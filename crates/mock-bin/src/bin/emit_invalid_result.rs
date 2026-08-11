//! Mock subprocess that reports an invalid sampled configuration.
//!
//! Emits `model.invalid_config` with `error` set, so tests can exercise the
//! tuner's invalid-config handling and the scheduler retry path.

use std::collections::BTreeMap;

fn main() {
    let _ = argtuner_talkback::emit_event(
        argtuner_common::EventKind::InvalidConfig,
        &BTreeMap::from([("error".to_string(), "bad%20config".to_string())]),
    );

    // Keep the PTY slave open so the parent drains the buffered lines.
    argtuner_talkback::hold_stdout_open();
}

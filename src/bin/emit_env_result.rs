//! Mock subprocess that verifies argtuner's environment-variable injection.
//!
//! Reads `ARGTUNER_TRIAL_ID` and `ARGTUNER_TRIAL_DIR` and echoes them back as
//! `trial_id_env` / `trial_dir_env` inside a `model.epoch_end` event, so tests
//! can assert the tuner exported the expected values.

use std::collections::BTreeMap;

fn main() {
    let trial_id = std::env::var("ARGTUNER_TRIAL_ID").unwrap_or_else(|_| "missing".to_string());
    let trial_dir = std::env::var("ARGTUNER_TRIAL_DIR").unwrap_or_else(|_| "missing".to_string());
    // Emit all fields in a single epoch_end event.
    let mut fields = BTreeMap::new();
    fields.insert("metric".to_string(), "0.5".to_string());
    fields.insert("trial_id_env".to_string(), trial_id);
    fields.insert("trial_dir_env".to_string(), trial_dir);
    let _ = argtuner_talkback::emit_event(argtuner_common::EventKind::EpochEnd, &fields);

    // Keep the PTY slave open so the parent drains the buffered lines.
    argtuner_talkback::hold_stdout_open();
}

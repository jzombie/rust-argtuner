//! Mock "successful trial" subprocess for the test harness.
//!
//! Emits a `model.epoch_end` event (`metric` = 0.42, `last_epoch` = 7) followed
//! by `model.early_stopped` — a deterministic, successful evaluation. Used as
//! the template command by most end-to-end tests (trial flow, tuner resume,
//! checkpointing, PSO, dry run, duplicate configs, artifact preservation).

use std::collections::BTreeMap;

fn main() {
    // Emit multiple fields in a single epoch_end event.
    // Early-stop is signalled via a separate event.
    let mut fields = BTreeMap::new();
    fields.insert("metric".to_string(), "0.42".to_string());
    fields.insert("last_epoch".to_string(), "7".to_string());
    let _ = argtuner_talkback::emit_event(argtuner_common::EventKind::EpochEnd, &fields);
    let _ = argtuner_talkback::emit_event(
        argtuner_common::EventKind::EarlyStopped,
        &BTreeMap::<String, String>::new(),
    );

    // Keep the PTY slave open so the parent drains the buffered lines.
    argtuner_talkback::hold_stdout_open();
}

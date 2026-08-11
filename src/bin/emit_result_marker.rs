//! Mock subprocess that records it ran by appending to a marker file.
//!
//! Emits a successful `model.epoch_end` + `model.early_stopped` and, when given
//! `--marker <path>`, appends a line to that file so a test can detect that the
//! command was actually invoked.
//!
//! Currently not referenced by the test suite (manual/debugging helper).

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;

#[derive(serde::Serialize)]
struct EvaluationResult {
    value: f64,
    epoch: usize,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut marker: Option<String> = None;
    while let Some(arg) = args.next() {
        if arg == "--marker" {
            marker = args.next();
        }
    }

    if let Some(path) = marker
        && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
    {
        let _ = writeln!(file, "1");
    }

    let _ = argtuner_talkback::emit_epoch_end(&EvaluationResult {
        value: 0.42,
        epoch: 1,
    });
    let _ = argtuner_talkback::emit_event(
        argtuner_common::EventKind::EarlyStopped,
        &BTreeMap::<String, String>::new(),
    );

    // Keep the PTY slave open so the parent drains the buffered lines.
    argtuner_talkback::hold_stdout_open();
}

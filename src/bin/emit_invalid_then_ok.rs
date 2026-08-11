//! Mock subprocess whose behavior changes across invocations via a marker file.
//!
//! On the first run (no marker file) it emits `model.invalid_config`; on later
//! runs it emits a successful `model.epoch_end`. This lets tests exercise the
//! retry behavior across repeated invocations.
//!
//! Currently not referenced by the test suite (manual/debugging helper).

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Write};

#[derive(serde::Serialize)]
struct EvaluationResult {
    score: f64,
    epoch: usize,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut marker: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--marker" => marker = args.next(),
            "--checkpoint-dir" | "--epochs" => {
                let _ = args.next();
            }
            _ => {}
        }
    }

    let mut prior = 0usize;
    if let Some(path) = marker.as_ref() {
        let path = std::path::Path::new(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::File::open(path) {
            let mut contents = String::new();
            let _ = file.read_to_string(&mut contents);
            prior = contents.lines().count();
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "1");
        }
    }

    if prior == 0 {
        let _ = argtuner_talkback::emit_event(
            argtuner_common::EventKind::InvalidConfig,
            &BTreeMap::from([("error".to_string(), "bad%20config".to_string())]),
        );
    } else {
        let _ = argtuner_talkback::emit_epoch_end(&EvaluationResult {
            score: 0.42,
            epoch: 1,
        });
    }

    // Keep the PTY slave open so the parent drains the buffered lines.
    argtuner_talkback::hold_stdout_open();
}

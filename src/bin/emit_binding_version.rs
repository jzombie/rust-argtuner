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
    let _ = argtuner_talkback::emit_event(argtuner_common::EventKind::TunerApiVersion, &fields);
    let _ = argtuner_talkback::emit_epoch_end(&EvaluationResult {
        value: result_value,
        epoch: 1,
    });

    // Keep the PTY slave open long enough that the parent's master reader drains
    // the buffered ::ARGTUNER:: lines before this process exits (macOS PTY race).
    // Flush first, then hold; 50 ms to tolerate scheduling jitter under load.
    std::io::Write::flush(&mut std::io::stdout()).ok();
    std::thread::sleep(std::time::Duration::from_millis(50));
}

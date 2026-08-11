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

    // Keep the PTY slave open long enough that the parent's master reader drains
    // the buffered ::ARGTUNER:: lines before this process exits (macOS PTY race).
    // Flush first, then hold; 50 ms to tolerate scheduling jitter under load.
    std::io::Write::flush(&mut std::io::stdout()).ok();
    std::thread::sleep(std::time::Duration::from_millis(50));
}

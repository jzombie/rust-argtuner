use std::collections::BTreeMap;

fn main() {
    let _ = argtuner_talkback::emit_event(
        argtuner_common::EventKind::InvalidConfig,
        &BTreeMap::from([("error".to_string(), "bad%20config".to_string())]),
    );

    // Keep the PTY slave open long enough that the parent's master reader drains
    // the buffered ::ARGTUNER:: lines before this process exits (macOS PTY race).
    // Flush first, then hold; 50 ms to tolerate scheduling jitter under load.
    std::io::Write::flush(&mut std::io::stdout()).ok();
    std::thread::sleep(std::time::Duration::from_millis(50));
}

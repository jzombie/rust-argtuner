use std::collections::BTreeMap;

fn main() {
    let _ = argtuner_talkback::emit_event(
        argtuner_common::EventKind::InvalidConfig,
        &BTreeMap::from([("error".to_string(), "bad%20config".to_string())]),
    );

    // Keep the PTY slave open so the parent drains the buffered lines.
    argtuner_talkback::hold_stdout_open();
}

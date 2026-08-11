use std::collections::BTreeMap;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut x_value = None;
    while let Some(arg) = args.next() {
        if arg == "--x" {
            x_value = args.next();
        } else if x_value.is_none() {
            x_value = Some(arg);
        }
    }
    let x_value = x_value.unwrap_or_else(|| "missing".to_string());

    let mut fields = BTreeMap::new();
    fields.insert("metric".to_string(), "0.0".to_string());
    fields.insert("x_used".to_string(), x_value);
    fields.insert("epoch".to_string(), "1".to_string());
    let _ = argtuner_talkback::emit_event(argtuner_common::EventKind::EpochEnd, &fields);

    // Keep the PTY slave open so the parent drains the buffered lines.
    argtuner_talkback::hold_stdout_open();
}

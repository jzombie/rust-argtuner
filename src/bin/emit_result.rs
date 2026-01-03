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
}

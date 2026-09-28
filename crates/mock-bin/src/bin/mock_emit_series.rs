//! Mock trial emitting multiple epochs and steps for live-streaming tests.
//!
//! Emits 3 `model.epoch_end` events (metric 0.5/0.3/0.1) with 4 interleaved
//! `model.step_end` events. Deterministic; used to assert live row recording,
//! step thinning, and no post-exit double-recording.

use std::collections::BTreeMap;

fn epoch_fields(epoch: &str, metric: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    fields.insert("metric".to_string(), metric.to_string());
    fields.insert("epoch".to_string(), epoch.to_string());
    fields
}

fn step_fields(step: &str, loss: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    fields.insert("step".to_string(), step.to_string());
    fields.insert("loss".to_string(), loss.to_string());
    fields
}

fn main() {
    use argtuner_common::EventKind;
    let _ = argtuner_sdk::emit_event(EventKind::StepEnd, &step_fields("1", "0.9"));
    let _ = argtuner_sdk::emit_event(EventKind::StepEnd, &step_fields("2", "0.8"));
    let _ = argtuner_sdk::emit_event(EventKind::EpochEnd, &epoch_fields("1", "0.5"));
    let _ = argtuner_sdk::emit_event(EventKind::StepEnd, &step_fields("3", "0.4"));
    let _ = argtuner_sdk::emit_event(EventKind::EpochEnd, &epoch_fields("2", "0.3"));
    let _ = argtuner_sdk::emit_event(EventKind::StepEnd, &step_fields("4", "0.2"));
    let _ = argtuner_sdk::emit_event(EventKind::EpochEnd, &epoch_fields("3", "0.1"));
}

//! Mock "multi-objective trial" subprocess for the test harness.
//!
//! Emits two deterministic objectives derived from the injected trial id:
//! - `loss`:  id + 1         (increases with trial id)
//! - `latency_ms`: 3, 1, then 4 for ids 0, 1, 2+   (so trial 2 is dominated
//!   by trial 0 in the (loss, latency) plane).
//!
//! With n_trials = 3 the non-dominated front is exactly {trial 0, trial 1}.

use std::collections::BTreeMap;

fn main() {
    let trial_id: i64 = std::env::var(argtuner::ENV_TRIAL_ID)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let loss = (trial_id + 1) as f64;
    let latency_ms = match trial_id {
        0 => 3.0,
        1 => 1.0,
        _ => 4.0,
    };
    let mut fields = BTreeMap::new();
    fields.insert("loss".to_string(), loss.to_string());
    fields.insert("latency_ms".to_string(), latency_ms.to_string());
    let _ = argtuner::emit_event(argtuner_common::EventKind::EpochEnd, &fields);
}

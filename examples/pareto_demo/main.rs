use std::collections::BTreeMap;

use argtuner_sdk::talkback_args;

#[talkback_args]
struct ParetoArgs {
    /// Learning rate
    #[param(default = 0.01, min = 0.001, max = 0.1, log = true)]
    learning_rate: f64,
    /// Batch size
    #[param(choices = ["16", "32", "64"])]
    batch_size: String,
    /// Checkpoint directory (reserved: trial_dir)
    #[param(value_name = "trial_dir")]
    checkpoint_dir: Option<String>,
}

fn main() {
    let (_talkback, args) = argtuner_sdk::init::<ParetoArgs>();
    let _ = args.checkpoint_dir;

    let batch_size: f64 = args.batch_size.parse().unwrap_or(32.0);
    // Simulate conflicting objectives: smaller batches lower loss but raise
    // per-trial latency (larger batches are cheaper to serve but train worse).
    let final_loss = 0.05 + batch_size * 0.001 + args.learning_rate * 0.5;
    let latency_ms = 1200.0 / batch_size;

    // Simulate a short training run so each trial renders a real curve in the
    // Watch TUI charts instead of a single point: loss converges over epochs
    // toward the trade-off value while latency stays flat.
    let epochs = 10;
    for epoch in 0..epochs {
        let progress = epoch as f64 / (epochs - 1) as f64;
        let loss = final_loss + (1.0 - progress) * 0.15;
        let mut fields = BTreeMap::new();
        fields.insert("loss".to_string(), format!("{loss:.6}"));
        fields.insert("latency_ms".to_string(), format!("{latency_ms:.2}"));
        fields.insert("epoch".to_string(), epoch.to_string());
        let _ = argtuner_sdk::emit_epoch_end(&fields);
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
}

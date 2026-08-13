use argtuner_derive::talkback_args;

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
    let (_talkback, args) = argtuner::init::<ParetoArgs>();
    let _ = args.checkpoint_dir;

    let batch_size: f64 = args.batch_size.parse().unwrap_or(32.0);
    // Simulate conflicting objectives: smaller batches lower loss but raise
    // per-trial latency (larger batches are cheaper to serve but train worse).
    let loss = 0.05 + batch_size * 0.001 + args.learning_rate * 0.5;
    let latency_ms = 1200.0 / batch_size;

    // Give a live `argtuner watch` session a moment to render each trial.
    std::thread::sleep(std::time::Duration::from_millis(500));
    let _ = argtuner::emit_metrics! { "loss" => loss, "latency_ms" => latency_ms };
}

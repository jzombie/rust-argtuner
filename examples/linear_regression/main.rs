use argtuner_sdk::talkback_args;

#[talkback_args]
struct ExampleArgs {
    /// Learning rate for gradient descent
    #[param(default = 0.01, min = 0.001, max = 0.1, log = true)]
    lr: f64,
    /// Number of gradient steps
    #[param(default = 100, min = 5, max = 200)]
    steps: usize,
    /// Checkpoint directory (reserved: trial_dir)
    #[param(value_name = "trial_dir")]
    checkpoint_dir: Option<String>,
}

fn main() {
    let (_talkback, args) = argtuner_sdk::init::<ExampleArgs>();
    let _ = args.checkpoint_dir;

    let data = (0..10)
        .map(|x| {
            let x = x as f64;
            let y = 3.0 * x + 1.0;
            (x, y)
        })
        .collect::<Vec<_>>();
    let mut weight = 0.0_f64;
    let mut bias = 0.0_f64;
    for _ in 0..args.steps {
        let mut grad_w = 0.0_f64;
        let mut grad_b = 0.0_f64;
        for (x, y) in &data {
            let pred = weight * x + bias;
            let err = pred - y;
            grad_w += err * x;
            grad_b += err;
        }
        let n = data.len() as f64;
        grad_w /= n;
        grad_b /= n;
        weight -= args.lr * grad_w;
        bias -= args.lr * grad_b;
    }
    let mut mse = 0.0_f64;
    for (x, y) in &data {
        let pred = weight * x + bias;
        let err = pred - y;
        mse += err * err;
    }
    mse /= data.len() as f64;
    let _ = argtuner_sdk::emit_metrics! { "loss" => mse, "epoch" => args.steps };
}

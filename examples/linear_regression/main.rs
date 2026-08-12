use argtuner_talkback_derive::talkback_args;
use clap::Parser;

#[derive(serde::Serialize)]
struct LinearRegressionResult {
    loss: f64,
    epoch: usize,
}

#[talkback_args]
#[derive(Debug, Parser)]
struct ExampleArgs {
    #[arg(long, value_name = "LR")]
    lr: Option<f64>,
    #[arg(long, value_name = "STEPS")]
    steps: Option<usize>,
    #[arg(long, value_name = "PATH")]
    checkpoint_dir: Option<String>,
}

fn main() {
    let (_talkback, args) = argtuner_talkback::init_with_args::<ExampleArgs>();
    let lr = args.lr.unwrap_or(0.01_f64);
    let steps = args.steps.unwrap_or(100_usize);
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
    for _ in 0..steps {
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
        weight -= lr * grad_w;
        bias -= lr * grad_b;
    }
    let mut mse = 0.0_f64;
    for (x, y) in &data {
        let pred = weight * x + bias;
        let err = pred - y;
        mse += err * err;
    }
    mse /= data.len() as f64;
    let _ = argtuner_talkback::emit_epoch_end(&LinearRegressionResult {
        loss: mse,
        epoch: steps,
    });
}

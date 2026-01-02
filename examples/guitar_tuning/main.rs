use argtuner_talkback_derive::talkback_args;
use clap::Parser;

#[derive(serde::Serialize)]
struct TuningResult {
    mean_abs: String,
    max_abs_error: String,
    rmse: String,
    epoch: usize,
}

#[talkback_args]
#[derive(Debug, Parser)]
struct ProbeArgs {
    #[arg(long, value_name = "HZ")]
    e2: Option<f64>,
    #[arg(long, value_name = "HZ")]
    a2: Option<f64>,
    #[arg(long, value_name = "HZ")]
    d3: Option<f64>,
    #[arg(long, value_name = "HZ")]
    g3: Option<f64>,
    #[arg(long, value_name = "HZ")]
    b3: Option<f64>,
    #[arg(long, value_name = "HZ")]
    e4: Option<f64>,
    #[arg(long, value_name = "PATH")]
    checkpoint_dir: Option<String>,
}

const TARGET_STRINGS: [(&str, f64); 6] = [
    ("e2", 82.41),
    ("a2", 110.0),
    ("d3", 146.83),
    ("g3", 196.0),
    ("b3", 246.94),
    ("e4", 329.63),
];

fn main() {
    let (_talkback, args) = argtuner_talkback::init_with_args::<ProbeArgs>();
    let _ = args.checkpoint_dir;
    let mut freqs = [
        TARGET_STRINGS[0].1,
        TARGET_STRINGS[1].1,
        TARGET_STRINGS[2].1,
        TARGET_STRINGS[3].1,
        TARGET_STRINGS[4].1,
        TARGET_STRINGS[5].1,
    ];
    if let Some(val) = args.e2 {
        freqs[0] = val;
    }
    if let Some(val) = args.a2 {
        freqs[1] = val;
    }
    if let Some(val) = args.d3 {
        freqs[2] = val;
    }
    if let Some(val) = args.g3 {
        freqs[3] = val;
    }
    if let Some(val) = args.b3 {
        freqs[4] = val;
    }
    if let Some(val) = args.e4 {
        freqs[5] = val;
    }

    let mut sum_abs = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    let mut max_abs = 0.0_f64;

    println!("Target vs. trial frequencies (Hz):");
    for (idx, (name, target)) in TARGET_STRINGS.iter().enumerate() {
        let actual = freqs[idx];
        let diff = (actual - *target).abs();
        sum_abs += diff;
        sum_sq += diff * diff;
        max_abs = max_abs.max(diff);
        println!(
            "  {name:<3}: target={target:>7.2} trial={actual:>7.2} |diff|={diff:>6.3}",
            name = name.to_uppercase()
        );
    }

    let count = TARGET_STRINGS.len() as f64;
    let mean_abs = sum_abs / count;
    let rmse = (sum_sq / count).sqrt();

    let _ = argtuner_talkback::emit_epoch_end(&TuningResult {
        mean_abs: format!("{:.6}", mean_abs),
        max_abs_error: format!("{:.6}", max_abs),
        rmse: format!("{:.6}", rmse),
        epoch: 1,
    });
}

mod patterns;

use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use patterns::{LossPattern, SmoothDecay, Overfitting, Underfitting, Spikes, Noisy};
use argtuner_talkback_derive::talkback_args;
use std::time::Duration;
use std::thread;

#[talkback_args]
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The pattern to generate
    #[arg(short, long, value_enum, default_value_t = PatternType::Smooth)]
    pattern: PatternType,

    /// Number of steps to generate
    #[arg(short, long, default_value_t = 100)]
    steps: usize,

    /// Noise level (for noisy patterns)
    #[arg(long, default_value_t = 0.1)]
    noise: f64,

    /// Spike probability (for spike patterns)
    #[arg(long, default_value_t = 0.05)]
    spike_prob: f64,

    /// Metric key name
    #[arg(long, default_value = "loss")]
    metric_key: String,

    /// Checkpoint directory
    #[arg(long)]
    checkpoint_dir: Option<String>,

    /// Simulated epoch time in milliseconds
    #[arg(long, default_value_t = 0.0)]
    epoch_time: f64,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum PatternType {
    Smooth,
    Overfitting,
    Underfitting,
    Spikes,
    NoisySmooth,
}

fn main() {
    let (talkback, args) = argtuner_talkback::init_with_args::<Args>();

    let mut pattern: Box<dyn LossPattern> = match args.pattern {
        PatternType::Smooth => Box::new(SmoothDecay::new(2.0, 0.1, 3.0)),
        PatternType::Overfitting => Box::new(Overfitting::new(2.0, 0.1, args.steps / 2, 20.0)),
        PatternType::Underfitting => Box::new(Underfitting::new(1.5, args.noise)),
        PatternType::Spikes => Box::new(Spikes::new(
            Box::new(SmoothDecay::new(2.0, 0.1, 3.0)),
            args.spike_prob,
            1.0,
        )),
        PatternType::NoisySmooth => Box::new(Noisy::new(
            Box::new(SmoothDecay::new(2.0, 0.1, 3.0)),
            args.noise,
        )),
    };

    eprintln!("Generating pattern: {}", pattern.name());
    println!("step,loss");

    let pb = if args.epoch_time > 0.0 {
        let pb = ProgressBar::new(args.steps as u64);
        pb.set_style(ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})")
            .unwrap()
            .progress_chars("#>-"));
        Some(pb)
    } else {
        None
    };

    let mut final_loss = 0.0;
    for step in 0..args.steps {
        if let Some(pb) = &pb {
            pb.set_position(step as u64);
            thread::sleep(Duration::from_millis(args.epoch_time as u64));
        }
        let loss = pattern.generate(step, args.steps);
        println!("{},{:.6}", step, loss);
        final_loss = loss;
    }

    if let Some(pb) = &pb {
        pb.finish_with_message("done");
    }

    let mut result = std::collections::BTreeMap::new();
    result.insert(args.metric_key, final_loss);
    talkback.emit_result(&result).expect("Failed to emit result");
}

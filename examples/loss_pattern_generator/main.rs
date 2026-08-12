mod patterns;

use argtuner_talkback_derive::talkback_args;
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use patterns::{LossPattern, Noisy, Overfitting, SmoothDecay, Spikes, Underfitting};
use std::thread;
use std::time::Duration;

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

    /// Metric key name (defaults to val_loss)
    #[arg(long, default_value = "val_loss")]
    metric_key: String,

    /// Checkpoint directory
    #[arg(long)]
    checkpoint_dir: Option<String>,

    /// Simulated epoch time in milliseconds
    #[arg(long, default_value_t = 0.0)]
    epoch_time: f64,
}

#[derive(serde::Serialize)]
struct LossStep {
    loss: f64,
    train_loss: f64,
    val_loss: f64,
    epoch: usize,
    pattern: String,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum PatternType {
    Smooth,
    Overfitting,
    Underfitting,
    Spikes,
    NoisySmooth,
}

fn pattern_label(pattern: PatternType) -> &'static str {
    match pattern {
        PatternType::Smooth => "smooth",
        PatternType::Overfitting => "overfitting",
        PatternType::Underfitting => "underfitting",
        PatternType::Spikes => "spikes",
        PatternType::NoisySmooth => "noisy-smooth",
    }
}

fn main() {
    let (talkback, args) = argtuner_talkback::init::<Args>();
    let pattern_name = pattern_label(args.pattern).to_string();

    let mut train_pattern: Box<dyn LossPattern> = match args.pattern {
        PatternType::Smooth => Box::new(SmoothDecay::new(2.0, 0.1, 3.0)),
        PatternType::Overfitting => Box::new(SmoothDecay::new(2.0, 0.1, 3.0)),
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

    let mut val_pattern: Box<dyn LossPattern> = match args.pattern {
        PatternType::Smooth => Box::new(SmoothDecay::new(2.1, 0.15, 3.0)),
        PatternType::Overfitting => Box::new(Overfitting::new(2.0, 0.1, args.steps / 2, 20.0)),
        PatternType::Underfitting => Box::new(Underfitting::new(1.7, args.noise * 1.2)),
        PatternType::Spikes => Box::new(Spikes::new(
            Box::new(SmoothDecay::new(2.0, 0.12, 3.0)),
            (args.spike_prob * 1.5).min(1.0),
            1.2,
        )),
        PatternType::NoisySmooth => Box::new(Noisy::new(
            Box::new(SmoothDecay::new(2.0, 0.12, 3.0)),
            args.noise * 1.5,
        )),
    };

    eprintln!("Generating pattern: {}", val_pattern.name());
    println!("step,loss,val_loss");

    let pb = if args.epoch_time > 0.0 {
        let pb = ProgressBar::new(args.steps as u64);
        pb.set_style(
            ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
                )
                .unwrap()
                .progress_chars("#>-"),
        );
        Some(pb)
    } else {
        None
    };

    let mut final_train_loss = 0.0;
    let mut final_val_loss = 0.0;
    let epoch_every = 10;
    for step in 0..args.steps {
        if let Some(pb) = &pb {
            pb.set_position(step as u64);
            thread::sleep(Duration::from_millis(args.epoch_time as u64));
        }
        let train_loss = train_pattern.generate(step, args.steps);
        let val_loss = val_pattern.generate(step, args.steps);
        println!("{},{:.6},{:.6}", step, train_loss, val_loss);
        let _ = talkback.emit_step_end(&LossStep {
            loss: train_loss,
            train_loss,
            val_loss,
            epoch: step + 1,
            pattern: pattern_name.clone(),
        });
        // Emit epoch event every `epoch_every` steps
        if (step + 1) % epoch_every == 0 || step == args.steps - 1 {
            let _ = talkback.emit_epoch_end(&LossStep {
                loss: train_loss,
                train_loss,
                val_loss,
                epoch: step + 1,
                pattern: pattern_name.clone(),
            });
        }
        final_train_loss = train_loss;
        final_val_loss = val_loss;
    }

    if let Some(pb) = &pb {
        pb.finish_with_message("done");
    }

    let mut result: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    result.insert(
        "loss".to_string(),
        serde_json::Value::from(final_train_loss),
    );
    result.insert(
        "train_loss".to_string(),
        serde_json::Value::from(final_train_loss),
    );
    result.insert(
        "val_loss".to_string(),
        serde_json::Value::from(final_val_loss),
    );
    result.insert(
        "pattern".to_string(),
        serde_json::Value::from(pattern_name.clone()),
    );
    let primary = if args.metric_key == "loss" {
        final_train_loss
    } else {
        final_val_loss
    };
    if args.metric_key != "loss" && args.metric_key != "val_loss" {
        result.insert(args.metric_key.clone(), serde_json::Value::from(primary));
    }
    talkback
        .emit_result(&result)
        .expect("Failed to emit result");
}

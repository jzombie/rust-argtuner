mod patterns;

use argtuner_derive::talkback_args;
use indicatif::{ProgressBar, ProgressStyle};
use patterns::{LossPattern, Noisy, Overfitting, SmoothDecay, Spikes, Underfitting};
use std::thread;
use std::time::Duration;

#[talkback_args]
struct Args {
    /// The pattern to generate
    #[param(
        default = "smooth",
        choices = ["smooth", "overfitting", "underfitting", "spikes", "noisy-smooth"]
    )]
    pattern: String,

    /// Number of steps to generate
    #[param(default = 100)]
    steps: usize,

    /// Noise level (for noisy patterns)
    #[param(default = 0.1)]
    noise: f64,

    /// Spike probability (for spike patterns)
    #[param(default = 0.05)]
    spike_prob: f64,

    /// Metric key name (defaults to val_loss)
    #[param(default = "val_loss")]
    metric_key: String,

    /// Checkpoint directory (reserved: trial_dir)
    #[param(value_name = "trial_dir")]
    checkpoint_dir: Option<String>,

    /// Simulated epoch time in milliseconds
    #[param(default = 0.0)]
    epoch_time: f64,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum PatternType {
    Smooth,
    Overfitting,
    Underfitting,
    Spikes,
    NoisySmooth,
}

fn pattern_from_label(label: &str) -> PatternType {
    match label {
        "overfitting" => PatternType::Overfitting,
        "underfitting" => PatternType::Underfitting,
        "spikes" => PatternType::Spikes,
        "noisy-smooth" => PatternType::NoisySmooth,
        _ => PatternType::Smooth,
    }
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
    let (talkback, args) = argtuner::init::<Args>();
    let _ = args.checkpoint_dir;
    let pattern = pattern_from_label(&args.pattern);
    let pattern_name = pattern_label(pattern).to_string();

    let mut train_pattern: Box<dyn LossPattern> = match pattern {
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

    let mut val_pattern: Box<dyn LossPattern> = match pattern {
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
        let _ = talkback
            .metrics()
            .record("loss", train_loss)
            .record("train_loss", train_loss)
            .record("val_loss", val_loss)
            .record("epoch", step + 1)
            .record("pattern", pattern_name.clone())
            .emit_step();
        // Emit epoch event every `epoch_every` steps
        if (step + 1) % epoch_every == 0 || step == args.steps - 1 {
            let _ = talkback
                .metrics()
                .record("loss", train_loss)
                .record("train_loss", train_loss)
                .record("val_loss", val_loss)
                .record("epoch", step + 1)
                .record("pattern", pattern_name.clone())
                .emit();
        }
        final_train_loss = train_loss;
        final_val_loss = val_loss;
    }

    if let Some(pb) = &pb {
        pb.finish_with_message("done");
    }

    let primary = if args.metric_key == "loss" {
        final_train_loss
    } else {
        final_val_loss
    };
    let mut metrics = talkback.metrics();
    metrics
        .record("loss", final_train_loss)
        .record("train_loss", final_train_loss)
        .record("val_loss", final_val_loss)
        .record("pattern", &pattern_name);
    if args.metric_key != "loss" && args.metric_key != "val_loss" {
        metrics.record(args.metric_key.clone(), primary);
    }
    metrics.emit_result().expect("Failed to emit result");
}

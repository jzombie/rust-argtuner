# Loss Tuning Project

This project demonstrates `argtuner` using a synthetic loss generator. It simulates model training processes with various characteristics (smooth decay, overfitting, spikes, etc.) to test the tuner's visualization and optimization capabilities without the overhead of training real machine learning models.

## Prerequisites

Ensure you have the `argtuner` workspace set up.

## Usage

Run the tuner from the repository root, pointing it to this project directory:

```bash
cargo run -p argtuner -- run crates/loss-generator/loss-tuning-project
```

Viewing in TUI app:

```bash
cargo run -- watch --project crates/loss-generator/loss-tuning-project
```

## How it works

The project uses the `argtuner-loss-generator` crate (located in `../../`) as the trial command.

The `argtuner.toml` configuration defines a search space that explores different synthetic loss patterns:

- **pattern**: The type of loss curve to generate.
  - `smooth`: A standard decaying loss curve.
  - `overfitting`: Loss decreases then starts increasing.
  - `underfitting`: Loss stays high.
  - `spikes`: Generally decreasing but with random spikes.
  - `noisy-smooth`: Smooth decay with added noise.
- **noise**: Magnitude of random noise added to the curve.
- **spike_prob**: Probability of a spike occurring at any step.
- **epoch_time**: Simulated duration of each step in milliseconds (to test UI responsiveness).

The tuner will run multiple trials (defined by `n_trials` in `argtuner.toml`), and for each trial, the generator will emit both training loss (`loss`) and validation loss (`val_loss`) values. The tuner's goal is to minimize `val_loss` so you can see patterns like overfitting when the two curves diverge.

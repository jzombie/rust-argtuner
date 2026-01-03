use rand::Rng;

pub trait LossPattern {
    fn generate(&mut self, step: usize, total_steps: usize) -> f64;
    fn name(&self) -> &str;
}

pub struct SmoothDecay {
    pub start_loss: f64,
    pub end_loss: f64,
    pub decay_rate: f64,
}

impl SmoothDecay {
    pub fn new(start_loss: f64, end_loss: f64, decay_rate: f64) -> Self {
        Self {
            start_loss,
            end_loss,
            decay_rate,
        }
    }
}

impl LossPattern for SmoothDecay {
    fn generate(&mut self, step: usize, total_steps: usize) -> f64 {
        let progress = step as f64 / total_steps as f64;
        // Exponential decay: y = a * e^(-bx) + c
        // We want y(0) = start, y(1) = end
        // This is a simplified version
        let range = self.start_loss - self.end_loss;
        self.end_loss + range * (-self.decay_rate * progress).exp()
    }

    fn name(&self) -> &str {
        "Smooth Decay"
    }
}

pub struct Overfitting {
    pub min_loss_step: usize,
    pub base_pattern: SmoothDecay,
    pub rise_rate: f64,
}

impl Overfitting {
    pub fn new(start_loss: f64, min_loss: f64, min_loss_step: usize, rise_rate: f64) -> Self {
        Self {
            min_loss_step,
            base_pattern: SmoothDecay::new(start_loss, min_loss, 5.0),
            rise_rate,
        }
    }
}

impl LossPattern for Overfitting {
    fn generate(&mut self, step: usize, total_steps: usize) -> f64 {
        let base_loss = self.base_pattern.generate(step, total_steps);

        if step > self.min_loss_step {
            let overfit_steps = step - self.min_loss_step;
            // Quadratic rise after min point
            base_loss + self.rise_rate * (overfit_steps as f64).powi(2) / total_steps as f64
        } else {
            base_loss
        }
    }

    fn name(&self) -> &str {
        "Overfitting"
    }
}

pub struct Underfitting {
    pub constant_loss: f64,
    pub noise_level: f64,
}

impl Underfitting {
    pub fn new(constant_loss: f64, noise_level: f64) -> Self {
        Self {
            constant_loss,
            noise_level,
        }
    }
}

impl LossPattern for Underfitting {
    fn generate(&mut self, _step: usize, _total_steps: usize) -> f64 {
        let mut rng = rand::thread_rng();
        let noise = rng.gen_range(-self.noise_level..self.noise_level);
        self.constant_loss + noise
    }

    fn name(&self) -> &str {
        "Underfitting"
    }
}

pub struct Spikes {
    pub base_pattern: Box<dyn LossPattern>,
    pub spike_probability: f64,
    pub spike_magnitude: f64,
}

impl Spikes {
    pub fn new(
        base_pattern: Box<dyn LossPattern>,
        spike_probability: f64,
        spike_magnitude: f64,
    ) -> Self {
        Self {
            base_pattern,
            spike_probability,
            spike_magnitude,
        }
    }
}

impl LossPattern for Spikes {
    fn generate(&mut self, step: usize, total_steps: usize) -> f64 {
        let base_loss = self.base_pattern.generate(step, total_steps);
        let mut rng = rand::thread_rng();

        if rng.gen_bool(self.spike_probability) {
            base_loss + rng.gen_range(0.0..self.spike_magnitude)
        } else {
            base_loss
        }
    }

    fn name(&self) -> &str {
        "Spikes"
    }
}

pub struct Noisy {
    pub base_pattern: Box<dyn LossPattern>,
    pub noise_level: f64,
}

impl Noisy {
    pub fn new(base_pattern: Box<dyn LossPattern>, noise_level: f64) -> Self {
        Self {
            base_pattern,
            noise_level,
        }
    }
}

impl LossPattern for Noisy {
    fn generate(&mut self, step: usize, total_steps: usize) -> f64 {
        let base_loss = self.base_pattern.generate(step, total_steps);
        let mut rng = rand::thread_rng();
        let noise = rng.gen_range(-self.noise_level..self.noise_level);
        base_loss + noise
    }

    fn name(&self) -> &str {
        "Noisy"
    }
}

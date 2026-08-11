mod config;
mod fixed;
pub mod plan;
mod successive_halving;

use crate::{TrialOverrides, project::ProjectConfig};

pub use config::Scheduler;
pub use fixed::FixedScheduler;
pub use plan::{ConfigPlanStep, PlanTier, SchedulerPlan, build_plan};
pub use successive_halving::SuccessiveHalvingScheduler;

#[derive(Debug, Clone, Copy)]
pub struct TrialToken {
    pub config_id: usize,
    pub rung: usize,
    pub bracket: usize,
    pub budget_epochs: usize,
}

#[derive(Debug, Clone)]
pub struct ScheduledTrial {
    pub coords: Vec<f64>,
    pub token: TrialToken,
    pub overrides: TrialOverrides,
}

pub trait TrialScheduler {
    fn next_trial(&mut self) -> Option<ScheduledTrial>;
    fn record_result(&mut self, token: TrialToken, score: Option<f64>);
    fn is_done(&self) -> bool;
    fn retry_trial(&mut self, _token: TrialToken) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SchedulerBinding<'a> {
    config: &'a ProjectConfig,
}

impl<'a> SchedulerBinding<'a> {
    pub fn new(config: &'a ProjectConfig) -> Self {
        Self { config }
    }

    pub fn allows_placeholder(&self, placeholder: &str) -> bool {
        matches!(
            self.config.scheduler.kind,
            Scheduler::SuccessiveHalving
                if placeholder == self.config.scheduler.successive_halving.budget_placeholder
        )
    }

    pub fn validate_template(&self, template_placeholders: &[String]) -> Result<(), String> {
        if matches!(self.config.scheduler.kind, Scheduler::SuccessiveHalving)
            && !template_placeholders
                .iter()
                .any(|p| p == &self.config.scheduler.successive_halving.budget_placeholder)
        {
            return Err(format!(
                "template missing budget placeholder {{{}}}",
                self.config.scheduler.successive_halving.budget_placeholder
            ));
        }
        Ok(())
    }

    pub fn build(&self, dims: usize) -> Box<dyn TrialScheduler> {
        match self.config.scheduler.kind {
            Scheduler::Fixed => Box::new(FixedScheduler::new_with_seed(
                dims,
                self.config.scheduler.n_trials,
                self.config.scheduler.seed,
                None,
                None,
            )),
            Scheduler::SuccessiveHalving => {
                let sh = &self.config.scheduler.successive_halving;
                Box::new(SuccessiveHalvingScheduler::new_with_seed(
                    dims,
                    self.config.scheduler.n_trials,
                    sh.min_epochs,
                    sh.max_epochs,
                    sh.eta,
                    sh.budget_placeholder.clone(),
                    self.config.scheduler.seed,
                ))
            }
        }
    }
}
fn sample_unit(rng: &mut rand::rngs::StdRng, dims: usize) -> Vec<f64> {
    use rand::RngExt;

    (0..dims).map(|_| rng.random_range(0.0..=1.0)).collect()
}

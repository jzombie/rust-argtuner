use crate::{FIELD_TUNING_BUDGET_REMAINING, FIELD_TUNING_BUDGET_TOTAL};
use crate::{
    TrialOverrides,
    constants::{
        FIELD_TRIAL_BRACKET, FIELD_TRIAL_BUDGET_EPOCHS, FIELD_TRIAL_BUDGET_STEP,
        FIELD_TRIAL_BUDGET_TOTAL, FIELD_TRIAL_CONFIG_ID, FIELD_TRIAL_RUNG, TRIAL_PREFIX,
    },
};
use rand::SeedableRng;

use super::{ScheduledTrial, TrialScheduler, TrialToken, sample_unit};

#[derive(Debug)]
pub struct FixedScheduler {
    dims: usize,
    remaining: usize,
    next_config_id: usize,
    rng: rand::rngs::StdRng,
    budget_epochs: Option<usize>,
    budget_key: Option<String>,
    total_budget: usize,
    issued_budget: usize,
}

impl FixedScheduler {
    pub fn new(
        dims: usize,
        n_trials: usize,
        budget_epochs: Option<usize>,
        budget_key: Option<String>,
    ) -> Self {
        Self::new_with_seed(
            dims,
            n_trials,
            rand::random::<u64>(),
            budget_epochs,
            budget_key,
        )
    }

    pub fn new_with_seed(
        dims: usize,
        n_trials: usize,
        seed: u64,
        budget_epochs: Option<usize>,
        budget_key: Option<String>,
    ) -> Self {
        let total_budget = n_trials * budget_epochs.unwrap_or(0);
        Self {
            dims,
            remaining: n_trials,
            next_config_id: 0,
            rng: rand::rngs::StdRng::seed_from_u64(seed),
            budget_epochs,
            budget_key,
            total_budget,
            issued_budget: 0,
        }
    }
}

impl TrialScheduler for FixedScheduler {
    fn next_trial(&mut self) -> Option<ScheduledTrial> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let coords = sample_unit(&mut self.rng, self.dims);
        let config_id = self.next_config_id;
        self.next_config_id += 1;
        let budget_epochs = self.budget_epochs.unwrap_or(0);
        self.issued_budget += budget_epochs;
        let remaining_budget = self.total_budget.saturating_sub(self.issued_budget);

        let mut overrides = TrialOverrides::default();
        if let (Some(budget), Some(key)) = (self.budget_epochs, self.budget_key.as_ref()) {
            overrides.values.insert(key.to_string(), budget.to_string());
            overrides
                .fields
                .insert(FIELD_TRIAL_BUDGET_EPOCHS.to_string(), budget.to_string());
            overrides
                .fields
                .insert(format!("{TRIAL_PREFIX}{key}"), budget.to_string());
            overrides
                .fields
                .insert(FIELD_TRIAL_BUDGET_TOTAL.to_string(), budget.to_string());
            overrides
                .fields
                .insert(FIELD_TRIAL_BUDGET_STEP.to_string(), budget.to_string());
            overrides.fields.insert(
                FIELD_TUNING_BUDGET_TOTAL.to_string(),
                self.total_budget.to_string(),
            );
            overrides.fields.insert(
                FIELD_TUNING_BUDGET_REMAINING.to_string(),
                remaining_budget.to_string(),
            );
        }
        overrides
            .fields
            .insert(FIELD_TRIAL_CONFIG_ID.to_string(), config_id.to_string());
        overrides
            .fields
            .insert(FIELD_TRIAL_RUNG.to_string(), "0".to_string());
        overrides
            .fields
            .insert(FIELD_TRIAL_BRACKET.to_string(), "0".to_string());
        Some(ScheduledTrial {
            coords,
            token: TrialToken {
                config_id,
                rung: 0,
                bracket: 0,
                budget_epochs,
            },
            overrides,
        })
    }

    fn record_result(&mut self, _token: TrialToken, _scores: Vec<f64>) {}

    fn is_done(&self) -> bool {
        self.remaining == 0
    }

    fn retry_trial(&mut self, token: TrialToken) -> bool {
        // Refund the trial budget because the previous attempt was invalid/failed.
        self.remaining = self.remaining.saturating_add(1);
        self.issued_budget = self.issued_budget.saturating_sub(token.budget_epochs);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::FixedScheduler;
    use crate::{FIELD_TUNING_BUDGET_REMAINING, FIELD_TUNING_BUDGET_TOTAL, TrialScheduler};

    #[test]
    fn fixed_scheduler_emits_expected_budgets() {
        let mut scheduler =
            FixedScheduler::new_with_seed(2, 3, 123, Some(5), Some("epochs".to_string()));
        let mut seen = Vec::new();
        while let Some(trial) = scheduler.next_trial() {
            seen.push(trial.token.budget_epochs);
            assert_eq!(
                trial.overrides.values.get("epochs").map(String::as_str),
                Some("5")
            );
            scheduler.record_result(trial.token, vec![1.0]);
        }
        assert_eq!(seen, vec![5, 5, 5]);
        assert!(scheduler.is_done());
    }

    #[test]
    fn fixed_scheduler_retries_invalid_trials() {
        let mut scheduler = FixedScheduler::new_with_seed(2, 1, 7, None, None);
        let trial = scheduler.next_trial().expect("trial");
        assert!(scheduler.retry_trial(trial.token));
        assert!(scheduler.next_trial().is_some());
    }

    #[test]
    fn fixed_scheduler_budget_accounting() {
        // 3 trials * 10 epochs = 30 total
        let mut scheduler =
            FixedScheduler::new_with_seed(2, 3, 123, Some(10), Some("epochs".to_string()));

        // 1. First trial
        let t1 = scheduler.next_trial().expect("t1");
        assert_eq!(
            t1.overrides
                .fields
                .get(FIELD_TUNING_BUDGET_TOTAL)
                .map(|s: &String| s.as_str()),
            Some("30")
        );
        assert_eq!(
            t1.overrides
                .fields
                .get(FIELD_TUNING_BUDGET_REMAINING)
                .map(|s: &String| s.as_str()),
            Some("20") // 30 - 10
        );

        // 2. Retry t1 (refund)
        assert!(scheduler.retry_trial(t1.token));

        // 3. Re-issue t1
        let t1_retry = scheduler.next_trial().expect("t1_retry");
        assert_eq!(
            t1_retry
                .overrides
                .fields
                .get(FIELD_TUNING_BUDGET_TOTAL)
                .map(|s: &String| s.as_str()),
            Some("30")
        );
        assert_eq!(
            t1_retry
                .overrides
                .fields
                .get(FIELD_TUNING_BUDGET_REMAINING)
                .map(|s: &String| s.as_str()),
            Some("20") // Should be back to 20, not 10
        );

        scheduler.record_result(t1_retry.token, vec![1.0]);

        // 4. Second trial
        let t2 = scheduler.next_trial().expect("t2");
        assert_eq!(
            t2.overrides
                .fields
                .get(FIELD_TUNING_BUDGET_REMAINING)
                .map(|s: &String| s.as_str()),
            Some("10") // 20 - 10
        );
    }
}

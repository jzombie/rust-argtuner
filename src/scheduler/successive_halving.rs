use std::collections::BTreeMap;

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

#[derive(Debug, Clone)]
pub struct SuccessiveHalvingScheduler {
    dims: usize,
    budgets: Vec<usize>,
    eta: usize,
    budget_key: String,
    rung: usize,
    current: Vec<ConfigCandidate>,
    pending: Vec<ConfigCandidate>,
    scores: BTreeMap<usize, f64>,
    rng: rand::rngs::StdRng,
    total_budget: usize,
    issued_budget: usize,
}

#[derive(Debug, Clone)]
struct ConfigCandidate {
    id: usize,
    coords: Vec<f64>,
}

impl SuccessiveHalvingScheduler {
    pub fn new(
        dims: usize,
        n_trials: usize,
        min_epochs: usize,
        max_epochs: usize,
        eta: usize,
        budget_key: String,
    ) -> Self {
        use rand::RngCore;
        Self::new_with_seed(
            dims,
            n_trials,
            min_epochs,
            max_epochs,
            eta,
            budget_key,
            rand::thread_rng().next_u64(),
        )
    }

    pub fn new_with_seed(
        dims: usize,
        n_trials: usize,
        min_epochs: usize,
        max_epochs: usize,
        eta: usize,
        budget_key: String,
        seed: u64,
    ) -> Self {
        let budgets = build_budgets(min_epochs, max_epochs, eta);
        let total_budget = calculate_total_budget(n_trials, &budgets, eta);
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        let mut current = Vec::with_capacity(n_trials);
        for id in 0..n_trials {
            current.push(ConfigCandidate {
                id,
                coords: sample_unit(&mut rng, dims),
            });
        }
        let mut pending = current.clone();
        pending.reverse();
        Self {
            dims,
            budgets,
            eta,
            budget_key,
            rung: 0,
            current,
            pending,
            scores: BTreeMap::new(),
            rng,
            total_budget,
            issued_budget: 0,
        }
    }

    fn promote(&mut self) {
        if self.pending.is_empty() && self.rung + 1 < self.budgets.len() {
            let mut scored = self
                .current
                .iter()
                .map(|candidate| {
                    let score = self
                        .scores
                        .get(&candidate.id)
                        .cloned()
                        .unwrap_or(f64::INFINITY);
                    (candidate.clone(), score)
                })
                .filter(|(_, score)| !score.is_infinite())
                .collect::<Vec<_>>();
            scored.sort_by(|a, b| a.1.total_cmp(&b.1));
            let keep = (scored.len() as f64 / self.eta as f64).ceil().max(1.0) as usize;
            self.current = scored
                .into_iter()
                .take(keep)
                .map(|(candidate, _)| candidate)
                .collect();
            self.pending = self.current.clone();
            self.pending.reverse();
            self.scores.clear();
            self.rung += 1;
        }
    }
}

impl TrialScheduler for SuccessiveHalvingScheduler {
    fn next_trial(&mut self) -> Option<ScheduledTrial> {
        if self.pending.is_empty() {
            self.promote();
        }
        let candidate = self.pending.pop()?;
        let budget_epochs = self.budgets.get(self.rung).copied().unwrap_or(0);
        let previous_budget = if self.rung > 0 {
            self.budgets.get(self.rung - 1).copied().unwrap_or(0)
        } else {
            0
        };
        let remaining_budget = budget_epochs.saturating_sub(previous_budget);
        let max_budget = self.budgets.last().copied().unwrap_or(0);
        self.issued_budget += remaining_budget;
        let tuning_remaining = self.total_budget.saturating_sub(self.issued_budget);

        let mut overrides = TrialOverrides::default();
        overrides
            .values
            .insert(self.budget_key.clone(), budget_epochs.to_string());
        overrides.fields.insert(
            FIELD_TRIAL_BUDGET_EPOCHS.to_string(),
            budget_epochs.to_string(),
        );
        overrides.fields.insert(
            format!("{TRIAL_PREFIX}{}", self.budget_key),
            budget_epochs.to_string(),
        );
        overrides
            .fields
            .insert(FIELD_TRIAL_BUDGET_TOTAL.to_string(), max_budget.to_string());
        overrides.fields.insert(
            FIELD_TRIAL_BUDGET_STEP.to_string(),
            remaining_budget.to_string(),
        );
        overrides.fields.insert(
            FIELD_TUNING_BUDGET_TOTAL.to_string(),
            self.total_budget.to_string(),
        );
        overrides.fields.insert(
            FIELD_TUNING_BUDGET_REMAINING.to_string(),
            tuning_remaining.to_string(),
        );
        overrides
            .fields
            .insert(FIELD_TRIAL_CONFIG_ID.to_string(), candidate.id.to_string());
        overrides
            .fields
            .insert(FIELD_TRIAL_RUNG.to_string(), self.rung.to_string());
        overrides
            .fields
            .insert(FIELD_TRIAL_BRACKET.to_string(), "0".to_string());
        Some(ScheduledTrial {
            coords: candidate.coords,
            token: TrialToken {
                config_id: candidate.id,
                rung: self.rung,
                bracket: 0,
                budget_epochs,
            },
            overrides,
        })
    }

    fn record_result(&mut self, token: TrialToken, score: Option<f64>) {
        let value = score.unwrap_or(f64::INFINITY);
        self.scores.insert(token.config_id, value);
    }

    fn is_done(&self) -> bool {
        self.pending.is_empty() && (self.rung + 1 >= self.budgets.len() || self.current.len() <= 1)
    }

    fn retry_trial(&mut self, token: TrialToken) -> bool {
        // Refund the issued budget for this failed attempt.
        let budget_epochs = token.budget_epochs;
        let previous_budget = if token.rung > 0 {
            self.budgets.get(token.rung - 1).copied().unwrap_or(0)
        } else {
            0
        };
        let incremental_budget = budget_epochs.saturating_sub(previous_budget);
        self.issued_budget = self.issued_budget.saturating_sub(incremental_budget);

        // Resample coordinates for the failed trial to ensure we don't retry the same invalid config.
        let coords = sample_unit(&mut self.rng, self.dims);
        if let Some(candidate) = self.current.iter_mut().find(|c| c.id == token.config_id) {
            candidate.coords = coords.clone();
        } else {
            self.current.push(ConfigCandidate {
                id: token.config_id,
                coords: coords.clone(),
            });
        }
        // Push back to pending to "refund" the trial slot.
        self.pending.push(ConfigCandidate {
            id: token.config_id,
            coords,
        });
        self.scores.remove(&token.config_id);
        true
    }
}

fn build_budgets(min_epochs: usize, max_epochs: usize, eta: usize) -> Vec<usize> {
    let min_epochs = min_epochs.max(1);
    let max_epochs = max_epochs.max(min_epochs);
    let eta = eta.max(2);
    let mut budgets = Vec::new();
    let mut value = min_epochs;
    while value < max_epochs {
        budgets.push(value);
        value = value.saturating_mul(eta);
    }
    budgets.push(max_epochs);
    budgets
}

fn calculate_total_budget(n_trials: usize, budgets: &[usize], eta: usize) -> usize {
    let mut total_epochs = 0;
    let mut current_count = n_trials;
    let mut previous_budget = 0;

    for &budget in budgets {
        let incremental_budget = budget.saturating_sub(previous_budget);
        total_epochs += current_count * incremental_budget;
        current_count = (current_count as f64 / eta as f64).ceil().max(1.0) as usize;
        previous_budget = budget;
    }
    total_epochs
}

#[cfg(test)]
mod tests {
    use super::{SuccessiveHalvingScheduler, build_budgets};
    use crate::{FIELD_TUNING_BUDGET_REMAINING, FIELD_TUNING_BUDGET_TOTAL, TrialScheduler};
    use std::collections::BTreeMap;

    #[test]
    fn budgets_include_min_and_max() {
        let budgets = build_budgets(2, 9, 3);
        assert_eq!(budgets, vec![2, 6, 9]);
    }

    #[test]
    fn successive_halving_progresses_rungs() {
        let mut scheduler =
            SuccessiveHalvingScheduler::new_with_seed(2, 6, 2, 8, 2, "epochs".to_string(), 7);
        let mut seen = 0;
        while let Some(trial) = scheduler.next_trial() {
            scheduler.record_result(trial.token, Some(seen as f64));
            seen += 1;
        }
        assert!(scheduler.is_done());
        assert!(seen > 0);
    }

    #[test]
    fn successive_halving_starts_with_lowest_config_id() {
        let mut scheduler =
            SuccessiveHalvingScheduler::new_with_seed(1, 4, 1, 4, 2, "epochs".to_string(), 7);
        let mut ids = Vec::new();
        for _ in 0..4 {
            let trial = scheduler.next_trial().expect("trial");
            ids.push(trial.token.config_id);
        }
        assert_eq!(ids, vec![0, 1, 2, 3]);
    }

    #[test]
    fn successive_halving_produces_budgeted_rungs() {
        let mut scheduler =
            SuccessiveHalvingScheduler::new_with_seed(2, 6, 2, 8, 2, "epochs".to_string(), 7);
        let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
        while let Some(trial) = scheduler.next_trial() {
            *counts.entry(trial.token.budget_epochs).or_insert(0) += 1;
            scheduler.record_result(trial.token, Some(trial.token.config_id as f64));
        }
        assert_eq!(counts.get(&2), Some(&6));
        assert_eq!(counts.get(&4), Some(&3));
        assert_eq!(counts.get(&8), Some(&2));
        assert!(scheduler.is_done());
    }

    #[test]
    fn successive_halving_retries_invalid_trials() {
        let mut scheduler =
            SuccessiveHalvingScheduler::new_with_seed(2, 1, 2, 4, 2, "epochs".to_string(), 7);
        let trial = scheduler.next_trial().expect("trial");
        assert!(scheduler.retry_trial(trial.token));
        assert!(scheduler.next_trial().is_some());
    }

    #[test]
    fn successive_halving_marks_resume_in_env() {
        let mut scheduler =
            SuccessiveHalvingScheduler::new_with_seed(1, 2, 1, 4, 2, "epochs".to_string(), 7);
        let first = scheduler.next_trial().expect("trial 0");
        assert_eq!(
            first.overrides.values.get("epochs").map(String::as_str),
            Some("1")
        );
        scheduler.record_result(first.token, Some(1.0));

        let second = scheduler.next_trial().expect("trial 1");
        scheduler.record_result(second.token, Some(2.0));

        let promoted = scheduler.next_trial().expect("trial 2");
        assert_eq!(promoted.token.rung, 1);
        assert_eq!(
            promoted.overrides.values.get("epochs").map(String::as_str),
            Some("2")
        );
    }

    #[test]
    fn successive_halving_budget_accounting() {
        // n=4, min=2, max=4, eta=2
        // Rung 0: 4 trials * 2 epochs = 8
        // Rung 1: 2 trials * (4-2) epochs = 4
        // Total = 12
        let mut scheduler =
            SuccessiveHalvingScheduler::new_with_seed(2, 4, 2, 4, 2, "epochs".to_string(), 7);

        // 1. First trial (Rung 0)
        let t1 = scheduler.next_trial().expect("t1");
        assert_eq!(t1.token.budget_epochs, 2);
        assert_eq!(
            t1.overrides
                .fields
                .get(FIELD_TUNING_BUDGET_TOTAL)
                .map(|s: &String| s.as_str()),
            Some("12")
        );
        assert_eq!(
            t1.overrides
                .fields
                .get(FIELD_TUNING_BUDGET_REMAINING)
                .map(|s: &String| s.as_str()),
            Some("10") // 12 - 2
        );

        // 2. Retry t1
        assert!(scheduler.retry_trial(t1.token));

        // 3. Re-issue t1
        let t1_retry = scheduler.next_trial().expect("t1_retry");
        assert_eq!(
            t1_retry
                .overrides
                .fields
                .get(FIELD_TUNING_BUDGET_REMAINING)
                .map(|s: &String| s.as_str()),
            Some("10") // Should be 10 again
        );
        scheduler.record_result(t1_retry.token, Some(1.0));

        // 4. Run remaining 3 trials in Rung 0
        for _ in 0..3 {
            let t = scheduler.next_trial().expect("rung0");
            scheduler.record_result(t.token, Some(1.0));
        }
        // Issued so far: 4 * 2 = 8. Remaining: 12 - 8 = 4.

        // 5. Promote to Rung 1 (2 trials promote)
        // First promoted trial
        let p1 = scheduler.next_trial().expect("p1");
        assert_eq!(p1.token.rung, 1);
        assert_eq!(p1.token.budget_epochs, 4);
        // Incremental cost for p1 is 4 - 2 = 2 epochs.
        // Remaining: 4 - 2 = 2.
        assert_eq!(
            p1.overrides
                .fields
                .get(FIELD_TUNING_BUDGET_REMAINING)
                .map(|s: &String| s.as_str()),
            Some("2")
        );

        // 6. Retry p1
        assert!(scheduler.retry_trial(p1.token));

        // 7. Re-issue p1
        let p1_retry = scheduler.next_trial().expect("p1_retry");
        assert_eq!(
            p1_retry
                .overrides
                .fields
                .get(FIELD_TUNING_BUDGET_REMAINING)
                .map(|s: &String| s.as_str()),
            Some("2") // Should be 2 again
        );
    }
}

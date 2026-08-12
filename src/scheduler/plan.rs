use std::fmt::Write;

use crate::project::ProjectConfig;
use crate::scheduler::Scheduler;

#[derive(Debug, Clone)]
pub struct SchedulerPlan {
    pub kind: Scheduler,
    pub n_trials: usize,
    pub eta: Option<usize>,
    pub budget_placeholder: Option<String>,
    pub total_budget: Option<usize>,
    pub tiers: Vec<PlanTier>,
    pub config_plan: Option<Vec<ConfigPlanStep>>,
}

#[derive(Debug, Clone)]
pub struct PlanTier {
    pub rung: usize,
    pub budget_epochs: Option<usize>,
    pub budget_increment: Option<usize>,
    pub configs: usize,
    pub promote: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ConfigPlanStep {
    pub rung: usize,
    pub budget_epochs: Option<usize>,
    pub budget_increment: Option<usize>,
}

pub fn build_plan(config: &ProjectConfig, config_id: Option<usize>) -> SchedulerPlan {
    match config.scheduler.kind {
        Scheduler::Fixed => {
            let tiers = vec![PlanTier {
                rung: 0,
                budget_epochs: None,
                budget_increment: None,
                configs: config.scheduler.n_trials,
                promote: None,
            }];
            let config_plan = config_id.map(|_| {
                vec![ConfigPlanStep {
                    rung: 0,
                    budget_epochs: None,
                    budget_increment: None,
                }]
            });
            SchedulerPlan {
                kind: Scheduler::Fixed,
                n_trials: config.scheduler.n_trials,
                eta: None,
                budget_placeholder: None,
                total_budget: None,
                tiers,
                config_plan,
            }
        }
        Scheduler::SuccessiveHalving => {
            let sh = &config.scheduler.successive_halving;
            let budgets = build_budgets(sh.min_epochs, sh.max_epochs, sh.eta);
            let counts =
                successive_halving_counts(config.scheduler.n_trials, sh.eta, budgets.len());
            let total_budget = calculate_total_budget(config.scheduler.n_trials, &budgets, sh.eta);
            let mut tiers = Vec::with_capacity(budgets.len());
            for (idx, budget_epochs) in budgets.iter().copied().enumerate() {
                let previous_budget = budgets.get(idx.saturating_sub(1)).copied().unwrap_or(0);
                let budget_increment = budget_epochs.saturating_sub(previous_budget);
                let promote = counts.get(idx + 1).copied();
                tiers.push(PlanTier {
                    rung: idx,
                    budget_epochs: Some(budget_epochs),
                    budget_increment: Some(budget_increment),
                    configs: *counts.get(idx).unwrap_or(&0),
                    promote,
                });
            }
            let config_plan = config_id.map(|_| {
                budgets
                    .iter()
                    .enumerate()
                    .map(|(idx, budget_epochs)| {
                        let previous_budget =
                            budgets.get(idx.saturating_sub(1)).copied().unwrap_or(0);
                        let budget_increment = budget_epochs.saturating_sub(previous_budget);
                        ConfigPlanStep {
                            rung: idx,
                            budget_epochs: Some(*budget_epochs),
                            budget_increment: Some(budget_increment),
                        }
                    })
                    .collect::<Vec<_>>()
            });
            SchedulerPlan {
                kind: Scheduler::SuccessiveHalving,
                n_trials: config.scheduler.n_trials,
                eta: Some(sh.eta),
                budget_placeholder: Some(sh.budget_placeholder.clone()),
                total_budget: Some(total_budget),
                tiers,
                config_plan,
            }
        }
    }
}

impl SchedulerPlan {
    pub fn render(&self, config_id: Option<usize>) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "Scheduler: {}", scheduler_label(self.kind));
        let _ = writeln!(out, "Trials: {}", self.n_trials);
        if let Some(eta) = self.eta {
            let _ = writeln!(out, "Eta: {}", eta);
        }
        if let Some(placeholder) = self.budget_placeholder.as_ref() {
            let _ = writeln!(out, "Budget placeholder: {{{}}}", placeholder);
        }
        if let Some(total_budget) = self.total_budget {
            let _ = writeln!(out, "Total budget: {}", total_budget);
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "{:<6} {:<8} {:<9} {:<8} {:<8}",
            "Rung", "Budget", "Increment", "Configs", "Promote"
        );
        for tier in &self.tiers {
            let budget = tier
                .budget_epochs
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string());
            let increment = tier
                .budget_increment
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string());
            let promote = tier
                .promote
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".to_string());
            let _ = writeln!(
                out,
                "{:<6} {:<8} {:<9} {:<8} {:<8}",
                tier.rung, budget, increment, tier.configs, promote
            );
        }
        if let Some(steps) = self.config_plan.as_ref()
            && let Some(config_id) = config_id
        {
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "Config {} path (assumes promotion each rung):",
                config_id
            );
            for step in steps {
                let budget = step
                    .budget_epochs
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let increment = step
                    .budget_increment
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".to_string());
                let _ = writeln!(
                    out,
                    "  rung {:<3} budget {:<8} increment {}",
                    step.rung, budget, increment
                );
            }
        }
        out
    }
}

pub(crate) fn build_budgets(min_epochs: usize, max_epochs: usize, eta: usize) -> Vec<usize> {
    let min_epochs = min_epochs.max(1);
    let max_epochs = max_epochs.max(min_epochs);
    let eta = eta.max(2);
    let mut budgets = Vec::new();
    let mut value = min_epochs;
    let mut step = 1usize;
    while value < max_epochs {
        budgets.push(value);
        value = value.saturating_add(step);
        step = step.saturating_mul(eta);
    }
    if budgets.last() != Some(&max_epochs) {
        budgets.push(max_epochs);
    }
    budgets
}

pub(crate) fn calculate_total_budget(n_trials: usize, budgets: &[usize], eta: usize) -> usize {
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

fn successive_halving_counts(n_trials: usize, eta: usize, rungs: usize) -> Vec<usize> {
    let mut counts = Vec::with_capacity(rungs);
    let mut current = n_trials;
    for _ in 0..rungs {
        counts.push(current);
        current = (current as f64 / eta as f64).ceil().max(1.0) as usize;
    }
    counts
}

fn scheduler_label(kind: Scheduler) -> &'static str {
    match kind {
        Scheduler::Fixed => "Fixed",
        Scheduler::SuccessiveHalving => "SuccessiveHalving",
    }
}

#[cfg(test)]
mod tests {
    use super::{build_budgets, build_plan};
    use crate::project::{ProjectConfig, SchedulerConfig, SuccessiveHalvingSchedulerConfig};
    use crate::scheduler::Scheduler;

    #[test]
    fn budgets_real_epochs_double_per_rung() {
        let budgets = build_budgets(1689, 1708, 2);
        let start_epoch = 1688;
        let real_epochs: Vec<usize> = budgets.iter().map(|b| b - start_epoch).collect();
        assert_eq!(real_epochs, vec![1, 2, 4, 8, 16, 20]);
        for pair in real_epochs.windows(2) {
            assert!(
                pair[1] == pair[0] * 2 || pair[1] <= pair[0] * 2,
                "real epochs should double per rung: {:?}",
                pair
            );
        }
    }

    #[test]
    fn plan_matches_successive_halving_budgets() {
        let config = ProjectConfig {
            metric_key: "metric".to_string(),
            goal: crate::Goal::Min,
            sampler: crate::SamplerConfig::default(),
            scheduler: SchedulerConfig {
                kind: Scheduler::SuccessiveHalving,
                n_trials: 6,
                seed: 7,
                trial_timeout_s: 0,
                fixed: crate::FixedSchedulerConfig::default(),
                successive_halving: SuccessiveHalvingSchedulerConfig {
                    budget_placeholder: "epochs".to_string(),
                    min_epochs: 2,
                    max_epochs: 8,
                    eta: 2,
                },
            },
            pruner: crate::Pruner::None,
            inject_trial_placeholders: true,
            checkpoint_arg: None,
        };

        let plan = build_plan(&config, Some(0));
        let budgets = build_budgets(2, 8, 2);
        assert_eq!(plan.tiers.len(), budgets.len());
        assert_eq!(plan.tiers[0].configs, 6);
        assert_eq!(plan.tiers[1].configs, 3);
        assert_eq!(plan.tiers[2].configs, 2);
        assert_eq!(plan.tiers[3].configs, 1);
        assert!(plan.config_plan.is_some());
    }
}

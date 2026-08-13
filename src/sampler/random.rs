use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::TrialOverrides;
use crate::command::CommandObjective;
use crate::constants::{
    DUPLICATE_CONFIG_PREFIX, FIELD_SCORE, FIELD_TRIAL_BRACKET, FIELD_TRIAL_CONFIG_ID,
    FIELD_TRIAL_ID, FIELD_TRIAL_RUNG, FIELD_TRIAL_STATUS, HP_PREFIX, INVALID_CONFIG_PREFIX,
    MAX_DUPLICATE_RETRIES, MAX_RANDOM_EXHAUSTIVE_CONFIGS,
};
use crate::scheduler::TrialScheduler;
use crate::search_space::SearchSpace;

use crate::trial::store::{TrialStatus, TrialStore};
#[derive(Clone)]
struct CompletedTrial {
    status: TrialStatus,
    score: Option<Vec<f64>>,
    trial_id: usize,
}

type TrialKey = (usize, usize, usize);
type CompletedTrialMap = BTreeMap<TrialKey, CompletedTrial>;

pub fn run_random(
    objective: CommandObjective,
    scheduler: Box<dyn TrialScheduler>,
    stop_flag: Option<Arc<AtomicBool>>,
) -> Result<(), Box<dyn Error>> {
    let completed = load_completed_trials(objective.store(), &objective.objective_names())?;
    run_scheduled(objective, scheduler, completed, stop_flag, None)
}

/// Multi-objective driver: the same scheduler-driven loop, additionally
/// maintaining the non-dominated front and printing it at the end.
pub fn run_pareto(
    objective: CommandObjective,
    scheduler: Box<dyn TrialScheduler>,
    stop_flag: Option<Arc<AtomicBool>>,
) -> Result<(), Box<dyn Error>> {
    let objective_names = objective.objective_names();
    let completed = load_completed_trials(objective.store(), &objective_names)?;
    let mut front = crate::sampler::pareto::ParetoFront::new();
    run_scheduled(objective, scheduler, completed, stop_flag, Some(&mut front))?;
    Ok(())
}

struct DiscreteConfigPool {
    params: Vec<String>,
    available: Vec<Vec<String>>,
    assigned: HashMap<usize, Vec<String>>,
}

impl DiscreteConfigPool {
    fn new(space: &SearchSpace, store: &TrialStore) -> Result<Option<Self>, Box<dyn Error>> {
        // Build the pool lazily only after a duplicate is observed.
        // The cap applies to the total discrete space size, not attempts per trial.
        let mut total = 1usize;
        for spec in &space.params {
            let Some(count) = spec.discrete_value_count() else {
                return Ok(None);
            };
            total = total.saturating_mul(count);
            if total > MAX_RANDOM_EXHAUSTIVE_CONFIGS {
                return Ok(None);
            }
        }
        let params: Vec<String> = space.params.iter().map(|s| s.name().to_string()).collect();
        let conditional: HashSet<String> = space
            .params
            .iter()
            .filter(|s| s.is_conditional())
            .map(|s| s.name().to_string())
            .collect();
        let used = collect_used_configs(store, &params, &conditional)?;
        let mut available = Vec::new();
        let mut current = Vec::with_capacity(space.params.len());
        let mut sampled = HashMap::new();
        build_available_configs(space, &used, &mut current, &mut available, &mut sampled);
        Ok(Some(Self {
            params,
            available,
            assigned: HashMap::new(),
        }))
    }

    fn assign_values(&mut self, config_id: usize) -> Option<Vec<String>> {
        if let Some(values) = self.assigned.get(&config_id) {
            return Some(values.clone());
        }
        let values = self.available.pop()?;
        self.assigned.insert(config_id, values.clone());
        Some(values)
    }

    fn apply_to_overrides(&self, values: &[String], overrides: &mut TrialOverrides) {
        for (name, value) in self.params.iter().zip(values.iter()) {
            if value.is_empty() {
                continue; // inactive conditional param: keep omitted
            }
            overrides.values.insert(name.clone(), value.clone());
            overrides
                .fields
                .insert(format!("{HP_PREFIX}{name}"), value.clone());
        }
    }

    fn forget_config(&mut self, config_id: usize) {
        self.assigned.remove(&config_id);
    }
}

fn run_scheduled(
    objective: CommandObjective,
    mut scheduler: Box<dyn TrialScheduler>,
    mut completed: CompletedTrialMap,
    stop_flag: Option<Arc<AtomicBool>>,
    mut pareto_front: Option<&mut crate::sampler::pareto::ParetoFront>,
) -> Result<(), Box<dyn Error>> {
    let mut retry_trial_ids: HashMap<TrialKey, usize> = HashMap::new();
    let mut duplicate_retries = 0usize;
    let mut discrete_pool: Option<DiscreteConfigPool> = None;
    while let Some(trial) = scheduler.next_trial() {
        // Graceful shutdown: check if Ctrl-C was pressed between trials
        if stop_flag.as_ref().is_some_and(|f| f.load(Ordering::SeqCst)) {
            eprintln!("INFO: stopping trial loop (Ctrl-C received)");
            break;
        }
        let token_key = (trial.token.config_id, trial.token.rung, trial.token.bracket);
        if let Some(existing) = completed.get(&token_key) {
            if matches!(existing.status, TrialStatus::Ok | TrialStatus::Error) {
                let scores = match existing.status {
                    TrialStatus::Ok => existing.score.clone(),
                    TrialStatus::Error => None,
                    _ => unreachable!(),
                };
                scheduler.record_result(trial.token, scores.unwrap_or_default());
                continue;
            }
            // If it's running, we want to resume it, so we don't skip.
            // We'll use its trial_id.
            if existing.status == TrialStatus::Running {
                retry_trial_ids.insert(token_key, existing.trial_id);
            }
        }

        let trial_id = retry_trial_ids.get(&token_key).cloned();
        let mut outcome =
            objective.eval_vector_with_overrides_retryable(&trial.coords, &trial.overrides, trial_id);
        match outcome {
            Ok((scores, finished_trial_id)) => {
                scheduler.record_result(trial.token, scores.clone());
                if let Some(front) = pareto_front.as_deref_mut() {
                    let before = front.len();
                    let removed = front.update(finished_trial_id, scores.clone());
                    if front.len() != before || !removed.is_empty() {
                        eprintln!("frontier: {} non-dominated trials", front.len());
                    }
                }
                retry_trial_ids.remove(&token_key);
                duplicate_retries = 0;
                completed.insert(
                    token_key,
                    CompletedTrial {
                        status: TrialStatus::Ok,
                        score: Some(scores),
                        trial_id: finished_trial_id,
                    },
                );
            }
            Err((err, _id)) => {
                let mut err = err;
                let is_duplicate = err.starts_with(DUPLICATE_CONFIG_PREFIX);
                if is_duplicate {
                    // Once a duplicate is detected, keep a shared pool of unused discrete configs.
                    // Future duplicates will retry once with the next unused config from the pool.
                    if discrete_pool.is_none() {
                        discrete_pool =
                            DiscreteConfigPool::new(objective.space(), objective.store())?;
                    }
                    if let Some(pool) = discrete_pool.as_mut()
                        && let Some(values) = pool.assign_values(trial.token.config_id)
                    {
                        let mut overrides = trial.overrides.clone();
                        pool.apply_to_overrides(&values, &mut overrides);
                        outcome = objective.eval_vector_with_overrides_retryable(
                            &trial.coords,
                            &overrides,
                            Some(_id),
                        );
                        if let Ok((scores, finished_trial_id)) = outcome {
                            scheduler.record_result(trial.token, scores.clone());
                            if let Some(front) = pareto_front.as_deref_mut() {
                                let before = front.len();
                                let removed = front.update(finished_trial_id, scores.clone());
                                if front.len() != before || !removed.is_empty() {
                                    eprintln!("frontier: {} non-dominated trials", front.len());
                                }
                            }
                            retry_trial_ids.remove(&token_key);
                            duplicate_retries = 0;
                            completed.insert(
                                token_key,
                                CompletedTrial {
                                    status: TrialStatus::Ok,
                                    score: Some(scores),
                                    trial_id: finished_trial_id,
                                },
                            );
                            continue;
                        }
                        if let Err((next_err, _)) = outcome {
                            err = next_err;
                        }
                    }
                    duplicate_retries += 1;
                    if duplicate_retries >= MAX_DUPLICATE_RETRIES {
                        return Err(format!(
                            "unable to find a unique config after {} retries; search space may be exhausted or too small for n_trials",
                            MAX_DUPLICATE_RETRIES
                        )
                        .into());
                    }
                }
                if err.starts_with(INVALID_CONFIG_PREFIX) && scheduler.retry_trial(trial.token) {
                    eprintln!("trial invalid: {err}; retrying");
                    if let Some(pool) = discrete_pool.as_mut() {
                        pool.forget_config(trial.token.config_id);
                    }
                    retry_trial_ids.remove(&token_key);
                    completed.remove(&token_key);
                    continue;
                }
                completed.insert(
                    token_key,
                    CompletedTrial {
                        status: TrialStatus::Error,
                        score: None,
                        trial_id: _id,
                    },
                );
                eprintln!("trial error: {err}");
                scheduler.record_result(trial.token, Vec::new());
                retry_trial_ids.remove(&token_key);
            }
        }
    }
    Ok(())
}

fn load_completed_trials(
    store: &TrialStore,
    objective_names: &[String],
) -> Result<CompletedTrialMap, Box<dyn Error>> {
    let rows = store.load_rows()?;
    let mut completed: CompletedTrialMap = BTreeMap::new();
    for row in rows {
        let status = match row.get(FIELD_TRIAL_STATUS).map(|v| v.as_str()) {
            Some("ok") => TrialStatus::Ok,
            Some("error") => TrialStatus::Error,
            Some("running") => TrialStatus::Running,
            Some(_) | None => continue,
        };
        let config_id = parse_usize_field(&row, &[FIELD_TRIAL_CONFIG_ID, "config_id"]);
        let rung = parse_usize_field(&row, &[FIELD_TRIAL_RUNG, "rung"]).unwrap_or(0);
        let bracket = parse_usize_field(&row, &[FIELD_TRIAL_BRACKET, "bracket"]).unwrap_or(0);
        let trial_id = parse_usize_field(&row, &[FIELD_TRIAL_ID]).unwrap_or(0);
        let Some(config_id) = config_id else {
            continue;
        };
        let score = load_trial_scores(&row, objective_names);
        completed.insert(
            (config_id, rung, bracket),
            CompletedTrial {
                status,
                score,
                trial_id,
            },
        );
    }
    Ok(completed)
}

/// Reconstruct a trial's normalized score vector from stored fields, in the
/// configured `objective_names` order. Missing `score.<name>` entries are
/// treated as worst (`INFINITY`); a row with none of them falls back to the
/// legacy single-objective `score` column.
fn load_trial_scores(row: &BTreeMap<String, String>, objective_names: &[String]) -> Option<Vec<f64>> {
    let mut scores = Vec::with_capacity(objective_names.len());
    let mut found = false;
    for name in objective_names {
        if let Some(value) = row
            .get(&format!("score.{name}"))
            .and_then(|v| v.parse::<f64>().ok())
        {
            scores.push(value);
            found = true;
        } else {
            scores.push(f64::INFINITY);
        }
    }
    if found {
        Some(scores)
    } else {
        row.get(FIELD_SCORE)
            .and_then(|v| v.parse::<f64>().ok())
            .map(|value| vec![value])
    }
}

fn parse_usize_field(row: &BTreeMap<String, String>, keys: &[&str]) -> Option<usize> {
    for key in keys {
        if let Some(value) = row.get(*key)
            && let Ok(parsed) = value.parse::<usize>()
        {
            return Some(parsed);
        }
    }
    None
}

fn collect_used_configs(
    store: &TrialStore,
    params: &[String],
    conditional: &HashSet<String>,
) -> Result<HashSet<Vec<String>>, Box<dyn Error>> {
    let rows = store.load_rows()?;
    let mut used = HashSet::new();
    for row in rows {
        let mut config = Vec::with_capacity(params.len());
        let mut missing = false;
        for name in params {
            let key = format!("{HP_PREFIX}{name}");
            match row.get(&key) {
                Some(value) => config.push(value.clone()),
                None => {
                    if conditional.contains(name) {
                        // Inactive params are omitted from stored hp.* fields.
                        config.push(String::new());
                    } else {
                        missing = true;
                        break;
                    }
                }
            }
        }
        if !missing {
            used.insert(config);
        }
    }
    Ok(used)
}

fn build_available_configs(
    space: &SearchSpace,
    used: &HashSet<Vec<String>>,
    current: &mut Vec<String>,
    available: &mut Vec<Vec<String>>,
    sampled: &mut HashMap<String, String>,
) {
    let idx = current.len();
    if idx == space.params.len() {
        if !used.contains(current) {
            available.push(current.clone());
        }
        return;
    }
    let spec = &space.params[idx];
    let active = space.param_active(spec, sampled);
    let values: Vec<String> = if active {
        spec.discrete_values().unwrap_or_default()
    } else {
        // Inactive conditional param: canonical empty value so it is neither
        // forced into overrides nor required in stored rows.
        vec![String::new()]
    };
    for value in values {
        current.push(value.clone());
        if active {
            sampled.insert(spec.name().to_string(), value.clone());
        }
        build_available_configs(space, used, current, available, sampled);
        if active {
            sampled.remove(spec.name());
        }
        current.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CommandTemplate, ParamSpec, TRIALS_CSV_FILENAME, TrialRecord};

    #[test]
    fn discrete_pool_returns_remaining_config() {
        let space = SearchSpace {
            params: vec![ParamSpec::Int {
                name: "x".to_string(),
                min: 0,
                max: 99,
                step: None,
                parent: None,
                parent_values: None,
            }],
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let template = CommandTemplate::new("".to_string());
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template);
        for i in 0..99 {
            let mut fields = BTreeMap::new();
            fields.insert(format!("{HP_PREFIX}x"), i.to_string());
            store
                .append(&TrialRecord {
                    trial_id: i as usize,
                    status: TrialStatus::Ok,
                    elapsed_ms: 0,
                    error: None,
                    fields,
                })
                .expect("append");
        }

        let pool = DiscreteConfigPool::new(&space, &store)
            .expect("pool")
            .expect("pool present");
        let mut pool = pool;
        let values = pool.assign_values(0).expect("unused");
        assert_eq!(values, vec!["99".to_string()]);
    }

    #[test]
    fn discrete_pool_skips_parent_invalid_combos() {
        let space = SearchSpace {
            params: vec![
                crate::ParamSpec::Choice {
                    name: "opt".to_string(),
                    values: vec!["sgd".to_string(), "adam".to_string()],
                    parent: None,
                    parent_values: None,
                },
                crate::ParamSpec::Bool {
                    name: "momentum".to_string(),
                    parent: Some("opt".to_string()),
                    parent_values: Some(vec!["sgd".to_string()]),
                },
            ],
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let template = CommandTemplate::new("".to_string());
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template);
        let mut pool = DiscreteConfigPool::new(&space, &store)
            .expect("pool")
            .expect("pool present");
        let mut combos = Vec::new();
        while let Some(values) = pool.assign_values(combos.len()) {
            combos.push(values);
        }
        // sgd→momentum {false,true}; adam→momentum inactive (empty sentinel).
        assert_eq!(combos.len(), 3, "combos: {combos:?}");
        let adam_combo = combos
            .iter()
            .find(|c| c[0] == "adam")
            .expect("adam combo present");
        assert!(adam_combo[1].is_empty(), "adam leaves momentum omitted");
        let mut sgd_combos: Vec<String> = combos
            .iter()
            .filter(|c| c[0] == "sgd")
            .map(|c| c[1].clone())
            .collect();
        sgd_combos.sort();
        assert_eq!(sgd_combos, vec!["false".to_string(), "true".to_string()]);
    }

    #[test]
    fn pareto_driver_keeps_only_non_dominated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(TRIALS_CSV_FILENAME);
        let template =
            CommandTemplate::new(crate::test_support::bin_command("mock_emit_two_metrics"));
        let store = TrialStore::new(path.clone(), template.clone());
        // A unique-per-trial param keeps configs distinct (metrics still come
        // from the mock's injected trial id).
        let space = SearchSpace {
            params: vec![crate::ParamSpec::Int {
                name: "dummy".to_string(),
                min: 0,
                max: 100_000,
                step: None,
                parent: None,
                parent_values: None,
            }],
        };
        let objectives = vec![
            crate::Objective {
                name: "loss".to_string(),
                goal: crate::Goal::Min,
                primary: true,
            },
            crate::Objective {
                name: "latency_ms".to_string(),
                goal: crate::Goal::Min,
                primary: false,
            },
        ];
        let objective = crate::command::CommandObjective::new(
            store,
            template.clone(),
            space,
            dir.path().join("artifacts"),
            "loss".to_string(),
            crate::Goal::Min,
            true,
            0,
        )
        .with_objectives(objectives);
        let scheduler = crate::scheduler::FixedScheduler::new(1, 3, None, None);
        crate::sampler::run_pareto(objective, Box::new(scheduler), None).expect("run_pareto");

        // Recompute the non-dominated front from the stored raw metrics.
        // mock_emit_two_metrics yields trial 0=(1,3), trial 1=(2,1),
        // trial 2=(3,4); trial 2 is dominated by trial 0.
        let check_store = TrialStore::new(path, template);
        let rows = check_store.load_rows().expect("rows");
        let mut trials: Vec<(usize, Vec<f64>)> = Vec::new();
        for row in &rows {
            if row.get(FIELD_TRIAL_STATUS).and_then(|s| s.parse::<TrialStatus>().ok())
                != Some(TrialStatus::Ok)
            {
                continue;
            }
            let id = row
                .get(FIELD_TRIAL_ID)
                .and_then(|v| v.parse::<usize>().ok())
                .expect("trial id");
            let loss = row
                .get("metric.loss")
                .and_then(|v| v.parse::<f64>().ok())
                .expect("loss");
            let latency = row
                .get("metric.latency_ms")
                .and_then(|v| v.parse::<f64>().ok())
                .expect("latency_ms");
            trials.push((id, vec![loss, latency]));
        }
        assert_eq!(trials.len(), 3);
        let normalized: Vec<Vec<f64>> = trials.iter().map(|(_, scores)| scores.clone()).collect();
        let fronts = crate::sampler::pareto::fast_nondominated_sort(&normalized);
        let front_ids: Vec<usize> = fronts[0].iter().map(|&i| trials[i].0).collect();
        assert_eq!(front_ids, vec![0, 1]);
        assert!(!front_ids.contains(&2), "dominated trial must be excluded");
    }
}

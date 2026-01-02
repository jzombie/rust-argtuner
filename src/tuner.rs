use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::sync::Arc;

use argmin::core::{CostFunction, Executor};
use argmin::solver::particleswarm::ParticleSwarm;

use crate::analysis::{print_hparam_impact, print_top_trials};
use crate::command::CommandObjective;
use crate::command::CommandTemplate;
use crate::constants::{
    FIELD_SCORE, FIELD_TRIAL_BRACKET, FIELD_TRIAL_CONFIG_ID, FIELD_TRIAL_ID, FIELD_TRIAL_RUNG,
    FIELD_TRIAL_STATUS, PLACEHOLDER_TRIAL_DIR, PLACEHOLDER_TRIAL_ID,
};
use crate::project::{Project, Sampler};
use crate::scheduler::Scheduler;
use crate::scheduler::{SchedulerBinding, TrialScheduler};
use crate::store::{TrialStatus, TrialStore};

pub struct Tuner {
    project: Project,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    pub dry_run: bool,
}

impl Tuner {
    pub fn new(project: Project) -> Self {
        Self { project }
    }

    pub fn run(&self) -> Result<(), Box<dyn Error>> {
        self.run_with_options(RunOptions::default())
    }

    pub fn run_with_options(&self, options: RunOptions) -> Result<(), Box<dyn Error>> {
        let config = self.project.load_config()?;
        let mut temp_root = None;
        if options.dry_run {
            let temp = tempfile::tempdir().map_err(|err| -> Box<dyn Error> {
                format!("failed to create dry-run tempdir: {err}").into()
            })?;
            temp_root = Some(temp);
        } else {
            self.project.ensure_dirs()?;
            let _lock = self
                .project
                .acquire_lock()
                .map_err(|err| -> Box<dyn Error> {
                    format!("failed to acquire project lock: {err}").into()
                })?;
        }
        let template = self.project.read_template()?;
        let checkpoint_arg = config
            .checkpoint_arg
            .as_deref()
            .unwrap_or("--checkpoint-dir");
        if !template_has_checkpoint_dir(&template, checkpoint_arg) {
            return Err(format!("template must include {checkpoint_arg} {{trial_dir}}").into());
        }
        let template_placeholders = template.placeholders().unwrap_or_default();

        let space = self.project.read_space()?;
        space
            .validate_specs()
            .map_err(|err| -> Box<dyn Error> { err.into() })?;

        // Validation
        let space_params: Vec<_> = space.params.iter().map(|p| p.name()).collect();
        let scheduler_binding = SchedulerBinding::new(&config);
        for p in &template_placeholders {
            if !space_params.contains(&p.as_str())
                && p != PLACEHOLDER_TRIAL_ID
                && p != PLACEHOLDER_TRIAL_DIR
                && !scheduler_binding.allows_placeholder(p)
            {
                return Err(
                    format!("template placeholder {{{}}} not found in search space", p).into(),
                );
            }
        }
        for param in &space_params {
            if !template_placeholders.contains(&param.to_string()) {
                eprintln!(
                    "Warning: parameter '{}' defined in search space but not used in template",
                    param
                );
            }
        }

        let store = if let Some(temp_root) = temp_root.as_ref() {
            let trials_path = temp_root.path().join(crate::TRIALS_CSV_FILENAME);
            TrialStore::new(trials_path, template.clone())
        } else {
            self.project.store()?
        };
        let store_for_summary = store.clone();
        let next_id = store.next_trial_id()?;
        let objective = CommandObjective::new(
            store,
            template,
            space,
            if let Some(temp_root) = temp_root.as_ref() {
                let artifacts = temp_root.path().join("artifacts");
                std::fs::create_dir_all(&artifacts)?;
                artifacts
            } else {
                self.project.artifacts_dir()
            },
            config.metric_key.clone(),
            config.goal,
            config.inject_trial_placeholders,
            next_id,
        );
        match config.sampler.kind {
            Sampler::Pso => {
                if config.scheduler.kind != Scheduler::Fixed {
                    return Err("scheduler must be fixed when using the pso sampler".into());
                }
                run_optimizer(
                    objective,
                    config.sampler.pso.iters,
                    config.sampler.pso.particles,
                )?;
            }
            Sampler::Random => {
                scheduler_binding
                    .validate_template(&template_placeholders)
                    .map_err(|err| -> Box<dyn Error> { err.into() })?;
                let scheduler: Box<dyn TrialScheduler> = scheduler_binding.build(objective.dims());
                let completed = load_completed_trials(objective.store())?;
                run_scheduled(objective, scheduler, completed)?;
            }
        }

        print_top_trials(&store_for_summary, 10);
        print_hparam_impact(&store_for_summary, config.goal, &config.metric_key);
        Ok(())
    }
}

fn template_has_checkpoint_dir(template: &CommandTemplate, checkpoint_arg: &str) -> bool {
    let text = template.as_str();
    if let Ok(tokens) = shell_words::split(text) {
        return tokens_have_checkpoint_dir(&tokens, checkpoint_arg);
    }
    let has_flag = text.contains(checkpoint_arg);
    has_flag && text.contains("{trial_dir}")
}

fn tokens_have_checkpoint_dir(tokens: &[String], checkpoint_arg: &str) -> bool {
    for (idx, token) in tokens.iter().enumerate() {
        let arg_eq = format!("{checkpoint_arg}=");
        if let Some(value) = token.strip_prefix(&arg_eq)
            && value.contains("{trial_dir}")
        {
            return true;
        }
        if token == checkpoint_arg
            && let Some(next) = tokens.get(idx + 1)
            && next.contains("{trial_dir}")
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{template_has_checkpoint_dir, tokens_have_checkpoint_dir};
    use crate::command::CommandTemplate;

    #[test]
    fn tokens_detect_checkpoint_dir() {
        assert!(tokens_have_checkpoint_dir(
            &["--checkpoint-dir".to_string(), "{trial_dir}".to_string()],
            "--checkpoint-dir"
        ));
        assert!(tokens_have_checkpoint_dir(
            &["--checkpoint_dir={trial_dir}".to_string()],
            "--checkpoint_dir"
        ));
        assert!(tokens_have_checkpoint_dir(
            &[
                "--checkpoint-dir".to_string(),
                "{trial_dir}/sub".to_string()
            ],
            "--checkpoint-dir"
        ));
        assert!(!tokens_have_checkpoint_dir(
            &["--checkpoint-dir".to_string(), "out".to_string()],
            "--checkpoint-dir"
        ));
    }

    #[test]
    fn template_detects_checkpoint_dir() {
        let template = CommandTemplate::new("run --checkpoint-dir {trial_dir}".to_string());
        assert!(template_has_checkpoint_dir(&template, "--checkpoint-dir"));
        let template = CommandTemplate::new("run --checkpoint_dir={trial_dir}/x".to_string());
        assert!(template_has_checkpoint_dir(&template, "--checkpoint_dir"));
        let template = CommandTemplate::new("run --checkpoint-dir out".to_string());
        assert!(!template_has_checkpoint_dir(&template, "--checkpoint-dir"));
    }
}

fn run_optimizer(
    objective: CommandObjective,
    iters: usize,
    particles: usize,
) -> Result<(), Box<dyn Error>> {
    #[derive(Clone)]
    struct ObjWrapper(Arc<CommandObjective>);

    impl CostFunction for ObjWrapper {
        type Param = Vec<f64>;
        type Output = f64;

        fn cost(&self, param: &Self::Param) -> Result<Self::Output, argmin::core::Error> {
            self.0.eval(param).map_err(argmin::core::Error::msg)
        }
    }

    let dims = objective.dims();
    let lower = vec![0.0; dims];
    let upper = vec![1.0; dims];
    let solver = ParticleSwarm::new((lower, upper), particles);
    let _res = Executor::new(ObjWrapper(Arc::new(objective)), solver)
        .configure(|state| state.max_iters(iters as u64))
        .run()?;
    Ok(())
}

#[derive(Clone, Copy)]
struct CompletedTrial {
    status: TrialStatus,
    score: Option<f64>,
    trial_id: usize,
}

type TrialKey = (usize, usize, usize);
type CompletedTrialMap = BTreeMap<TrialKey, CompletedTrial>;

fn run_scheduled(
    objective: CommandObjective,
    mut scheduler: Box<dyn TrialScheduler>,
    mut completed: CompletedTrialMap,
) -> Result<(), Box<dyn Error>> {
    let mut retry_trial_ids: HashMap<TrialKey, usize> = HashMap::new();
    while let Some(trial) = scheduler.next_trial() {
        let token_key = (trial.token.config_id, trial.token.rung, trial.token.bracket);
        if let Some(existing) = completed.get(&token_key) {
            if matches!(existing.status, TrialStatus::Ok | TrialStatus::Error) {
                let score = match existing.status {
                    TrialStatus::Ok => existing.score,
                    TrialStatus::Error => None,
                    _ => unreachable!(),
                };
                scheduler.record_result(trial.token, score);
                continue;
            }
            // If it's running, we want to resume it, so we don't skip.
            // We'll use its trial_id.
            if existing.status == TrialStatus::Running {
                retry_trial_ids.insert(token_key, existing.trial_id);
            }
        }

        let trial_id = retry_trial_ids.get(&token_key).cloned();
        let outcome =
            objective.eval_with_overrides_retryable(&trial.coords, &trial.overrides, trial_id);
        match outcome {
            Ok((score, finished_trial_id)) => {
                scheduler.record_result(trial.token, Some(score));
                retry_trial_ids.remove(&token_key);
                completed.insert(
                    token_key,
                    CompletedTrial {
                        status: TrialStatus::Ok,
                        score: Some(score),
                        trial_id: finished_trial_id,
                    },
                );
            }
            Err((err, _id)) => {
                if err.starts_with("invalid_config:") && scheduler.retry_trial(trial.token) {
                    eprintln!("trial invalid: {err}; retrying");
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
                scheduler.record_result(trial.token, None);
                retry_trial_ids.remove(&token_key);
            }
        }
    }
    Ok(())
}

fn load_completed_trials(store: &TrialStore) -> Result<CompletedTrialMap, Box<dyn Error>> {
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
        let score = row.get(FIELD_SCORE).and_then(|v| v.parse::<f64>().ok());
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

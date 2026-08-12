use std::error::Error;

use crate::analysis::{print_hparam_impact, print_top_trials};
use crate::checkpoint::{ControllableObjective, StopFlag, sweep_stale_running_trials};
use crate::command::CommandObjective;
use crate::project::{Project, Sampler};
use crate::sampler::{run_pso, run_random};
use crate::scheduler::Scheduler;
use crate::scheduler::{SchedulerBinding, TrialScheduler};
use crate::trial::store::{StepPublisher, TrialStore};
use crate::validate::validate_project_config;

pub struct Tuner {
    project: Project,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    pub dry_run: bool,
    pub allow_config_change: bool,
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
        let config_text = self.project.read_unified_config_text()?;
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
        let template_placeholders = template.placeholders().unwrap_or_default();
        let space = self.project.read_space()?;
        validate_project_config(&config, &template, &space, &template_placeholders)
            .map_err(|err| -> Box<dyn Error> { err.into() })?;

        let scheduler_binding = SchedulerBinding::new(&config);
        let mut store = if let Some(temp_root) = temp_root.as_ref() {
            let trials_path = temp_root.path().join(crate::TRIALS_CSV_FILENAME);
            TrialStore::new(trials_path, template.clone())
        } else {
            self.project.store()?
        };
        if temp_root.is_none() {
            store.ensure_project_config(&config_text, options.allow_config_change)?;
        }
        // Start step publisher for real-time TUI communication
        if let Some((publisher, port)) = StepPublisher::bind(
            argtuner_common::STEP_PUBLISHER_PORT,
            store.step_cache_handle(),
        ) {
            eprintln!("step publisher listening on port {port}");
            store = store.with_step_publisher(publisher);
        }
        let store_for_summary = store.clone();
        let next_id = store.next_trial_id()?;
        // Register Ctrl-C handler for graceful shutdown across all samplers
        let stop_flag = StopFlag::new();
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
        )
        .with_runner_options(
            if config.scheduler.trial_timeout_s > 0 {
                Some(std::time::Duration::from_secs(
                    config.scheduler.trial_timeout_s,
                ))
            } else {
                None
            },
            Some(stop_flag.inner()),
        );

        // Sweep stale Running trials from a prior interrupted run so that
        // PSO's duplicate check and SHA's artifact copy behave correctly.
        if temp_root.is_none()
            && let Err(e) =
                sweep_stale_running_trials(&store_for_summary, &self.project.artifacts_dir())
        {
            eprintln!("WARN: stale trial sweep failed (continuing): {e}");
        }

        match config.sampler.kind {
            Sampler::Pso => {
                if config.scheduler.kind != Scheduler::Fixed {
                    return Err("scheduler must be fixed when using the pso sampler".into());
                }
                let ctrl = ControllableObjective::new(objective, stop_flag.inner());
                run_pso(ctrl, config.sampler.pso.iters, config.sampler.pso.particles)?;
            }
            Sampler::Random => {
                let scheduler: Box<dyn TrialScheduler> = scheduler_binding.build(objective.dims());
                run_random(objective, scheduler, Some(stop_flag.inner()))?;
            }
        }

        print_top_trials(&store_for_summary, 10);
        print_hparam_impact(&store_for_summary, config.goal, &config.metric_key);
        Ok(())
    }
}

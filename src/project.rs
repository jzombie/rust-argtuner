use crate::constants::{ENV_TRIAL_DIR, ENV_TRIAL_ID, FIELD_METRIC, TRIALS_CSV_FILENAME};
use crate::scheduler::Scheduler;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::command::CommandTemplate;
use crate::lock::{ProjectLock, lock_path_for_config};
use crate::space::SearchSpace;
use crate::store::TrialStore;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedConfig {
    pub project: ProjectSettings,
    pub sampler: SamplerConfig,
    pub scheduler: SchedulerConfig,
    pub space: SearchSpace,
    pub template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSettings {
    #[serde(default = "default_metric_key")]
    pub metric_key: String,
    #[serde(default = "default_goal")]
    pub goal: Goal,
    #[serde(default)]
    pub pruner: Pruner,
    #[serde(default = "default_inject_trial_placeholders")]
    pub inject_trial_placeholders: bool,
    #[serde(default)]
    pub checkpoint_arg: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectConfig {
    pub metric_key: String,
    pub goal: Goal,
    pub sampler: SamplerConfig,
    pub scheduler: SchedulerConfig,
    pub pruner: Pruner,
    pub inject_trial_placeholders: bool,
    pub checkpoint_arg: Option<String>,
}

impl ProjectConfig {
    fn from_sections(
        project: ProjectSettings,
        sampler: SamplerConfig,
        scheduler: SchedulerConfig,
    ) -> Self {
        Self {
            metric_key: project.metric_key,
            goal: project.goal,
            sampler,
            scheduler,
            pruner: project.pruner,
            inject_trial_placeholders: project.inject_trial_placeholders,
            checkpoint_arg: project.checkpoint_arg,
        }
    }
}

pub fn format_injected_env(trial_id: usize, trial_dir: &Path) -> String {
    format!(
        "{ENV_TRIAL_ID}={trial_id} {ENV_TRIAL_DIR}={}",
        trial_dir.display()
    )
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Goal {
    Min,
    Max,
}

impl Goal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Goal::Min => "min",
            Goal::Max => "max",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sampler {
    Pso,
    Random,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Pruner {
    #[default]
    None,
}

fn default_metric_key() -> String {
    FIELD_METRIC.to_string()
}

fn default_goal() -> Goal {
    Goal::Min
}

fn default_sampler() -> Sampler {
    Sampler::Pso
}

fn default_scheduler() -> Scheduler {
    Scheduler::Fixed
}

fn default_n_trials() -> usize {
    0
}

fn default_seed() -> u64 {
    42
}

fn default_pso_iters() -> usize {
    10
}

fn default_pso_particles() -> usize {
    5
}

fn default_budget_placeholder() -> String {
    "epochs".to_string()
}

fn default_halving_min_epochs() -> usize {
    1
}

fn default_halving_max_epochs() -> usize {
    10
}

fn default_halving_eta() -> usize {
    3
}

fn default_inject_trial_placeholders() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FixedSchedulerConfig {}

impl FixedSchedulerConfig {
    pub fn is_empty(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SuccessiveHalvingSchedulerConfig {
    #[serde(default = "default_budget_placeholder")]
    pub budget_placeholder: String,
    #[serde(default = "default_halving_min_epochs")]
    pub min_epochs: usize,
    #[serde(default = "default_halving_max_epochs")]
    pub max_epochs: usize,
    #[serde(default = "default_halving_eta")]
    pub eta: usize,
}

impl Default for SuccessiveHalvingSchedulerConfig {
    fn default() -> Self {
        Self {
            budget_placeholder: default_budget_placeholder(),
            min_epochs: default_halving_min_epochs(),
            max_epochs: default_halving_max_epochs(),
            eta: default_halving_eta(),
        }
    }
}

impl SuccessiveHalvingSchedulerConfig {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplerConfig {
    #[serde(default = "default_sampler", rename = "type")]
    pub kind: Sampler,
    #[serde(default, skip_serializing_if = "RandomSamplerConfig::is_empty")]
    pub random: RandomSamplerConfig,
    #[serde(default, skip_serializing_if = "PsoSamplerConfig::is_default")]
    pub pso: PsoSamplerConfig,
}

impl Default for SamplerConfig {
    fn default() -> Self {
        Self {
            kind: default_sampler(),
            random: RandomSamplerConfig::default(),
            pso: PsoSamplerConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RandomSamplerConfig {}

impl RandomSamplerConfig {
    pub fn is_empty(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PsoSamplerConfig {
    #[serde(default = "default_pso_iters")]
    pub iters: usize,
    #[serde(default = "default_pso_particles")]
    pub particles: usize,
}

impl Default for PsoSamplerConfig {
    fn default() -> Self {
        Self {
            iters: default_pso_iters(),
            particles: default_pso_particles(),
        }
    }
}

impl PsoSamplerConfig {
    pub fn is_default(&self) -> bool {
        self.iters == default_pso_iters() && self.particles == default_pso_particles()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    #[serde(default = "default_scheduler", rename = "type")]
    pub kind: Scheduler,
    #[serde(default = "default_n_trials")]
    pub n_trials: usize,
    #[serde(default = "default_seed")]
    pub seed: u64,
    #[serde(default, skip_serializing_if = "FixedSchedulerConfig::is_empty")]
    pub fixed: FixedSchedulerConfig,
    #[serde(
        default,
        rename = "successive_halving",
        skip_serializing_if = "SuccessiveHalvingSchedulerConfig::is_default"
    )]
    pub successive_halving: SuccessiveHalvingSchedulerConfig,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            kind: default_scheduler(),
            n_trials: default_n_trials(),
            seed: default_seed(),
            fixed: FixedSchedulerConfig::default(),
            successive_halving: SuccessiveHalvingSchedulerConfig::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Project {
    root: PathBuf,
}

impl Project {
    pub fn new<P: AsRef<Path>>(root: P) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.root())?;
        std::fs::create_dir_all(self.artifacts_dir())?;
        Ok(())
    }

    pub fn unified_config_path(&self) -> PathBuf {
        self.root.join("argtuner.toml")
    }

    pub fn acquire_lock(&self) -> std::io::Result<ProjectLock> {
        let config_path = self.unified_config_path();
        let lock_path = lock_path_for_config(&config_path);
        ProjectLock::acquire(lock_path, &config_path)
    }

    pub fn trials_path(&self) -> PathBuf {
        self.root.join(TRIALS_CSV_FILENAME)
    }

    pub fn trials_db_path(&self) -> PathBuf {
        self.trials_path().with_extension("sqlite")
    }

    pub fn artifacts_dir(&self) -> PathBuf {
        self.root.join("artifacts")
    }

    pub fn store(&self) -> std::io::Result<TrialStore> {
        let template = self.read_template()?;
        Ok(TrialStore::new(self.trials_path(), template))
    }

    pub fn load_config(&self) -> std::io::Result<ProjectConfig> {
        let data = std::fs::read_to_string(self.unified_config_path())?;
        let config: UnifiedConfig = toml::from_str(&data)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        Ok(ProjectConfig::from_sections(
            config.project,
            config.sampler,
            config.scheduler,
        ))
    }

    pub fn save_unified_config(&self, config: &UnifiedConfig) -> std::io::Result<()> {
        let data = toml::to_string_pretty(config)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        std::fs::write(self.unified_config_path(), data)?;
        Ok(())
    }

    pub fn read_template(&self) -> std::io::Result<CommandTemplate> {
        let data = std::fs::read_to_string(self.unified_config_path())?;
        let config: UnifiedConfig = toml::from_str(&data)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        Ok(CommandTemplate::new(config.template))
    }

    pub fn read_space(&self) -> std::io::Result<SearchSpace> {
        let data = std::fs::read_to_string(self.unified_config_path())?;
        let config: UnifiedConfig = toml::from_str(&data)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        Ok(config.space)
    }
}

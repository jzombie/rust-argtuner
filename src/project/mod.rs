pub mod config;

use std::path::{Path, PathBuf};

use crate::command::CommandTemplate;
use crate::constants::{CONFIG_FILENAME, ENV_TRIAL_DIR, ENV_TRIAL_ID, TRIALS_CSV_FILENAME};
use crate::lock::{ProjectLock, lock_path_for_config};
use crate::search_space::SearchSpace;
use crate::trial::store::TrialStore;

pub use config::{
    FixedSchedulerConfig, Goal, ProjectConfig, ProjectSettings, Pruner, PsoSamplerConfig,
    RandomSamplerConfig, Sampler, SamplerConfig, SchedulerConfig, SuccessiveHalvingSchedulerConfig,
    UnifiedConfig,
};

pub fn format_injected_env(trial_id: usize, trial_dir: &Path) -> String {
    format!(
        "{ENV_TRIAL_ID}={trial_id} {ENV_TRIAL_DIR}={}",
        trial_dir.display()
    )
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
        self.root.join(CONFIG_FILENAME)
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
        let config = self.load_unified_config().map_err(|err| {
            let mut message = err.to_string();
            if message.contains("unknown field `budget_placeholder`") {
                message.push_str(
                    "; note: budget_placeholder belongs under [scheduler.successive_halving]",
                );
            }
            std::io::Error::new(std::io::ErrorKind::InvalidData, message)
        })?;
        Ok(ProjectConfig::from_sections(
            config.project,
            config.sampler,
            config.scheduler,
        ))
    }

    pub fn read_unified_config_text(&self) -> std::io::Result<String> {
        std::fs::read_to_string(self.unified_config_path())
    }

    /// Parse the project's `argtuner.toml` into its full deserialized struct
    /// form (`template` + `[project]` + `[sampler]` + `[scheduler]` + `[space]`).
    pub fn load_unified_config(&self) -> std::io::Result<UnifiedConfig> {
        let data = std::fs::read_to_string(self.unified_config_path())?;
        toml::from_str(&data).map_err(|err| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string())
        })
    }

    pub fn save_unified_config(&self, config: &UnifiedConfig) -> std::io::Result<()> {
        let data = toml::to_string_pretty(config)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
        std::fs::write(self.unified_config_path(), data)?;
        Ok(())
    }

    pub fn read_template(&self) -> std::io::Result<CommandTemplate> {
        let config = self.load_unified_config()?;
        Ok(CommandTemplate::new(config.template))
    }

    pub fn read_space(&self) -> std::io::Result<SearchSpace> {
        let config = self.load_unified_config()?;
        Ok(config.space)
    }
}

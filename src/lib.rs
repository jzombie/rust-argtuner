pub mod analysis;
pub mod command;
pub mod constants;
pub mod db;
pub mod lock;
pub mod project;
pub mod sampler;
pub mod scheduler;
pub mod space;
pub mod store;
pub mod trial;
pub mod tuner;
pub mod utils;
pub mod validate;
pub use constants::*;
pub mod workspace;
pub use workspace::workspace_root;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use crate::command::template::{CommandTemplate, TemplateError};
pub use project::{
    FixedSchedulerConfig, Goal, Project, ProjectConfig, ProjectSettings, Pruner, Sampler,
    SamplerConfig, SchedulerConfig, SuccessiveHalvingSchedulerConfig, UnifiedConfig,
    format_injected_env,
};
pub use scheduler::{
    FixedScheduler, ScheduledTrial, Scheduler, SuccessiveHalvingScheduler, TrialScheduler,
    TrialToken,
};
pub use space::{ParamSpec, SearchSpace};
pub use store::{TrialRecord, TrialStatus, TrialStore};
pub use tuner::{RunOptions, Tuner};

#[derive(Debug, Default, Clone)]
pub struct TrialOverrides {
    pub values: BTreeMap<String, String>,
    pub fields: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
}

pub struct RenderedTrial {
    pub command: String,
    pub fields: BTreeMap<String, String>,
    pub trial_dir: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
}

pub fn render_trial_command(
    template: &CommandTemplate,
    space: &SearchSpace,
    coords: &[f64],
    trial_id: usize,
    artifacts_dir: &Path,
    inject_trial_placeholders: bool,
) -> Result<RenderedTrial, TemplateError> {
    let overrides = TrialOverrides::default();
    render_trial_command_with_overrides(
        template,
        space,
        coords,
        trial_id,
        artifacts_dir,
        inject_trial_placeholders,
        &overrides,
    )
}

pub fn render_trial_command_with_overrides(
    template: &CommandTemplate,
    space: &SearchSpace,
    coords: &[f64],
    trial_id: usize,
    artifacts_dir: &Path,
    inject_trial_placeholders: bool,
    overrides: &TrialOverrides,
) -> Result<RenderedTrial, TemplateError> {
    let mut values = space.values_from_unit(coords);
    let mut fields = space.fields_from_unit(coords);
    let mut env = BTreeMap::new();
    for (key, value) in overrides.values.iter() {
        values.insert(key.clone(), value.clone());
    }
    for (key, value) in overrides.fields.iter() {
        fields.insert(key.clone(), value.clone());
    }
    for (key, value) in overrides.env.iter() {
        env.insert(key.clone(), value.clone());
    }
    let mut trial_dir = None;
    if inject_trial_placeholders {
        // let config_id_key = format!("{TRIAL_PREFIX}config_id");
        // let bracket_key = format!("{TRIAL_PREFIX}bracket");
        // let config_id = fields
        //     .get(&config_id_key)
        //     .and_then(|value| value.parse::<usize>().ok());
        // let bracket = fields
        //     .get(&bracket_key)
        //     .and_then(|value| value.parse::<usize>().ok())
        //     .unwrap_or(0);
        let dir_value = values
            .get(crate::constants::PLACEHOLDER_TRIAL_DIR)
            .cloned()
            .unwrap_or_else(|| format!("trial_{trial_id}"));
        let dir = artifacts_dir.join(&dir_value);
        values.insert(
            crate::constants::PLACEHOLDER_TRIAL_ID.to_string(),
            trial_id.to_string(),
        );
        values
            .entry(crate::constants::PLACEHOLDER_TRIAL_DIR.to_string())
            .or_insert_with(|| dir.to_string_lossy().to_string());
        fields.insert(
            crate::constants::PLACEHOLDER_TRIAL_ID.to_string(),
            trial_id.to_string(),
        );
        fields
            .entry(crate::constants::PLACEHOLDER_TRIAL_DIR.to_string())
            .or_insert_with(|| dir.to_string_lossy().to_string());
        env.insert(
            crate::constants::ENV_TRIAL_ID.to_string(),
            trial_id.to_string(),
        );
        env.entry(crate::constants::ENV_TRIAL_DIR.to_string())
            .or_insert_with(|| dir.to_string_lossy().to_string());
        trial_dir = Some(dir);
    }
    let command = template.render(&values)?;
    Ok(RenderedTrial {
        command,
        fields,
        trial_dir,
        env,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_uses_trial_id_for_trial_dir() {
        let template = CommandTemplate::new("echo {trial_dir}".to_string());
        let space = SearchSpace { params: vec![] };
        let mut overrides = TrialOverrides::default();
        overrides
            .fields
            .insert(FIELD_TRIAL_CONFIG_ID.to_string(), "7".to_string());
        overrides
            .fields
            .insert(FIELD_TRIAL_BRACKET.to_string(), "2".to_string());
        let artifacts_dir = PathBuf::from("artifacts");
        let rendered = render_trial_command_with_overrides(
            &template,
            &space,
            &[],
            9,
            &artifacts_dir,
            true,
            &overrides,
        )
        .expect("render");
        let expected = artifacts_dir.join("trial_9").to_string_lossy().to_string();
        assert_eq!(
            rendered.env.get(ENV_TRIAL_DIR).map(String::as_str),
            Some(expected.as_str())
        );
        assert!(rendered.command.contains(&expected));
    }
}

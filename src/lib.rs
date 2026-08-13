#![doc = include_str!("../README.md")]

#[cfg(feature = "cli")]
pub mod analysis;
#[cfg(feature = "cli")]
pub mod checkpoint;
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "cli")]
pub mod command;
pub mod constants;
#[cfg(feature = "cli")]
pub mod discover;
#[cfg(feature = "cli")]
pub mod inspect;
#[cfg(feature = "cli")]
pub mod lock;
#[cfg(feature = "cli")]
pub mod project;
#[cfg(feature = "cli")]
pub mod sampler;
#[cfg(feature = "cli")]
pub mod scheduler;
#[cfg(feature = "cli")]
pub mod search_space;
#[cfg(feature = "cli")]
pub mod trial;
#[cfg(feature = "cli")]
pub mod tuner;
#[cfg(feature = "cli")]
pub mod utils;
#[cfg(feature = "cli")]
pub mod validate;
pub use constants::*;
#[cfg(feature = "cli")]
pub mod workspace;
#[cfg(feature = "cli")]
pub use workspace::workspace_root;
#[cfg(feature = "cli")]
#[doc(hidden)]
pub mod test_support;

#[cfg(feature = "cli")]
pub use discover::find_projects;

#[cfg(feature = "cli")]
pub use crate::command::template::{CommandTemplate, TemplateError};
#[cfg(feature = "cli")]
pub use project::{
    FixedSchedulerConfig, Goal, Objective, Project, ProjectConfig, ProjectSettings, Pruner,
    PsoSamplerConfig, RandomSamplerConfig, Sampler, SamplerConfig, SchedulerConfig,
    SuccessiveHalvingSchedulerConfig, UnifiedConfig, format_injected_env,
};
#[cfg(feature = "cli")]
pub use scheduler::{
    FixedScheduler, ScheduledTrial, Scheduler, SuccessiveHalvingScheduler, TrialScheduler,
    TrialToken,
};
#[cfg(feature = "cli")]
pub use search_space::{ParamSpec, SearchSpace};
#[cfg(feature = "cli")]
pub use trial::store::{TrialRecord, TrialStatus, TrialStore};
#[cfg(feature = "cli")]
pub use trial::{
    RenderedTrial, TrialOverrides, render_trial_command, render_trial_command_with_overrides,
};
#[cfg(feature = "cli")]
pub use tuner::{RunOptions, Tuner};

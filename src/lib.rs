pub mod analysis;
pub mod checkpoint;
#[cfg(feature = "cli")]
pub mod cli;
pub mod command;
pub mod constants;
pub mod discover;
pub mod inspect;
pub mod lock;
pub mod project;
pub mod sampler;
pub mod scheduler;
pub mod search_space;
pub mod trial;
pub mod tuner;
pub mod utils;
pub mod validate;
pub use constants::*;
pub mod workspace;
pub use workspace::workspace_root;
#[doc(hidden)]
pub mod test_support;

pub use discover::find_projects;

// Talkback bindings: declare your clap CLI once, get a production binary plus
// zero-touch argtuner compatibility (`argtuner::init::<P>()`,
// `argtuner::talkback_args`). Requires `clap` (derive) and `serde` (derive) as
// direct dependencies of the consuming crate.
pub use argtuner_talkback::init;
pub use argtuner_talkback::Talkback;
pub use argtuner_talkback_derive::talkback_args;

pub use crate::command::template::{CommandTemplate, TemplateError};
pub use project::{
    FixedSchedulerConfig, Goal, Project, ProjectConfig, ProjectSettings, Pruner, PsoSamplerConfig,
    RandomSamplerConfig, Sampler, SamplerConfig, SchedulerConfig, SuccessiveHalvingSchedulerConfig,
    UnifiedConfig, format_injected_env,
};
pub use scheduler::{
    FixedScheduler, ScheduledTrial, Scheduler, SuccessiveHalvingScheduler, TrialScheduler,
    TrialToken,
};
pub use search_space::{ParamSpec, SearchSpace};
pub use trial::store::{TrialRecord, TrialStatus, TrialStore};
pub use trial::{
    RenderedTrial, TrialOverrides, render_trial_command, render_trial_command_with_overrides,
};
pub use tuner::{RunOptions, Tuner};

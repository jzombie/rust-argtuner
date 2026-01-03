// Configuration
pub const CONFIG_FILENAME: &str = "argtuner.toml";
pub const MAX_DUPLICATE_RETRIES: usize = 50;
pub const INVALID_CONFIG_PREFIX: &str = "invalid_config:";
pub const DUPLICATE_CONFIG_REASON: &str = "duplicate_config";
pub const DUPLICATE_CONFIG_PREFIX: &str = "invalid_config: duplicate_config";

// Command stdout parser (for communication from trial back to tuner)
pub use argtuner_common::RESULT_PREFIX;
pub use argtuner_common::{METRIC_NAMESPACE, MODEL_NAMESPACE, TUNER_NAMESPACE};

// Field
pub const FIELD_METRIC: &str = "metric";
pub const FIELD_SCORE: &str = "score";
pub const FIELD_INVALID_CONFIG: &str = "invalid_config";
pub const HP_PREFIX: &str = "hp.";
pub const TRIAL_PREFIX: &str = "trial.";
pub const TRIALS_CSV_FILENAME: &str = "trials.csv";
pub const FIELD_TRIAL_ID: &str = "trial_id";
pub const FIELD_TRIAL_STATUS: &str = "status";
pub const FIELD_TRIAL_ELAPSED_MS: &str = "elapsed_ms";
pub const FIELD_TRIAL_ERROR: &str = "error";
pub const FIELD_TRIAL_CONFIG_ID: &str = "trial.config_id";
pub const FIELD_TRIAL_RUNG: &str = "trial.rung";
pub const FIELD_TRIAL_BRACKET: &str = "trial.bracket";
pub const FIELD_TRIAL_BUDGET_EPOCHS: &str = "trial.budget_epochs";
pub const FIELD_TRIAL_PARENT_ID: &str = "trial.parent_trial_id";
pub const FIELD_TRIAL_TIME: &str = "trial.time";
pub const FIELD_TRIAL_BUDGET_TOTAL: &str = "trial.budget_total";
pub const FIELD_TRIAL_BUDGET_STEP: &str = "trial.budget_step";
pub const FIELD_TUNING_BUDGET_TOTAL: &str = "tuning.budget_total";
pub const FIELD_TUNING_BUDGET_REMAINING: &str = "tuning.budget_remaining";

// Project
pub const PLACEHOLDER_TRIAL_ID: &str = "trial_id";
pub const PLACEHOLDER_TRIAL_DIR: &str = "trial_dir";
pub const ENV_TRIAL_ID: &str = "ARGTUNER_TRIAL_ID";
pub const ENV_TRIAL_DIR: &str = "ARGTUNER_TRIAL_DIR";

//! Shared constants for argtuner crates.

pub mod protocol;

pub use protocol::{TalkbackMessage, protocol_schema, protocol_schema_string};

/// Command stdout parser prefix for result/event messages.
pub const RESULT_PREFIX: &str = "::ARGTUNER::";

/// When present in the child envs or the argtuner process environment,
/// `CommandRunner` uses piped stdio instead of a PTY (test-only).
pub const FORCE_PIPES_ENV: &str = "ARGTUNER_FORCE_PIPES";

/// Env var the tuner always exports to trial subprocesses. The talkback binding
/// checks it to detect "running under argtuner" and suppresses stdout emission
/// when absent, so standalone runs of the same binary stay clean.
pub const TUNING_MARKER_ENV: &str = "ARGTUNER_TUNING";

/// Value set for [`TUNING_MARKER_ENV`] on every trial subprocess.
pub const TUNING_MARKER_VALUE: &str = "1";

/// Reserved template placeholder for the auto-injected per-trial id.
pub const PLACEHOLDER_TRIAL_ID: &str = "trial_id";

/// Reserved template placeholder for the auto-injected per-trial directory.
pub const PLACEHOLDER_TRIAL_DIR: &str = "trial_dir";

/// Namespaces for payload keys.
pub const METRIC_NAMESPACE: &str = "metric";
pub const MODEL_NAMESPACE: &str = "model";
pub const TUNER_NAMESPACE: &str = "tuner";

/// Event names.
pub const EARLY_STOPPED_EVENT: &str = "early_stopped";
pub const INVALID_CONFIG_EVENT: &str = "invalid_config";
pub const EPOCH_END_EVENT: &str = "epoch_end";
pub const STEP_END_EVENT: &str = "step_end";
pub const BINDING_VERSION_EVENT: &str = "binding_version";
pub const BINDING_VERSION_FIELD: &str = "version";
pub const MODEL_EARLY_STOPPED_EVENT: &str = "model.early_stopped";
pub const MODEL_INVALID_CONFIG_EVENT: &str = "model.invalid_config";
pub const MODEL_EPOCH_END_EVENT: &str = "model.epoch_end";
pub const MODEL_STEP_END_EVENT: &str = "model.step_end";
pub const TUNER_BINDING_VERSION_EVENT: &str = "tuner.binding_version";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Result,
    EarlyStopped,
    InvalidConfig,
    EpochEnd,
    StepEnd,
    TunerApiVersion,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Result => "result",
            EventKind::EarlyStopped => MODEL_EARLY_STOPPED_EVENT,
            EventKind::InvalidConfig => MODEL_INVALID_CONFIG_EVENT,
            EventKind::EpochEnd => MODEL_EPOCH_END_EVENT,
            EventKind::StepEnd => MODEL_STEP_END_EVENT,
            EventKind::TunerApiVersion => TUNER_BINDING_VERSION_EVENT,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            MODEL_EARLY_STOPPED_EVENT | EARLY_STOPPED_EVENT => Some(EventKind::EarlyStopped),
            MODEL_INVALID_CONFIG_EVENT | INVALID_CONFIG_EVENT => Some(EventKind::InvalidConfig),
            MODEL_EPOCH_END_EVENT | EPOCH_END_EVENT => Some(EventKind::EpochEnd),
            MODEL_STEP_END_EVENT | STEP_END_EVENT => Some(EventKind::StepEnd),
            TUNER_BINDING_VERSION_EVENT | BINDING_VERSION_EVENT => Some(EventKind::TunerApiVersion),
            _ => None,
        }
    }
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// TCP port for the step publisher (real-time step data to TUI).
pub const STEP_PUBLISHER_PORT: u16 = 45100;

/// Starter `argtuner.toml` skeleton, shared by all bindings. Import as a
/// string, or embed the canonical file at `crates/common/assets/starter_template.toml`
/// at build time from non-Rust bindings.
pub const STARTER_TEMPLATE_TOML: &str = include_str!("../assets/starter_template.toml");

/// Render a starter `argtuner.toml` with the provided template command
/// substituted into the `template` line. The command is embedded as a TOML
/// basic string via the `toml` serializer (escaping fully library-handled).
pub fn render_starter_toml(command: &str) -> String {
    STARTER_TEMPLATE_TOML.replace(
        "__ARGTUNER_TEMPLATE__",
        &toml::Value::String(command.to_string()).to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_starter_toml_plain_command() {
        let out = render_starter_toml("my_bin --lr {lr}");
        assert!(out.contains("template = \"my_bin --lr {lr}\""));
    }

    #[test]
    fn render_starter_toml_escapes_special_characters() {
        let cmd = "run --flag \"a b\" --path C:\\tmp\\x\n--next";
        let out = render_starter_toml(cmd);
        // The substituted template line must re-parse to the exact command,
        // including quotes, backslashes, and a newline.
        let doc: toml::Table = out.parse().unwrap();
        assert_eq!(doc["template"].as_str(), Some(cmd));
    }
}

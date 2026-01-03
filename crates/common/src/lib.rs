//! Shared constants for argtuner crates.

/// Command stdout parser prefix for result/event messages.
pub const RESULT_PREFIX: &str = "::ARGTUNER::";

/// Namespaces for payload keys.
pub const METRIC_NAMESPACE: &str = "metric";
pub const MODEL_NAMESPACE: &str = "model";
pub const TUNER_NAMESPACE: &str = "tuner";

/// Event names.
pub const EARLY_STOPPED_EVENT: &str = "early_stopped";
pub const INVALID_CONFIG_EVENT: &str = "invalid_config";
pub const EPOCH_END_EVENT: &str = "epoch_end";
pub const BINDING_VERSION_EVENT: &str = "binding_version";
pub const BINDING_VERSION_FIELD: &str = "version";
pub const MODEL_EARLY_STOPPED_EVENT: &str = "model.early_stopped";
pub const MODEL_INVALID_CONFIG_EVENT: &str = "model.invalid_config";
pub const MODEL_EPOCH_END_EVENT: &str = "model.epoch_end";
pub const TUNER_BINDING_VERSION_EVENT: &str = "tuner.binding_version";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Result,
    EarlyStopped,
    InvalidConfig,
    EpochEnd,
    TunerApiVersion,
}

impl EventKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::Result => "result",
            EventKind::EarlyStopped => MODEL_EARLY_STOPPED_EVENT,
            EventKind::InvalidConfig => MODEL_INVALID_CONFIG_EVENT,
            EventKind::EpochEnd => MODEL_EPOCH_END_EVENT,
            EventKind::TunerApiVersion => TUNER_BINDING_VERSION_EVENT,
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            MODEL_EARLY_STOPPED_EVENT | EARLY_STOPPED_EVENT => Some(EventKind::EarlyStopped),
            MODEL_INVALID_CONFIG_EVENT | INVALID_CONFIG_EVENT => Some(EventKind::InvalidConfig),
            MODEL_EPOCH_END_EVENT | EPOCH_END_EVENT => Some(EventKind::EpochEnd),
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

/// Render a starter argtuner.toml with the provided template command.
pub fn render_template_toml(command: &str) -> String {
    let template_bytes = include_bytes!("../assets/template.toml");
    let template = String::from_utf8_lossy(template_bytes);
    template.replace("__ARGTUNER_TEMPLATE__", &escape_toml_string(command))
}

fn escape_toml_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

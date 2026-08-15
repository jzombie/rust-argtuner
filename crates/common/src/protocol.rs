//! Canonical definition of the argtuner "IPC" stdio protocol.
//!
//! Training subprocesses emit JSON lines on stdout, each prefixed with the
//! literal [`RESULT_PREFIX`] (`::ARGTUNER::`). This module is the single source
//! of truth for the wire shapes: the emitter
//! (`argtuner`) and the tuner parser
//! (`crate::command::subprocess::ipc`) both (de)serialize the same
//! [`IpcMessage`] type.
//!
//! The protocol self-describes via a JSON Schema generated from
//! [`IpcMessage`] with `schemars` (see [`protocol_schema`]). Consumers can
//! tap into the protocol with any JSON Schema tooling: validate intercepted
//! lines, generate bindings in other languages, or autocomplete event names.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    BINDING_VERSION_EVENT, EARLY_STOPPED_EVENT, EPOCH_END_EVENT, INVALID_CONFIG_EVENT,
    METRIC_NAMESPACE, MODEL_NAMESPACE, RESULT_PREFIX, STEP_END_EVENT, TUNER_NAMESPACE,
};

/// Name of the protocol spoken after the [`RESULT_PREFIX`] on each line.
pub const PROTOCOL_NAME: &str = "argtuner.ipc";

/// Regex (applied after ANSI stripping) that matches any stdout line carrying an
/// IPC message. The parser finds the first occurrence of the literal
/// prefix anywhere in the stripped line; this documents that framing
/// contract for consumers that validate whole lines.
pub const LINE_PATTERN: &str = "^.*::ARGTUNER::.*$";

/// One IPC message as it appears on the wire.
///
/// `type` discriminates the two shapes; `fields` values are always strings.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum IpcMessage {
    /// A typed protocol event, e.g. `model.epoch_end`. The `name` is one of
    /// the canonical event names (or a legacy un-namespaced alias).
    #[schemars(description = "A typed protocol event, e.g. `model.epoch_end`.")]
    Event {
        #[schemars(schema_with = "crate::protocol::event_name_schema")]
        name: String,
        #[serde(default)]
        fields: BTreeMap<String, String>,
    },
    #[schemars(
        description = "A flat result dump. No `name`; each field becomes a top-level trial field on the tuner side."
    )]
    Result {
        #[serde(default)]
        fields: BTreeMap<String, String>,
    },
}

/// Canonical event names plus the legacy un-namespaced aliases that
/// [`crate::EventKind::from_name`] still accepts.
pub fn event_name_values() -> Vec<String> {
    let mut names = Vec::new();
    for kind in [
        crate::EventKind::EarlyStopped,
        crate::EventKind::InvalidConfig,
        crate::EventKind::EpochEnd,
        crate::EventKind::StepEnd,
        crate::EventKind::TunerApiVersion,
    ] {
        names.push(kind.as_str().to_string());
    }
    for alias in [
        EARLY_STOPPED_EVENT,
        INVALID_CONFIG_EVENT,
        EPOCH_END_EVENT,
        STEP_END_EVENT,
        BINDING_VERSION_EVENT,
    ] {
        names.push(alias.to_string());
    }
    names.sort();
    names.dedup();
    names
}

fn event_name_schema(_gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let schema = serde_json::json!({
        "type": "string",
        "title": "event name",
        "description": "Canonical event name, or a legacy un-namespaced alias.",
        "enum": event_name_values(),
    });
    schemars::Schema::try_from(schema).expect("event name schema is a JSON object")
}

/// Generate the JSON Schema that self-describes the IPC protocol.
///
/// The schema validates the JSON *document* after the prefix. The line framing
/// (prefix, ANSI stripping, scanning rule) lives outside the JSON document and
/// is captured in the `x-argtuner` extension object.
pub fn protocol_schema() -> schemars::Schema {
    let mut schema = schemars::schema_for!(IpcMessage);
    if let Some(obj) = schema.as_object_mut() {
        obj.insert(
            "title".to_string(),
            serde_json::Value::String("argtuner IPC protocol".to_string()),
        );
        obj.insert(
            "description".to_string(),
            serde_json::Value::String(
                "Line-framed JSON protocol spoken over subprocess stdout. Each stdout line is \
                 ANSI-stripped; the first occurrence of the literal prefix `::ARGTUNER::` marks \
                 the start of a message, and the JSON document after the prefix must match this \
                 schema. Field values are always strings on the wire."
                    .to_string(),
            ),
        );
        obj.insert(
            "x-argtuner".to_string(),
            serde_json::json!({
                "protocol": PROTOCOL_NAME,
                "prefix": RESULT_PREFIX,
                "stripAnsi": true,
                "linePattern": LINE_PATTERN,
                "namespaces": [METRIC_NAMESPACE, MODEL_NAMESPACE, TUNER_NAMESPACE],
            }),
        );
    }
    schema
}

/// The generated protocol schema serialized as a stable, pretty-printed JSON
/// document. Byte-for-byte stable across runs; committed to the repo at
/// `crates/common/assets/protocol.schema.json`.
pub fn protocol_schema_string() -> String {
    serde_json::to_string_pretty(&protocol_schema()).expect("protocol schema serializes")
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn event_name_values_cover_eventkind() {
        let names = event_name_values();
        let mut accepted = BTreeSet::new();
        for kind in [
            crate::EventKind::EarlyStopped,
            crate::EventKind::InvalidConfig,
            crate::EventKind::EpochEnd,
            crate::EventKind::StepEnd,
            crate::EventKind::TunerApiVersion,
        ] {
            accepted.insert(kind.as_str().to_string());
        }
        for alias in [
            EARLY_STOPPED_EVENT,
            INVALID_CONFIG_EVENT,
            EPOCH_END_EVENT,
            STEP_END_EVENT,
            BINDING_VERSION_EVENT,
        ] {
            accepted.insert(alias.to_string());
        }
        assert_eq!(names, accepted.into_iter().collect::<Vec<_>>());
    }

    #[test]
    fn every_event_name_is_recognized() {
        for name in event_name_values() {
            assert!(
                crate::EventKind::from_name(&name).is_some(),
                "{name} is listed in the schema but EventKind::from_name rejects it"
            );
        }
    }

    #[test]
    fn round_trip_event_and_result() {
        let event = IpcMessage::Event {
            name: "model.epoch_end".to_string(),
            fields: BTreeMap::from([("metric".to_string(), "0.5".to_string())]),
        };
        let json = serde_json::to_value(&event).unwrap();
        let back: IpcMessage = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), json);

        let result = IpcMessage::Result {
            fields: BTreeMap::from([("loss".to_string(), "0.1".to_string())]),
        };
        let json = serde_json::to_value(&result).unwrap();
        let back: IpcMessage = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), json);
    }

    #[test]
    fn legacy_aliases_still_parse() {
        let msg: IpcMessage =
            serde_json::from_str(r#"{"type":"event","name":"epoch_end","fields":{}}"#).unwrap();
        assert!(matches!(msg, IpcMessage::Event { name, .. } if name == "epoch_end"));
    }

    #[test]
    fn committed_schema_is_current() {
        let generated = protocol_schema_string();
        let committed = include_str!("../assets/protocol.schema.json");
        assert_eq!(
            line_ending::LineEnding::normalize(&generated),
            line_ending::LineEnding::normalize(committed),
            "protocol.schema.json is stale; regenerate it with: \
             cargo run -p argtuner --bin print_protocol_schema > crates/common/assets/protocol.schema.json"
        );
    }

    #[test]
    fn schema_documents_framing() {
        let schema = protocol_schema();
        let value = schema.to_value();
        let x_argtuner = value["x-argtuner"].as_object().unwrap();
        assert_eq!(x_argtuner["prefix"].as_str(), Some(RESULT_PREFIX));
        assert_eq!(x_argtuner["protocol"].as_str(), Some(PROTOCOL_NAME));
        assert_eq!(x_argtuner["stripAnsi"].as_bool(), Some(true));
    }

    #[test]
    fn schema_has_no_unknown_enum() {
        let schema = protocol_schema();
        let value = schema.to_value();
        let names: Vec<&str> = value
            .get("oneOf")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s["properties"]["name"]["enum"].as_array())
                    .flatten()
                    .filter_map(|v| v.as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            names.len(),
            10,
            "expected canonical + alias event names in schema"
        );
    }
}

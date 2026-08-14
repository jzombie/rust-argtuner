//! End-to-end check that the committed protocol schema (the artifact external
//! consumers actually tap into) accepts every line the tuner parser accepts,
//! and rejects malformed ones.

use jsonschema::validate as validate_against;
use serde_json::Value;

const SCHEMA: &str = include_str!("../crates/common/assets/protocol.schema.json");
const PREFIX: &str = "::ARGTUNER::";

fn assert_valid(line: &str) {
    let payload = line
        .strip_prefix(PREFIX)
        .unwrap_or_else(|| panic!("{line:?} missing the {PREFIX:?} prefix"));
    let instance: Value = serde_json::from_str(payload)
        .unwrap_or_else(|e| panic!("{line:?} is not valid JSON after the prefix: {e}"));
    let schema: Value = serde_json::from_str(SCHEMA).expect("committed schema parses");
    validate_against(&schema, &instance)
        .unwrap_or_else(|e| panic!("{line:?} does not match the protocol schema: {e}"));
}

#[test]
fn representative_lines_validate_against_committed_schema() {
    for line in [
        "::ARGTUNER::{\"type\":\"event\",\"name\":\"model.epoch_end\",\"fields\":{\"metric\":\"0.123\",\"aux\":\"42\",\"epoch\":\"1\"}}",
        "::ARGTUNER::{\"type\":\"event\",\"name\":\"model.early_stopped\",\"fields\":{}}",
        "::ARGTUNER::{\"type\":\"event\",\"name\":\"model.step_end\",\"fields\":{\"loss\":\"0.5\",\"epoch\":\"1\"}}",
        "::ARGTUNER::{\"type\":\"event\",\"name\":\"model.invalid_config\",\"fields\":{\"error\":\"bad hp\"}}",
        "::ARGTUNER::{\"type\":\"event\",\"name\":\"tuner.binding_version\",\"fields\":{\"version\":\"0.1.0-alpha\"}}",
        // legacy un-namespaced alias, still accepted by EventKind::from_name
        "::ARGTUNER::{\"type\":\"event\",\"name\":\"epoch_end\",\"fields\":{}}",
        "::ARGTUNER::{\"type\":\"result\",\"fields\":{\"loss\":\"0.1\",\"epoch\":\"1\"}}",
        // empty result fields are legal (the emitter skips them, the parser tolerates them)
        "::ARGTUNER::{\"type\":\"result\",\"fields\":{}}",
    ] {
        assert_valid(line);
    }
}

#[test]
fn malformed_lines_are_rejected() {
    let schema: Value = serde_json::from_str(SCHEMA).expect("committed schema parses");
    for payload in [
        r#"{"type":"bogus","fields":{}}"#,
        r#"{"type":"event","fields":{}}"#, // event requires a name
        r#"{"type":"event","name":"model.epoch_end","fields":{"metric":42}}"#, // values must be strings
        r#"{"type":"event","name":"model.does_not_exist","fields":{}}"#,       // unknown event name
    ] {
        let instance: Value = serde_json::from_str(payload).unwrap();
        assert!(
            validate_against(&schema, &instance).is_err(),
            "{payload} should be rejected by the schema"
        );
    }
}

#[test]
fn framing_lives_outside_the_json_document() {
    // The prefix/ANSI framing is not part of the JSON document, so it is
    // documented in `x-argtuner` rather than validated by the schema.
    let full_line = "::ARGTUNER::{\"type\":\"event\",\"name\":\"model.epoch_end\",\"fields\":{}}";
    let payload: Value = serde_json::from_str(full_line.strip_prefix(PREFIX).unwrap()).unwrap();
    let schema: Value = serde_json::from_str(SCHEMA).unwrap();
    validate_against(&schema, &payload).expect("stripped payload validates");
    assert!(
        serde_json::from_str::<Value>(full_line).is_err(),
        "the raw line including the prefix is not itself a JSON document"
    );
}

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

/// Extract a README fenced block: everything between the `marker` comment's
/// following `` ```<fence_lang> `` fence and the closing `` ``` `` fence, line
/// endings normalized to LF and trimmed.
fn extract_fenced_block(readme: &str, marker: &str, fence_lang: &str) -> Option<String> {
    let marker_end = readme.find(marker)?.checked_add(marker.len())?;
    let rest = &readme[marker_end..];
    let fence = format!("```{fence_lang}");
    let fence_start = rest.find(&fence)? + fence.len();
    let after_fence = &rest[fence_start..];
    let after_fence = line_ending::LineEnding::normalize(after_fence);
    let content = after_fence.strip_prefix('\n')?;
    let close = content.find("\n```")?;
    Some(content[..close].trim().to_string())
}

/// Extract the schema echoed in the README.
fn extract_readme_schema(readme: &str) -> Option<String> {
    extract_fenced_block(readme, "<!-- protocol.schema.json -->", "json")
}

#[test]
fn readme_echoes_current_schema() {
    let readme = include_str!("../README.md");
    let echoed = extract_readme_schema(readme).unwrap_or_else(|| {
        panic!(
            "README.md must embed the schema in a ```json block prefixed by the \
             `<!-- protocol.schema.json -->` marker"
        )
    });
    let asset = line_ending::LineEnding::normalize(SCHEMA);
    assert_eq!(
        echoed,
        asset.trim(),
        "the schema echoed in README.md is stale; regenerate crates/common/assets/\
         protocol.schema.json and paste its contents into the README block:\n  \
         cargo run -p argtuner --bin print_protocol_schema \
         > crates/common/assets/protocol.schema.json"
    );
}

#[test]
fn readme_example_extracts_payload() {
    let readme = include_str!("../README.md");
    let feed = extract_fenced_block(readme, "<!-- protocol.example.feed -->", "text")
        .unwrap_or_else(|| {
            panic!(
                "README.md must embed the example feed in a ```text block prefixed by the \
                 `<!-- protocol.example.feed -->` marker"
            )
        })
        .replace("\\x1b", "\u{1b}");
    let expected_json = extract_fenced_block(readme, "<!-- protocol.example.parsed -->", "json")
        .unwrap_or_else(|| {
            panic!(
                "README.md must embed the expected payload in a ```json block prefixed by the \
                 `<!-- protocol.example.parsed -->` marker"
            )
        });

    let expected: argtuner_common::TalkbackMessage = serde_json::from_str(&expected_json)
        .expect("README expected payload must be a valid TalkbackMessage");

    let parsed = argtuner::command::subprocess::parse_prefix_lines(&feed, PREFIX)
        .expect("example feed must parse cleanly");
    match expected {
        argtuner_common::TalkbackMessage::Event { name, fields } => {
            assert_eq!(
                parsed,
                vec![vec![argtuner::command::subprocess::ParsedItem::Event {
                    name,
                    fields,
                }]],
                "the example feed must parse to exactly the README's expected message"
            );
        }
        argtuner_common::TalkbackMessage::Result { .. } => {
            panic!("README example must show an `event` payload");
        }
    }
}

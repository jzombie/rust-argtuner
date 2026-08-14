//! All README self-assertions in one place: every fenced block in the README
//! that claims to show a real artifact is extracted by its marker comment and
//! compared byte-for-byte against the live artifact (committed schema, protocol
//! example feed, and the config_showcase project + its `argtuner inspect` output).

use std::path::PathBuf;
use std::process::Command;

use argtuner::project::Project;
use argtuner::test_support::{bin_path, extract_fenced_block};

const SHOWCASE: &str = "examples/config_showcase";
const PREFIX: &str = "::ARGTUNER::";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn readme() -> &'static str {
    include_str!("../README.md")
}

/// Extract a marker-annotated README fenced block, panicking with the standard
/// "stale/regenerate" guidance if the marker or fence is missing.
fn readme_block(marker: &str, fence_lang: &str) -> String {
    extract_fenced_block(readme(), marker, fence_lang).unwrap_or_else(|| {
        panic!(
            "README.md must embed the `{marker}` artifact in a ```{fence_lang} block \
             prefixed by the `{marker}` comment"
        )
    })
}

fn inspect_render() -> String {
    argtuner::inspect::render_inspect(&Project::new(SHOWCASE)).expect("showcase config must parse")
}

// ---------------------------------------------------------------------------
// Protocol schema (<!-- protocol.schema.json -->)
// ---------------------------------------------------------------------------

#[test]
fn readme_echoes_current_schema() {
    let echoed = readme_block("<!-- protocol.schema.json -->", "json");
    let asset = line_ending::LineEnding::normalize(include_str!(
        "../crates/common/assets/protocol.schema.json"
    ));
    assert_eq!(
        echoed,
        asset.trim(),
        "the schema echoed in README.md is stale; regenerate crates/common/assets/\
         protocol.schema.json and paste its contents into the README block:\n  \
         cargo run -p argtuner --bin print_protocol_schema \
         > crates/common/assets/protocol.schema.json"
    );
}

// ---------------------------------------------------------------------------
// Protocol example feed (<!-- protocol.example.feed --> / .parsed)
// ---------------------------------------------------------------------------

#[test]
fn readme_example_extracts_payload() {
    let feed = readme_block("<!-- protocol.example.feed -->", "text").replace("\\x1b", "\u{1b}");
    let expected: argtuner_common::TalkbackMessage =
        serde_json::from_str(&readme_block("<!-- protocol.example.parsed -->", "json"))
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

// ---------------------------------------------------------------------------
// Config_showcase (<!-- config.example -->)
// ---------------------------------------------------------------------------

#[test]
fn config_example_matches_committed_file() {
    let echoed = readme_block("<!-- config.example -->", "toml");
    let file = line_ending::LineEnding::normalize(
        &std::fs::read_to_string(manifest_dir().join(SHOWCASE).join("argtuner.toml"))
            .expect("showcase config exists"),
    );
    assert_eq!(
        echoed,
        file.trim(),
        "the config echoed in README.md is stale; copy \
         examples/config_showcase/argtuner.toml into the README block"
    );
}

// ---------------------------------------------------------------------------
// `argtuner inspect` output (<!-- config.inspect.output -->)
// ---------------------------------------------------------------------------

#[test]
fn inspect_output_matches_committed_project() {
    // Pin CWD so `Project::new(SHOWCASE)` resolves to the repo root regardless
    // of where the test harness was launched.
    std::env::set_current_dir(manifest_dir()).expect("chdir to package root");

    let echoed = readme_block("<!-- config.inspect.output -->", "text");
    assert_eq!(
        echoed,
        inspect_render().trim(),
        "the `argtuner inspect` output echoed in README.md is stale; \
         regenerate it with `cargo run -p argtuner -- inspect examples/config_showcase`"
    );
}

#[test]
fn binary_inspect_matches_lib_render() {
    std::env::set_current_dir(manifest_dir()).expect("chdir to package root");

    let output = Command::new(bin_path("argtuner"))
        .arg("inspect")
        .arg(SHOWCASE)
        .current_dir(manifest_dir())
        .output()
        .expect("run `argtuner inspect`");
    assert!(
        output.status.success(),
        "`argtuner inspect` exited {:?}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        inspect_render().trim(),
        String::from_utf8_lossy(&output.stdout).trim(),
        "the `argtuner inspect` subcommand must print exactly what the lib renders"
    );
}

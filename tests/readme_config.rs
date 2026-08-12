//! End-to-end check that the config showcased in the README — the
//! `examples/config_showcase` project and the `argtuner inspect` output for it —
//! matches what argtuner actually parses. Mirrors the protocol-schema README
//! echo test: the fenced blocks are extracted by marker comment and compared
//! byte-for-byte against the real artifact.

use std::path::PathBuf;
use std::process::Command;

use argtuner::project::Project;
use argtuner::test_support::{bin_path, extract_fenced_block};

const SHOWCASE: &str = "examples/config_showcase";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn config_example_matches_committed_file() {
    let readme = include_str!("../README.md");
    let echoed = extract_fenced_block(readme, "<!-- config.example -->", "toml").unwrap_or_else(
        || {
            panic!(
                "README.md must embed the showcase config in a ```toml block prefixed by the \
                 `<!-- config.example -->` marker"
            )
        },
    );
    let file = std::fs::read_to_string(manifest_dir().join(SHOWCASE).join("argtuner.toml"))
        .expect("showcase config exists");
    assert_eq!(
        echoed,
        file.trim(),
        "the config echoed in README.md is stale; copy \
         examples/config_showcase/argtuner.toml into the README block"
    );
}

#[test]
fn inspect_output_matches_committed_project() {
    // Pin CWD so `Project::new(SHOWCASE)` resolves to the repo root regardless
    // of where the test harness was launched.
    std::env::set_current_dir(manifest_dir()).expect("chdir to package root");

    let readme = include_str!("../README.md");
    let echoed =
        extract_fenced_block(readme, "<!-- config.inspect.output -->", "text").unwrap_or_else(
            || {
                panic!(
                    "README.md must embed the inspect output in a ```text block prefixed by the \
                     `<!-- config.inspect.output -->` marker"
                )
            },
        );
    let actual = argtuner::inspect::render_inspect(&Project::new(SHOWCASE))
        .expect("showcase config must parse");
    assert_eq!(
        echoed,
        actual.trim(),
        "the `argtuner inspect` output echoed in README.md is stale; \
         regenerate it with `cargo run -p argtuner -- inspect examples/config_showcase`"
    );
}

#[test]
fn binary_inspect_matches_lib_render() {
    std::env::set_current_dir(manifest_dir()).expect("chdir to package root");
    let expected = argtuner::inspect::render_inspect(&Project::new(SHOWCASE))
        .expect("showcase config must parse");

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
        expected.trim(),
        String::from_utf8_lossy(&output.stdout).trim(),
        "the `argtuner inspect` subcommand must print exactly what the lib renders"
    );
}

//! End-to-end coverage for `argtuner resume-trial`.
//!
//! The dispatch arm in `src/cli/mod.rs` only ever runs inside the real
//! binary, so these tests spawn it and assert on exit status plus output.
//! Every branch reachable from the outside is exercised here: the happy
//! path, each refusal `resume_trial` can produce, and the setup failures
//! (missing config, unusable project root, held project lock, invalid
//! config, config edited since the last run).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use argtuner::{
    Goal, PLACEHOLDER_TRIAL_DIR, Project, ProjectSettings, Pruner, Sampler, SamplerConfig,
    Scheduler, SchedulerConfig, SearchSpace, TrialRecord, TrialStatus, UnifiedConfig,
};

fn emit_result_command() -> String {
    argtuner::test_support::bin_command("mock_emit_result")
}

/// Write a project config whose template satisfies `validate_project_config`
/// (it must carry `--checkpoint-dir {trial_dir}`).
fn save_config(project: &Project, template: &str, trial_timeout_s: u64) {
    let config = UnifiedConfig {
        project: ProjectSettings {
            metric_key: "metric".to_string(),
            goal: Goal::Min,
            pruner: Pruner::None,
            inject_trial_placeholders: true,
            checkpoint_arg: None,
            objectives: vec![],
        },
        sampler: SamplerConfig {
            kind: Sampler::Random,
            ..SamplerConfig::default()
        },
        scheduler: SchedulerConfig {
            kind: Scheduler::Fixed,
            n_trials: 1,
            seed: 42,
            trial_timeout_s,
            ..SchedulerConfig::default()
        },
        space: SearchSpace { params: vec![] },
        template: template.to_string(),
    };
    project.save_unified_config(&config).expect("save config");
}

fn setup_project(root: &Path) -> Project {
    setup_project_with_timeout(root, 0)
}

fn setup_project_with_timeout(root: &Path, trial_timeout_s: u64) -> Project {
    let project = Project::new(root);
    project.ensure_dirs().expect("dirs");
    let template = format!("{} --checkpoint-dir {{trial_dir}}", emit_result_command());
    save_config(&project, &template, trial_timeout_s);
    project
}

/// Plant an interrupted trial: artifact dir with a checkpoint-like sentinel
/// plus a store row, exactly as the sweep leaves a mid-run process death.
fn insert_trial(project: &Project, trial_id: usize, status: TrialStatus) -> PathBuf {
    let trial_dir = project.artifacts_dir().join(format!("trial_{trial_id}"));
    std::fs::create_dir_all(&trial_dir).expect("trial dir");
    std::fs::write(trial_dir.join("checkpoint_epoch_005.mpk"), b"weights").expect("sentinel");
    let mut fields = BTreeMap::new();
    fields.insert(
        PLACEHOLDER_TRIAL_DIR.to_string(),
        trial_dir.to_string_lossy().into_owned(),
    );
    project
        .store()
        .expect("store")
        .append(&TrialRecord {
            trial_id,
            status,
            elapsed_ms: 0,
            error: None,
            fields,
        })
        .expect("append trial");
    trial_dir
}

fn resume(root: &Path, trial_id: usize) -> Output {
    Command::new(argtuner::test_support::bin_path("argtuner"))
        .arg("resume-trial")
        .arg(root)
        .arg(trial_id.to_string())
        .output()
        .expect("spawn argtuner resume-trial")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn assert_failed(output: &Output, expected: &str) {
    assert!(
        !output.status.success(),
        "expected failure, got success.\nstdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
    assert!(
        stderr(output).contains(expected),
        "expected stderr to contain {expected:?}.\nstdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
}

#[test]
fn resume_trial_cli_reruns_interrupted_trial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("cli-resume");
    // A configured trial timeout exercises the `Some(duration)` side of the
    // runner-options wiring; the other tests keep the default (0 → `None`).
    let project = setup_project_with_timeout(&root, 60);
    let trial_dir = insert_trial(&project, 0, TrialStatus::Running);

    let out = resume(&root, 0);
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        stdout(&out),
        stderr(&out)
    );
    assert!(
        stdout(&out).contains("resumed trial 0: score="),
        "stdout: {}",
        stdout(&out)
    );
    assert!(
        trial_dir.join("checkpoint_epoch_005.mpk").exists(),
        "resume must preserve the trial dir (the sweep path deletes it)"
    );

    let row = project
        .store()
        .expect("store")
        .load_fields(0)
        .expect("load fields")
        .expect("row");
    assert_eq!(row.get("status"), Some(&"ok".to_string()));
}

#[test]
fn resume_trial_cli_refuses_completed_trial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("cli-resume-done");
    let project = setup_project(&root);
    insert_trial(&project, 0, TrialStatus::Ok);

    assert_failed(&resume(&root, 0), "already completed");
}

#[test]
fn resume_trial_cli_refuses_missing_trial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("cli-resume-missing");
    setup_project(&root);

    assert_failed(&resume(&root, 7), "no trial 7");
}

#[test]
fn resume_trial_cli_refuses_when_trial_dir_is_gone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("cli-resume-gone");
    let project = setup_project(&root);
    let trial_dir = insert_trial(&project, 0, TrialStatus::Running);
    std::fs::remove_dir_all(&trial_dir).expect("rm trial dir");

    assert_failed(&resume(&root, 0), "dir is gone");
}

#[test]
fn resume_trial_cli_fails_without_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("cli-resume-no-config");

    assert_failed(&resume(&root, 0), "failed to load config");
}

#[test]
fn resume_trial_cli_fails_on_unparseable_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("cli-resume-bad-config");
    let project = Project::new(&root);
    project.ensure_dirs().expect("dirs");
    std::fs::write(
        project.unified_config_path(),
        "this is not valid toml [[[\n",
    )
    .expect("write bad config");

    assert_failed(&resume(&root, 0), "failed to load config");
}

#[test]
fn resume_trial_cli_fails_when_project_root_is_a_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("cli-resume-root-is-file");
    std::fs::write(&root, b"not a directory").expect("write file");

    assert_failed(&resume(&root, 0), "failed to prepare project dirs");
}

#[test]
fn resume_trial_cli_fails_when_project_is_locked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("cli-resume-locked");
    let project = setup_project(&root);
    insert_trial(&project, 0, TrialStatus::Running);

    // Hold the project lock while the CLI tries to take it.
    let _lock = project.acquire_lock().expect("lock");
    assert_failed(&resume(&root, 0), "failed to acquire project lock");
}

#[test]
fn resume_trial_cli_fails_validation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("cli-resume-invalid");
    let project = Project::new(&root);
    project.ensure_dirs().expect("dirs");
    // No `--checkpoint-dir {trial_dir}`: validate_project_config rejects it.
    save_config(&project, &emit_result_command(), 0);

    assert_failed(&resume(&root, 0), "template must include");
}

#[test]
fn resume_trial_cli_refuses_changed_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("cli-resume-changed");
    let project = setup_project(&root);
    insert_trial(&project, 0, TrialStatus::Running);

    // Snapshot the config the way a previous run would have recorded it.
    let stored = std::fs::read_to_string(project.unified_config_path()).expect("read config");
    project
        .store()
        .expect("store")
        .ensure_project_config(&stored, false)
        .expect("record config");

    // Edit the config on disk; a trailing comment keeps it parseable, so the
    // refusal comes from the snapshot comparison, not from parsing.
    let edited = format!("{stored}# touched after the last run\n");
    std::fs::write(project.unified_config_path(), edited).expect("edit config");

    assert_failed(&resume(&root, 0), "changed since the last run");
}

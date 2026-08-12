use argtuner::{CONFIG_FILENAME, Project, RunOptions, TRIALS_CSV_FILENAME, Tuner};

#[test]
fn dry_run_avoids_project_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project_root = dir.path().join("probe-project");
    std::fs::create_dir_all(&project_root).expect("project dir");

    let emit = argtuner::test_support::bin_command("mock_emit_result");

    let toml = format!(
        r#"
        template = "{emit} --checkpoint-dir {{trial_dir}} --lr {{lr}}"

        [project]
        metric_key = "metric"
        goal = "min"
        pruner = "none"
        inject_trial_placeholders = true

        [sampler]
        type = "random"

        [scheduler]
        type = "fixed"
        n_trials = 1
        seed = 7

        [space]
        [[space.params]]
        type = "Float"
        name = "lr"
        min = 0.0
        max = 1.0
        log = false
    "#,
        emit = emit.replace('\\', "\\\\")
    );
    std::fs::write(project_root.join(CONFIG_FILENAME), toml).expect("write config");

    let project = Project::new(&project_root);
    let tuner = Tuner::new(project);
    tuner
        .run_with_options(RunOptions {
            dry_run: true,
            allow_config_change: false,
        })
        .expect("dry run");

    let trials_csv = project_root.join(TRIALS_CSV_FILENAME);
    let trials_sqlite = trials_csv.with_extension("sqlite");
    assert!(!trials_csv.exists(), "dry run should not write trials.csv");
    assert!(
        !trials_sqlite.exists(),
        "dry run should not write trials.sqlite"
    );
}

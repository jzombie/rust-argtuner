use argtuner::{Project, RunOptions, Tuner};
use indoc::indoc;

fn emit_result_command() -> String {
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_emit_result") {
        format!("{path} --")
    } else {
        "cargo run -q -p argtuner --bin emit_result --".to_string()
    }
}

#[test]
fn duplicate_configs_stop_tuning_after_retries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project_root = dir.path().join("dup-configs");
    std::fs::create_dir_all(&project_root).expect("project dir");
    let emit = emit_result_command();

    let toml = format!(
        indoc! {r#"
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
        n_trials = 2
        seed = 7

        [space]
        [[space.params]]
        type = "Float"
        name = "lr"
        min = 0.5
        max = 0.5
        log = false
    "#},
        emit = emit
    );
    std::fs::write(project_root.join("argtuner.toml"), toml).expect("write config");

    let project = Project::new(&project_root);
    let tuner = Tuner::new(project);
    let err = tuner
        .run_with_options(RunOptions { dry_run: false })
        .expect_err("expected duplicate-config stop");
    assert!(
        err.to_string().contains("unable to find a unique config"),
        "unexpected error: {err}"
    );
}

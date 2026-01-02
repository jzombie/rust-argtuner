use argtuner::{
    FIELD_METRIC, FIELD_SCORE, FIELD_TRIAL_STATUS, METRIC_NAMESPACE, TRIALS_CSV_FILENAME,
    TrialStatus,
};
use indoc::indoc;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

// Use the crate-provided helper to locate the workspace root.

#[test]
fn argtuner_runs_linear_regression_example() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project_path = dir.path().join("project");
    fs::create_dir_all(&project_path).expect("project dir");
    fs::create_dir_all(project_path.join("artifacts")).expect("artifacts");

    fs::write(
        project_path.join("argtuner.toml"),
        indoc! {r#"
            template = "cargo run -p argtuner --example linear_regression -- --lr {lr} --steps {steps} --checkpoint-dir {trial_dir}"

            [project]
            metric_key = "loss"
            goal = "min"
            pruner = "none"

            [sampler]
            type = "random"

            [scheduler]
            type = "fixed"
            n_trials = 4


            [space]
            [[space.params]]
            type = "Float"
            name = "lr"
            min = 0.001
            max = 0.05
            log = false

            [[space.params]]
            type = "Int"
            name = "steps"
            min = 5
            max = 50
        "#},
    )
    .expect("config");

    let project = argtuner::Project::new(&project_path);
    let _guard = DirectoryGuard::new(argtuner::workspace_root());

    argtuner::Tuner::new(project).run().expect("tuner run");

    let trials_path = project_path.join(TRIALS_CSV_FILENAME);
    let mut reader = csv::Reader::from_path(&trials_path).expect("trials csv");
    let headers = reader
        .headers()
        .expect("headers")
        .iter()
        .map(|h| h.to_string())
        .collect::<Vec<_>>();
    let metric_idx = headers
        .iter()
        .position(|h| h == &format!("{METRIC_NAMESPACE}.loss"))
        .expect("metric header");
    let score_idx = headers
        .iter()
        .position(|h| h == FIELD_SCORE)
        .expect("score header");
    let status_idx = headers
        .iter()
        .position(|h| h == FIELD_TRIAL_STATUS)
        .expect("status header");
    let mut metrics = Vec::new();
    let mut distinct = HashSet::new();
    for result in reader.records() {
        let record = result.expect("record");
        if record.get(status_idx).unwrap_or("") != TrialStatus::Ok.as_str() {
            continue;
        }
        let metric: f64 = record
            .get(metric_idx)
            .expect("metric cell")
            .parse()
            .expect("metric parse");
        let score: f64 = record
            .get(score_idx)
            .expect("score cell")
            .parse()
            .expect("score parse");
        assert!((metric - score).abs() < 1e-9);
        let metric_name = record
            .get(
                headers
                    .iter()
                    .position(|h| h == FIELD_METRIC)
                    .expect("metric name header"),
            )
            .unwrap_or("");
        assert_eq!(metric_name, "loss");
        metrics.push(metric);
        distinct.insert(format!("{metric:.8}"));
    }
    assert!(metrics.len() >= 4);
    assert!(distinct.len() >= 2);
}

struct DirectoryGuard {
    original: PathBuf,
}

impl DirectoryGuard {
    fn new(path: PathBuf) -> Self {
        let original = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(path).expect("set current dir");
        Self { original }
    }
}

impl Drop for DirectoryGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

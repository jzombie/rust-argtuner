use argtuner::{
    CommandTemplate, HP_PREFIX, TRIALS_CSV_FILENAME, TrialRecord, TrialStatus, TrialStore,
};
use indoc::indoc;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn rebuild_csv_from_db_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project_root = dir.path().join("rebuild-project");
    std::fs::create_dir_all(&project_root).expect("project dir");

    let toml = indoc! {r#"
        template = "echo ok"

        [project]
        metric_key = "metric"
        goal = "min"
        pruner = "none"

        [sampler]
        type = "random"

        [scheduler]
        type = "fixed"
        n_trials = 1

        [space]
        [[space.params]]
        type = "Float"
        name = "lr"
        min = 0.0
        max = 1.0
        log = false
    "#};
    std::fs::write(project_root.join("argtuner.toml"), toml).expect("write config");

    let trials_path = project_root.join(TRIALS_CSV_FILENAME);
    let template = CommandTemplate::new("echo ok".to_string());
    let store = TrialStore::new(&trials_path, template);
    let mut fields = BTreeMap::new();
    fields.insert(format!("{HP_PREFIX}lr"), "0.1".to_string());
    store
        .append(&TrialRecord {
            trial_id: 0,
            status: TrialStatus::Ok,
            elapsed_ms: 10,
            error: None,
            fields,
        })
        .expect("append");

    std::fs::remove_file(&trials_path).expect("remove csv");

    let exe = std::env::var_os("CARGO_BIN_EXE_argtuner")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut path = std::env::current_exe().expect("current exe");
            path.pop();
            path.pop();
            path.push("argtuner");
            if cfg!(windows) {
                path.set_extension("exe");
            }
            path
        });
    let status = Command::new(exe)
        .arg("rebuild-csv")
        .arg(project_root)
        .status()
        .expect("run rebuild-csv");
    assert!(status.success());

    let mut reader = csv::Reader::from_path(&trials_path).expect("trials csv");
    let headers = reader
        .headers()
        .expect("headers")
        .iter()
        .map(|h| h.to_string())
        .collect::<Vec<_>>();
    let lr_idx = headers
        .iter()
        .position(|h| h == &format!("{HP_PREFIX}lr"))
        .expect("lr header");
    let rows = reader
        .records()
        .collect::<Result<Vec<_>, _>>()
        .expect("records");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get(lr_idx).unwrap_or(""), "0.1");
}

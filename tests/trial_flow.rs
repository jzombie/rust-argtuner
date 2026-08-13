use argtuner::{
    Goal, Project, ProjectSettings, Pruner, Sampler, SamplerConfig, Scheduler, SchedulerConfig,
    SuccessiveHalvingSchedulerConfig, Tuner,
};

fn emit_result_command() -> String {
    argtuner::test_support::bin_command("mock_emit_result")
}

#[test]
fn successive_halving_creates_new_row_per_rung() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project_root = dir.path().join("argtuner").join("halving-flow");
    std::fs::create_dir_all(&project_root).expect("mkdir");
    let project = Project::new(&project_root);
    project.ensure_dirs().expect("dirs");

    let project_settings = ProjectSettings {
        metric_key: "metric".to_string(),
        goal: Goal::Min,
        pruner: Pruner::None,
        inject_trial_placeholders: true,
        checkpoint_arg: None,
        objectives: vec![],
    };
    let sampler = SamplerConfig {
        kind: Sampler::Random,
        ..SamplerConfig::default()
    };
    let scheduler = SchedulerConfig {
        kind: Scheduler::SuccessiveHalving,
        n_trials: 1,
        seed: 7,
        successive_halving: SuccessiveHalvingSchedulerConfig {
            budget_placeholder: "epochs".to_string(),
            min_epochs: 1,
            max_epochs: 2,
            eta: 2,
        },
        ..SchedulerConfig::default()
    };

    let template = argtuner::CommandTemplate::new(format!(
        "{} --epochs {{epochs}} --checkpoint-dir {{trial_dir}}",
        emit_result_command()
    ));
    let space: argtuner::SearchSpace = serde_json::from_value(serde_json::json!({
            "params": [
                    {"type": "Float", "name": "lr", "min": 0.001, "max": 0.01}
            ]
    }))
    .expect("space json");

    let unified_config = argtuner::UnifiedConfig {
        project: project_settings,
        sampler,
        scheduler,
        space,
        template: template.as_str().to_string(),
    };
    project
        .save_unified_config(&unified_config)
        .expect("save config");

    let tuner = Tuner::new(project.clone());
    tuner.run().expect("run");

    let rows = project.store().expect("store").load_rows().expect("rows");
    let ok_rows = rows
        .iter()
        .filter(|row| row.get("status").map(String::as_str) == Some("ok"))
        .collect::<Vec<_>>();
    // Should have 2 rows: rung 0 and rung 1
    assert_eq!(ok_rows.len(), 2);

    let row0 = ok_rows[0];
    assert_eq!(row0.get("trial.rung").map(String::as_str), Some("0"));

    let row1 = ok_rows[1];
    assert_eq!(row1.get("trial.rung").map(String::as_str), Some("1"));
}

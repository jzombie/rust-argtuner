use std::collections::BTreeMap;
use std::fs;

use argtuner::{
    CommandTemplate, FIELD_SCORE, Project, ProjectSettings, Sampler, SamplerConfig, Scheduler,
    SchedulerConfig, SearchSpace, SuccessiveHalvingSchedulerConfig, TRIAL_PREFIX, TrialRecord,
    TrialStatus, Tuner,
};

fn emit_result_command() -> String {
    argtuner::test_support::bin_command("mock_emit_result")
}

#[test]
fn engine_preserves_existing_artifacts_on_resume() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project_root = dir.path().join("argtuner").join("artifact-preservation");
    let project = Project::new(&project_root);
    project.ensure_dirs().expect("dirs");

    // 1. Setup Project
    // We use a simple echo command that outputs the result format.
    let template = CommandTemplate::new(format!(
        "{} --checkpoint-dir {{trial_dir}} --epochs {{epochs}}",
        emit_result_command()
    ));
    let space = SearchSpace { params: vec![] };

    let project_settings = ProjectSettings {
        metric_key: "metric".to_string(),
        goal: argtuner::Goal::Min,
        pruner: argtuner::Pruner::None,
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
        seed: 42,
        successive_halving: SuccessiveHalvingSchedulerConfig {
            budget_placeholder: "epochs".to_string(),
            min_epochs: 1,
            max_epochs: 2,
            eta: 2,
        },
        ..SchedulerConfig::default()
    };

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

    // 2. Create Parent Trial (trial_0)
    let trial_0_dir = project.artifacts_dir().join("trial_0");
    fs::create_dir_all(&trial_0_dir).expect("trial_0 dir");
    fs::write(trial_0_dir.join("unique_parent.txt"), "parent_unique").expect("write unique");
    fs::write(trial_0_dir.join("common.txt"), "parent_common").expect("write common");

    // 3. Insert Parent Record (Rung 0)
    let mut fields = BTreeMap::new();
    fields.insert(format!("{TRIAL_PREFIX}config_id"), "0".to_string());
    fields.insert(format!("{TRIAL_PREFIX}rung"), "0".to_string());
    fields.insert(format!("{TRIAL_PREFIX}bracket"), "0".to_string());
    fields.insert(FIELD_SCORE.to_string(), "0.5".to_string());
    project
        .store()
        .expect("store")
        .update(&TrialRecord {
            trial_id: 0,
            status: TrialStatus::Ok,
            elapsed_ms: 100,
            error: None,
            fields,
        })
        .expect("insert trial 0");

    // 4. Create Child Trial (trial_1) Pre-existing Artifacts
    // Successive Halving will promote trial 0 to rung 1.
    // Since trial 0 is the only one, it will likely be trial_id 1.
    let trial_1_dir = project.artifacts_dir().join("trial_1");
    fs::create_dir_all(&trial_1_dir).expect("trial_1 dir");
    fs::write(trial_1_dir.join("common.txt"), "child_common").expect("write child common");

    // 5. Run Tuner
    let tuner = Tuner::new(project.clone());
    tuner.run().expect("run");

    // 6. Verify Artifacts
    let unique_content =
        fs::read_to_string(trial_1_dir.join("unique_parent.txt")).expect("read unique");
    assert_eq!(unique_content, "parent_unique");

    let common_content = fs::read_to_string(trial_1_dir.join("common.txt")).expect("read common");
    assert_eq!(common_content, "child_common");
}

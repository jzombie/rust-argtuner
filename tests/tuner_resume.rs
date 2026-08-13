use argtuner::RunOptions;
use argtuner::{
    CONFIG_FILENAME, CommandTemplate, FIELD_SCORE, Project, ProjectSettings, Sampler,
    SamplerConfig, Scheduler, SchedulerConfig, SearchSpace, TRIAL_PREFIX, TrialRecord, TrialStatus,
    Tuner,
};
use std::collections::BTreeMap;

fn emit_result_command() -> String {
    argtuner::test_support::bin_command("mock_emit_result")
}

#[test]
fn tuner_skips_completed_trials() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("resume");
    let project = Project::new(&root);
    project.ensure_dirs().expect("dirs");

    // 1. Setup Project
    let template = CommandTemplate::new(format!(
        "{} --checkpoint-dir {{trial_dir}}",
        emit_result_command()
    ));
    let space = SearchSpace {
        params: vec![argtuner::ParamSpec::Float {
            name: "lr".to_string(),
            min: 0.0,
            max: 1.0,
            log_scale: false,
            step: None,
            format: None,
            parent: None,
            parent_values: None,
        }],
    };

    let project_settings = ProjectSettings {
        metric_key: "metric".to_string(),
        goal: argtuner::Goal::Min,
        pruner: argtuner::Pruner::None,
        inject_trial_placeholders: true,
        checkpoint_arg: None,
    };
    let sampler = SamplerConfig {
        kind: Sampler::Random,
        ..SamplerConfig::default()
    };
    let scheduler = SchedulerConfig {
        kind: Scheduler::Fixed,
        n_trials: 2,
        seed: 42,
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

    // 2. Insert Completed Trial 0
    let mut fields = BTreeMap::new();
    fields.insert(format!("{TRIAL_PREFIX}config_id"), "0".to_string());
    fields.insert(format!("{TRIAL_PREFIX}rung"), "0".to_string());
    fields.insert(format!("{TRIAL_PREFIX}bracket"), "0".to_string());
    fields.insert(FIELD_SCORE.to_string(), "0.5".to_string());
    fields.insert("hp.lr".to_string(), "0.123".to_string());
    project
        .store()
        .expect("store")
        .append(&TrialRecord {
            trial_id: 0,
            status: TrialStatus::Ok,
            elapsed_ms: 10,
            error: None,
            fields,
        })
        .expect("append");

    // 3. Run Tuner
    let tuner = Tuner::new(project.clone());
    tuner.run().expect("run");

    // 4. Verify
    // Should have 2 trials total (0 was pre-existing, 1 was run)
    let rows = project.store().expect("store").load_rows().expect("rows");
    let ok_rows = rows
        .iter()
        .filter(|row| row.get("status").map(String::as_str) == Some("ok"))
        .count();
    assert_eq!(ok_rows, 2);
}

#[test]
fn resume_aborts_on_config_change() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("resume-change");
    let project = Project::new(&root);
    project.ensure_dirs().expect("dirs");

    let template = CommandTemplate::new(format!(
        "{} --checkpoint-dir {{trial_dir}}",
        emit_result_command()
    ));
    let space = SearchSpace {
        params: vec![argtuner::ParamSpec::Float {
            name: "lr".to_string(),
            min: 0.0,
            max: 1.0,
            log_scale: false,
            step: None,
            format: None,
            parent: None,
            parent_values: None,
        }],
    };

    let project_settings = ProjectSettings {
        metric_key: "metric".to_string(),
        goal: argtuner::Goal::Min,
        pruner: argtuner::Pruner::None,
        inject_trial_placeholders: true,
        checkpoint_arg: None,
    };
    let sampler = SamplerConfig {
        kind: Sampler::Random,
        ..SamplerConfig::default()
    };
    let mut scheduler = SchedulerConfig {
        kind: Scheduler::Fixed,
        n_trials: 1,
        seed: 42,
        ..SchedulerConfig::default()
    };
    let mut unified_config = argtuner::UnifiedConfig {
        project: project_settings,
        sampler,
        scheduler: scheduler.clone(),
        space,
        template: template.as_str().to_string(),
    };
    project
        .save_unified_config(&unified_config)
        .expect("save config");

    let tuner = Tuner::new(project.clone());
    tuner.run().expect("run");

    // Modify the config
    scheduler.n_trials = 2;
    unified_config.scheduler = scheduler;
    project
        .save_unified_config(&unified_config)
        .expect("save updated config");

    let err = tuner.run().expect_err("resume should fail");
    let message = err.to_string();
    assert!(message.contains(&format!("{CONFIG_FILENAME} changed")));
    assert!(message.contains("-n_trials = 1"));
    assert!(message.contains("+n_trials = 2"));
}

#[test]
fn resume_allows_config_override_with_flag() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("argtuner").join("resume-override");
    let project = Project::new(&root);
    project.ensure_dirs().expect("dirs");

    let template = CommandTemplate::new(format!(
        "{} --checkpoint-dir {{trial_dir}}",
        emit_result_command()
    ));
    let space = SearchSpace {
        params: vec![argtuner::ParamSpec::Float {
            name: "lr".to_string(),
            min: 0.0,
            max: 1.0,
            log_scale: false,
            step: None,
            format: None,
            parent: None,
            parent_values: None,
        }],
    };

    let project_settings = ProjectSettings {
        metric_key: "metric".to_string(),
        goal: argtuner::Goal::Min,
        pruner: argtuner::Pruner::None,
        inject_trial_placeholders: true,
        checkpoint_arg: None,
    };
    let sampler = SamplerConfig {
        kind: Sampler::Random,
        ..SamplerConfig::default()
    };
    let mut scheduler = SchedulerConfig {
        kind: Scheduler::Fixed,
        n_trials: 1,
        seed: 42,
        ..SchedulerConfig::default()
    };
    let mut unified_config = argtuner::UnifiedConfig {
        project: project_settings,
        sampler,
        scheduler: scheduler.clone(),
        space,
        template: template.as_str().to_string(),
    };
    project
        .save_unified_config(&unified_config)
        .expect("save config");

    let tuner = Tuner::new(project.clone());
    tuner.run().expect("run");

    scheduler.seed = 99;
    unified_config.scheduler = scheduler;
    project
        .save_unified_config(&unified_config)
        .expect("save updated config");

    tuner
        .run_with_options(RunOptions {
            dry_run: false,
            allow_config_change: true,
        })
        .expect("override config");
}

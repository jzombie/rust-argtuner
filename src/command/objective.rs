use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

use crate::{
    CommandTemplate, Goal, SearchSpace, TrialOverrides, TrialRecord, TrialStatus, TrialStore,
    constants::{
        FIELD_METRIC, FIELD_SCORE, FIELD_TRIAL_BUDGET_STEP, FIELD_TRIAL_BUDGET_TOTAL,
        FIELD_TRIAL_CONFIG_ID, FIELD_TRIAL_ELAPSED_MS, FIELD_TRIAL_ERROR, FIELD_TRIAL_ID,
        FIELD_TRIAL_PARENT_ID, FIELD_TRIAL_STATUS,
    },
    render_trial_command_with_overrides,
};

fn tail_lines(output: &str, max_lines: usize) -> String {
    if max_lines == 0 {
        return String::new();
    }
    let lines: Vec<&str> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return String::new();
    }
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

enum EvalError {
    InvalidConfig {
        message: String,
        fields: BTreeMap<String, String>,
    },
    Other(String),
}

pub struct CommandObjective {
    store: TrialStore,
    template: CommandTemplate,
    space: SearchSpace,
    artifacts_dir: PathBuf,
    metric_key: String,
    goal: Goal,
    inject_trial_placeholders: bool,
    next_id: std::sync::Mutex<usize>,
    best_score: std::sync::Mutex<Option<f64>>,
}

impl CommandObjective {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: TrialStore,
        template: CommandTemplate,
        space: SearchSpace,
        artifacts_dir: PathBuf,
        metric_key: String,
        goal: Goal,
        inject_trial_placeholders: bool,
        next_id: usize,
    ) -> Self {
        Self {
            store,
            template,
            space,
            artifacts_dir,
            metric_key,
            goal,
            inject_trial_placeholders,
            next_id: std::sync::Mutex::new(next_id),
            best_score: std::sync::Mutex::new(None),
        }
    }

    pub fn eval(&self, coords: &[f64]) -> Result<f64, String> {
        let overrides = TrialOverrides::default();
        self.eval_with_overrides(coords, &overrides)
    }

    pub fn eval_with_overrides(
        &self,
        coords: &[f64],
        overrides: &TrialOverrides,
    ) -> Result<f64, String> {
        let trial_id = self.next_trial_id();
        self.eval_with_overrides_internal(coords, overrides, true, trial_id)
    }

    pub fn eval_with_overrides_retryable(
        &self,
        coords: &[f64],
        overrides: &TrialOverrides,
        trial_id: Option<usize>,
    ) -> Result<(f64, usize), (String, usize)> {
        let trial_id = trial_id.unwrap_or_else(|| self.next_trial_id());
        match self.eval_with_overrides_internal(coords, overrides, true, trial_id) {
            Ok(score) => Ok((score, trial_id)),
            Err(err) => Err((err, trial_id)),
        }
    }

    fn eval_with_overrides_internal(
        &self,
        coords: &[f64],
        overrides: &TrialOverrides,
        persist_invalid_config: bool,
        trial_id: usize,
    ) -> Result<f64, String> {
        // Load existing fields first to check for validity
        let existing_fields = match self.store.load_fields(trial_id) {
            Ok(Some(fields)) => Some(fields),
            Ok(None) => None,
            Err(err) => return Err(format!("trial log load failed: {err}")),
        };

        let mut effective_overrides = overrides.clone();

        if let Some(config_id_str) = effective_overrides.fields.get(FIELD_TRIAL_CONFIG_ID) {
            if let Ok(config_id) = config_id_str.parse::<usize>() {
                if let Ok(Some(parent_id)) = self.store.find_last_trial_for_config(config_id) {
                    effective_overrides
                        .fields
                        .insert(FIELD_TRIAL_PARENT_ID.to_string(), parent_id.to_string());
                }
            }
        }

        if let Some(existing) = &existing_fields {
            for (key, value) in existing {
                if let Some(param_name) = key.strip_prefix(crate::HP_PREFIX) {
                    if self.space.validate_value(param_name, value) {
                        effective_overrides
                            .values
                            .insert(param_name.to_string(), value.clone());
                        effective_overrides
                            .fields
                            .insert(key.clone(), value.clone());
                    } else {
                        let err_msg = format!(
                            "existing trial {trial_id} has invalid value for {param_name}: {value}"
                        );
                        let mut fields = existing.clone();
                        fields.remove(FIELD_TRIAL_STATUS);
                        fields.remove(FIELD_TRIAL_ERROR);
                        fields.remove(FIELD_TRIAL_ELAPSED_MS);
                        fields.remove(FIELD_TRIAL_ID);

                        self.store
                            .update(&TrialRecord {
                                trial_id,
                                status: TrialStatus::Error,
                                elapsed_ms: 0,
                                error: Some(err_msg.clone()),
                                fields,
                            })
                            .map_err(|e| format!("failed to update trial error: {e}"))?;
                        return Err(err_msg);
                    }
                }
            }
        }

        let rendered = render_trial_command_with_overrides(
            &self.template,
            &self.space,
            coords,
            trial_id,
            &self.artifacts_dir,
            self.inject_trial_placeholders,
            &effective_overrides,
        )
        .map_err(|err| err.to_string())?;
        if let Some(trial_dir) = rendered.trial_dir.as_ref() {
            if let Err(err) = std::fs::create_dir_all(trial_dir) {
                return Err(format!("trial artifacts dir failed: {err}"));
            }

            if let Some(parent_id_str) = effective_overrides.fields.get(FIELD_TRIAL_PARENT_ID) {
                if let Ok(parent_id) = parent_id_str.parse::<usize>() {
                    let parent_dir = self.artifacts_dir.join(format!("trial_{parent_id}"));
                    if parent_dir.exists() {
                        crate::utils::copy_dir_recursive(&parent_dir, trial_dir)
                            .map_err(|e| format!("failed to copy parent artifacts: {e}"))?;
                    }
                }
            }
        }
        let command = rendered.command;
        let start = std::time::Instant::now();

        let existing_fields_for_update = existing_fields;

        let existing_fields = match existing_fields_for_update {
            Some(existing) => {
                let fields = crate::trial::merge_running_fields(existing, &rendered.fields);
                let record = TrialRecord {
                    trial_id,
                    status: TrialStatus::Running,
                    elapsed_ms: 0,
                    error: None,
                    fields,
                };
                self.store
                    .update(&record)
                    .map_err(|err| format!("trial log update failed: {err}"))?;
                Some(record.fields)
            }
            None => {
                let record = TrialRecord {
                    trial_id,
                    status: TrialStatus::Running,
                    elapsed_ms: 0,
                    error: None,
                    fields: rendered.fields.clone(),
                };
                self.store
                    .append(&record)
                    .map_err(|err| format!("trial log append failed: {err}"))?;
                None
            }
        };
        crate::analysis::print_top_trials(&self.store, 1);

        eprintln!("\n===== Starting Trial {} =====", trial_id);
        use crate::{FIELD_TUNING_BUDGET_REMAINING, FIELD_TUNING_BUDGET_TOTAL};
        let budget_total = rendered.fields.get(FIELD_TRIAL_BUDGET_TOTAL).cloned();
        let budget_step = rendered.fields.get(FIELD_TRIAL_BUDGET_STEP).cloned();
        let tuning_total = rendered.fields.get(FIELD_TUNING_BUDGET_TOTAL).cloned();
        let tuning_remaining = rendered.fields.get(FIELD_TUNING_BUDGET_REMAINING).cloned();

        if budget_total.is_some() || budget_step.is_some() {
            let total_display = budget_total.as_deref().unwrap_or("?");
            let step_display = budget_step.as_deref().unwrap_or("?");
            eprint!("Budget: total={} step={}", total_display, step_display);
            if let (Some(tt), Some(tr)) = (tuning_total, tuning_remaining) {
                eprint!(" | Tuning: total={} remaining={}", tt, tr);
            }
            eprintln!();
        }
        eprintln!("Command: {}", command);
        let _ = std::io::stderr().flush();
        std::thread::sleep(std::time::Duration::from_millis(80));
        let result = (|| {
            let output = crate::command::CommandRunner::run(&command, &rendered.env)
                .map_err(EvalError::Other)?;
            if output.exit_code != 0 {
                let tail = tail_lines(&output.stdout, 1);
                let tail = if tail.is_empty() {
                    "<no output>".to_string()
                } else {
                    tail
                };
                return Err(EvalError::Other(format!(
                    "command exited with code {}: {}",
                    output.exit_code, tail
                )));
            }
            let payload = output
                .parse_payload(crate::RESULT_PREFIX)
                .map_err(EvalError::Other)?;
            let binding_version_key = format!(
                "{}.{}.{}",
                crate::TUNER_NAMESPACE,
                argtuner_common::BINDING_VERSION_EVENT,
                argtuner_common::BINDING_VERSION_FIELD
            );
            if let Some(version) = payload.data.get(&binding_version_key) {
                if version != argtuner_talkback::BINDING_VERSION {
                    eprintln!(
                        "binding version mismatch: expected {} got {}",
                        argtuner_talkback::BINDING_VERSION,
                        version
                    );
                    std::process::exit(2);
                }
            }
            let extra_fields = payload.to_fields();
            if payload.get_bool(argtuner_common::EventKind::InvalidConfig.as_str()) {
                let reason = payload
                    .data
                    .get("error")
                    .map(String::as_str)
                    .unwrap_or("invalid_config")
                    .to_string();
                return Err(EvalError::InvalidConfig {
                    message: format!("invalid_config: {reason}"),
                    fields: extra_fields,
                });
            }
            let metric = payload
                .get_metric(&self.metric_key)
                .map_err(EvalError::Other)?;
            let score = match self.goal {
                Goal::Min => metric,
                Goal::Max => -metric,
            };
            let epoch_results = payload.epoch_results.clone();
            let epoch_fields = payload.epoch_fields();
            Ok((metric, score, extra_fields, epoch_results, epoch_fields))
        })();
        match result {
            Ok((metric, score, extra_fields, epoch_results, epoch_fields)) => {
                let base_fields = rendered.fields;
                let mut out_fields = base_fields.clone();
                for (key, value) in extra_fields {
                    out_fields.entry(key).or_insert(value);
                }
                let metric_field = crate::trial::metric_value_field(&self.metric_key);
                out_fields.entry(metric_field).or_insert(metric.to_string());
                out_fields.insert(FIELD_METRIC.to_string(), self.metric_key.clone());
                out_fields.insert(FIELD_SCORE.to_string(), score.to_string());
                crate::trial::enforce_hp_immutability(existing_fields.as_ref(), &mut out_fields);
                for (epoch_result, epoch_row_fields) in
                    epoch_results.iter().zip(epoch_fields.iter())
                {
                    let epoch_metric = metric_from_map(epoch_result, &self.metric_key)
                        .map_err(|err| format!("epoch metric parse failed: {err}"))?;
                    let epoch_score = match self.goal {
                        Goal::Min => epoch_metric,
                        Goal::Max => -epoch_metric,
                    };
                    let mut epoch_fields = base_fields.clone();
                    for (key, value) in epoch_row_fields {
                        epoch_fields.entry(key.clone()).or_insert(value.clone());
                    }
                    let metric_field = crate::trial::metric_value_field(&self.metric_key);
                    epoch_fields
                        .entry(metric_field)
                        .or_insert(epoch_metric.to_string());
                    epoch_fields.insert(FIELD_METRIC.to_string(), self.metric_key.clone());
                    epoch_fields.insert(FIELD_SCORE.to_string(), epoch_score.to_string());
                    crate::trial::enforce_hp_immutability(existing_fields.as_ref(), &mut epoch_fields);
                    self.store
                        .append_epoch(&TrialRecord {
                            trial_id,
                            status: TrialStatus::Running,
                            elapsed_ms: start.elapsed().as_millis(),
                            error: None,
                            fields: epoch_fields,
                        })
                        .map_err(|err| format!("epoch log append failed: {err}"))?;
                }
                self.store
                    .update(&TrialRecord {
                        trial_id,
                        status: TrialStatus::Ok,
                        elapsed_ms: start.elapsed().as_millis(),
                        error: None,
                        fields: out_fields,
                    })
                    .map_err(|err| format!("trial log update failed: {err}"))?;
                if let Ok(mut best) = self.best_score.lock() {
                    let is_best = best.is_none_or(|value| score < value);
                    if is_best {
                        *best = Some(score);
                        println!("new best: trial={trial_id} metric={metric:.6} score={score:.6}");
                    }
                }
                eprintln!("\n===== Finished Trial {}: OK =====", trial_id);
                eprintln!("Metric: {}  Score: {:.6}", metric, score);
                let _ = std::io::stderr().flush();
                Ok(score)
            }
            Err(err) => {
                let mut extra_fields = BTreeMap::new();
                let err = match err {
                    EvalError::InvalidConfig { message, fields } => {
                        extra_fields = fields;
                        if persist_invalid_config {
                            message
                        } else {
                            return Err(message);
                        }
                    }
                    EvalError::Other(message) => message,
                };
                let mut out_fields = crate::trial::merge_error_fields(
                    existing_fields.as_ref(),
                    &rendered.fields,
                    extra_fields,
                );
                crate::trial::enforce_hp_immutability(existing_fields.as_ref(), &mut out_fields);
                self.store
                    .update(&TrialRecord {
                        trial_id,
                        status: TrialStatus::Error,
                        elapsed_ms: start.elapsed().as_millis(),
                        error: Some(err.clone()),
                        fields: out_fields,
                    })
                    .map_err(|update_err| {
                        format!("trial log update failed: {update_err}; original error: {err}")
                    })?;
                eprintln!("\n===== Finished Trial {}: ERROR =====", trial_id);
                eprintln!("Error: {}", err);
                let _ = std::io::stderr().flush();
                Err(err)
            }
        }
    }

    pub fn store(&self) -> &TrialStore {
        &self.store
    }

    pub fn dims(&self) -> usize {
        self.space.dims()
    }

    fn next_trial_id(&self) -> usize {
        let mut guard = self.next_id.lock().expect("trial id lock");
        let id = *guard;
        *guard += 1;
        id
    }
}

fn metric_from_map(map: &BTreeMap<String, String>, metric_key: &str) -> Result<f64, String> {
    let metric = map
        .get(metric_key)
        .ok_or_else(|| format!("result missing key '{metric_key}'"))?;
    let text = metric.trim();
    if text.eq_ignore_ascii_case("null") {
        return Ok(f64::NAN);
    }
    text.parse::<f64>()
        .map_err(|_| format!("result key '{metric_key}' not numeric"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emit_result_command() -> String {
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_emit_result") {
            path
        } else {
            "cargo run -q -p argtuner --bin emit_result --".to_string()
        }
    }

    fn emit_invalid_result_command() -> String {
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_emit_invalid_result") {
            path
        } else {
            "cargo run -q -p argtuner --bin emit_invalid_result --".to_string()
        }
    }

    #[test]
    fn objective_runs_command_and_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = crate::CommandTemplate::new(emit_result_command());
        let store = crate::TrialStore::new(
            dir.path().join(crate::TRIALS_CSV_FILENAME),
            template.clone(),
        );
        let space = crate::SearchSpace { params: vec![] };
        let objective = CommandObjective::new(
            store,
            template,
            space,
            dir.path().join("artifacts"),
            "metric".to_string(),
            crate::Goal::Min,
            true,
            0,
        );
        let score = objective.eval(&[]).expect("score");
        assert!((score - 0.42).abs() < 1e-6);
        let fields = objective
            .store()
            .load_fields(0)
            .expect("load fields")
            .expect("fields row");
        assert_eq!(fields.get("metric.last_epoch"), Some(&"7".to_string()));
        assert_eq!(
            fields.get(argtuner_common::MODEL_EARLY_STOPPED_EVENT),
            Some(&"true".to_string())
        );
    }

    #[test]
    fn objective_marks_trial_as_early_stopped_when_reported() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = crate::CommandTemplate::new(emit_result_command());
        let store = crate::TrialStore::new(
            dir.path().join(crate::TRIALS_CSV_FILENAME),
            template.clone(),
        );
        let space = crate::SearchSpace { params: vec![] };
        let objective = CommandObjective::new(
            store,
            template,
            space,
            dir.path().join("artifacts"),
            "metric".to_string(),
            crate::Goal::Min,
            true,
            0,
        );
        let _ = objective.eval(&[]).expect("score");
        let fields = objective
            .store()
            .load_fields(0)
            .expect("load fields")
            .expect("fields row");
        assert_eq!(
            fields.get(argtuner_common::MODEL_EARLY_STOPPED_EVENT),
            Some(&"true".to_string())
        );
        assert_eq!(
            fields.get(crate::FIELD_TRIAL_STATUS),
            Some(&"ok".to_string())
        );
    }

    #[test]
    fn objective_records_invalid_config_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = crate::CommandTemplate::new(emit_invalid_result_command());
        let store = crate::TrialStore::new(
            dir.path().join(crate::TRIALS_CSV_FILENAME),
            template.clone(),
        );
        let space = crate::SearchSpace { params: vec![] };
        let objective = CommandObjective::new(
            store,
            template,
            space,
            dir.path().join("artifacts"),
            "metric".to_string(),
            crate::Goal::Min,
            true,
            0,
        );
        let err = objective.eval(&[]).expect_err("invalid config");
        assert!(err.starts_with("invalid_config:"));
        let fields = objective
            .store()
            .load_fields(0)
            .expect("load fields")
            .expect("fields row");
        assert_eq!(
            fields.get(argtuner_common::MODEL_INVALID_CONFIG_EVENT),
            Some(&"true".to_string())
        );
        assert_eq!(
            fields.get(crate::FIELD_TRIAL_STATUS),
            Some(&"error".to_string())
        );
    }

    #[test]
    fn trial_csv_records_start_update_finish() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = crate::CommandTemplate::new(emit_result_command());
        let store = crate::TrialStore::new(
            dir.path().join(crate::TRIALS_CSV_FILENAME),
            template.clone(),
        );
        let space = crate::SearchSpace {
            params: vec![crate::ParamSpec::Float {
                name: "lr".to_string(),
                min: 0.001,
                max: 0.01,
                log_scale: false,
                step: None,
                format: None,
            }],
        };

        let template = crate::CommandTemplate::new(emit_result_command());
        let objective = CommandObjective::new(
            store,
            template,
            space.clone(),
            dir.path().join("artifacts"),
            "metric".to_string(),
            crate::Goal::Min,
            false,
            0,
        );

        let score = objective.eval(&[0.5]).expect("eval ok");
        let row = objective
            .store()
            .load_fields(0)
            .expect("load fields")
            .expect("missing row");
        assert_eq!(
            row.get(crate::FIELD_TRIAL_STATUS).map(String::as_str),
            Some("ok")
        );
        assert_eq!(
            row.get(crate::FIELD_METRIC).map(String::as_str),
            Some("metric")
        );
        let metric_value_key = crate::trial::metric_value_field("metric");
        assert_eq!(row.get(&metric_value_key).map(String::as_str), Some("0.42"));
        assert!(score.is_finite());

        let bad_template = crate::CommandTemplate::new("echo nope".to_string());
        let bad_objective = CommandObjective::new(
            crate::TrialStore::new(
                dir.path().join(crate::TRIALS_CSV_FILENAME),
                bad_template.clone(),
            ),
            bad_template,
            space,
            dir.path().join("artifacts"),
            "metric".to_string(),
            crate::Goal::Min,
            false,
            1,
        );
        let _err = bad_objective.eval(&[0.5]).expect_err("eval should fail");
        let failed = bad_objective
            .store()
            .load_fields(1)
            .expect("load failed")
            .expect("missing failed row");
        assert_eq!(
            failed.get(crate::FIELD_TRIAL_STATUS).map(String::as_str),
            Some("error")
        );
        assert!(failed.contains_key(crate::FIELD_TRIAL_ERROR));
    }

    fn emit_env_result_command() -> String {
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_emit_env_result") {
            path
        } else {
            let manifest = crate::workspace_root().join("Cargo.toml");
            format!(
                "cargo run -q --manifest-path \"{}\" -p argtuner --bin emit_env_result --",
                manifest.to_string_lossy()
            )
        }
    }

    #[test]
    fn objective_injects_trial_env_vars() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = crate::CommandTemplate::new(emit_env_result_command());
        let store = crate::TrialStore::new(
            dir.path().join(crate::TRIALS_CSV_FILENAME),
            template.clone(),
        );
        let space = crate::SearchSpace {
            params: vec![crate::ParamSpec::Float {
                name: "lr".to_string(),
                min: 0.0,
                max: 1.0,
                log_scale: false,
                step: None,
                format: None,
            }],
        };

        let template = crate::CommandTemplate::new(emit_env_result_command());

        let objective = CommandObjective::new(
            store,
            template,
            space,
            dir.path().join("artifacts"),
            "metric".to_string(),
            crate::Goal::Min,
            true,
            0,
        );

        let _ = objective.eval(&[0.5]).expect("eval");

        let fields = objective
            .store()
            .load_fields(0)
            .expect("load")
            .expect("row");
        assert_eq!(
            fields.get("metric.trial_id_env").map(String::as_str),
            Some("0")
        );

        let expected_dir = dir.path().join("artifacts").join("trial_0");
        assert_eq!(
            fields.get("metric.trial_dir_env").map(String::as_str),
            Some(expected_dir.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn csv_parameter_conflict_is_resolved_by_using_existing_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        // Template uses {x}
        let json =
            r#"{{"type":"event","name":"model.epoch_end","fields":{{"metric":"0.0","x_used":"{x}","epoch":"1"}}}}"#;
        let template =
            crate::CommandTemplate::new(format!("echo '{}{}'", crate::RESULT_PREFIX, json));
        let store = crate::TrialStore::new(
            dir.path().join(crate::TRIALS_CSV_FILENAME),
            template.clone(),
        );

        // Space defines x
        let space = crate::SearchSpace {
            params: vec![crate::ParamSpec::Float {
                name: "x".to_string(),
                min: 0.0,
                max: 1.0,
                log_scale: false,
                step: None,
                format: None,
            }],
        };

        // Pre-populate CSV with trial 0 having x=0.5
        store
            .append(&crate::TrialRecord {
                trial_id: 0,
                status: crate::TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields: [
                    (crate::FIELD_TRIAL_CONFIG_ID.to_string(), "0".to_string()),
                    (crate::FIELD_TRIAL_RUNG.to_string(), "0".to_string()),
                    (crate::FIELD_TRIAL_BRACKET.to_string(), "0".to_string()),
                    (
                        crate::FIELD_TRIAL_BUDGET_EPOCHS.to_string(),
                        "1".to_string(),
                    ),
                    ("hp.x".to_string(), "0.5".to_string()), // CSV says 0.5
                ]
                .into_iter()
                .collect(),
            })
            .expect("append");

        let objective = CommandObjective::new(
            store,
            template,
            space,
            dir.path().join("artifacts"),
            "metric".to_string(),
            crate::Goal::Min,
            true,
            0,
        );

        // Scheduler proposes x=0.8 (different from CSV)
        let coords = [0.8];
        let overrides = crate::TrialOverrides::default();

        // Run trial 0
        let result = objective.eval_with_overrides_retryable(&coords, &overrides, Some(0));

        match result {
            Ok(_) => {
                let fields = objective
                    .store()
                    .load_fields(0)
                    .expect("load")
                    .expect("row");
                assert_eq!(fields.get("metric.x_used").map(String::as_str), Some("0.5"));
            }
            Err(e) => panic!(
                "Should have succeeded by resolving conflict, but failed: {:?}",
                e
            ),
        }
    }

    #[test]
    fn csv_invalid_value_is_handled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = crate::CommandTemplate::new("echo --x {x}".to_string());
        let store = crate::TrialStore::new(
            dir.path().join(crate::TRIALS_CSV_FILENAME),
            template.clone(),
        );

        let space = crate::SearchSpace {
            params: vec![crate::ParamSpec::Float {
                name: "x".to_string(),
                min: 0.0,
                max: 1.0,
                log_scale: false,
                step: None,
                format: None,
            }],
        };

        // Pre-populate CSV with trial 0 having x="null" (invalid for Float)
        store
            .append(&crate::TrialRecord {
                trial_id: 0,
                status: crate::TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields: [
                    (crate::FIELD_TRIAL_CONFIG_ID.to_string(), "0".to_string()),
                    (crate::FIELD_TRIAL_RUNG.to_string(), "0".to_string()),
                    (crate::FIELD_TRIAL_BRACKET.to_string(), "0".to_string()),
                    (
                        crate::FIELD_TRIAL_BUDGET_EPOCHS.to_string(),
                        "1".to_string(),
                    ),
                    ("hp.x".to_string(), "null".to_string()),
                ]
                .into_iter()
                .collect(),
            })
            .expect("append");

        let objective = CommandObjective::new(
            store,
            template,
            space,
            dir.path().join("artifacts"),
            "metric".to_string(),
            crate::Goal::Min,
            true,
            1,
        );

        // Scheduler proposes x=0.5 (valid)
        let coords = [0.5];
        let overrides = crate::TrialOverrides::default();

        // Run trial 0
        let result = objective.eval_with_overrides_retryable(&coords, &overrides, Some(0));

        match result {
            Err((msg, _)) => {
                assert!(
                    msg.contains("invalid value"),
                    "Error message should mention invalid value: {}",
                    msg
                );
            }
            Ok(_) => panic!("Should have failed due to invalid value"),
        }
    }
}

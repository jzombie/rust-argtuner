use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::constants::{
    DUPLICATE_CONFIG_PREFIX, FIELD_TRIAL_ERROR, FIELD_TRIAL_ID, FIELD_TRIAL_STATUS, HP_PREFIX,
};

/// Wraps a `CommandObjective` with a stop flag check and optional trial cache.
///
/// The trial cache lets any sampler avoid re-running a command whose HP values
/// already have a recorded score in the store.  The cache is mutable so that
/// results from the current run are accumulated and revisited configs return
/// the cached score without hitting the duplicate-config check.
///
/// Populated via [`build_trial_result_cache`] and attached with
/// [`ControllableObjective::with_cache`].
pub struct ControllableObjective {
    inner: crate::command::CommandObjective,
    stop_flag: Arc<AtomicBool>,
    trial_cache: Option<Mutex<HashMap<Vec<(String, String)>, f64>>>,
}

impl ControllableObjective {
    pub fn new(objective: crate::command::CommandObjective, stop_flag: Arc<AtomicBool>) -> Self {
        Self {
            inner: objective,
            stop_flag,
            trial_cache: None,
        }
    }

    /// Attach a trial result cache so that previously-scored HP configurations
    /// are returned immediately without re-running the command.
    pub fn with_cache(mut self, cache: HashMap<Vec<(String, String)>, f64>) -> Self {
        self.trial_cache = Some(Mutex::new(cache));
        self
    }

    /// Build the cache key (sorted HP-prefixed field pairs) from `coords`.
    fn cache_key(&self, coords: &[f64]) -> Vec<(String, String)> {
        let fields = self.inner.space().fields_from_unit(coords);
        let mut hps: Vec<(String, String)> = fields
            .iter()
            .filter(|(k, _)| k.starts_with(HP_PREFIX))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        hps.sort_by(|a, b| a.0.cmp(&b.0));
        hps
    }

    /// Evaluate coords, returning an error if the stop flag is set.
    /// If a trial cache is attached and the resulting HP values have a hit,
    /// the cached score is returned without executing the command.
    /// Otherwise runs the command and inserts the result into the cache.
    ///
    /// If `inner.eval` returns a duplicate-config error, the cache is
    /// refreshed from the store (the duplicate's score may already be
    /// persisted) and the lookup is retried before propagating the
    /// error.  This handles the case where PSO visits the same HP
    /// configuration multiple times as particles converge.
    pub fn eval(&self, coords: &[f64]) -> Result<f64, String> {
        if self.stop_flag.load(Ordering::SeqCst) {
            return Err("interrupted by user".to_string());
        }

        // ---- trial cache check ----
        if let Some(ref cache) = self.trial_cache {
            let key = self.cache_key(coords);
            if let Some(score) = cache.lock().unwrap().get(&key) {
                return Ok(*score);
            }
        }

        let result = self.inner.eval(coords);

        // ---- cache insert on success ----
        if let Some(ref cache) = self.trial_cache {
            if let Ok(score) = result {
                let key = self.cache_key(coords);
                cache.lock().unwrap().insert(key, score);
            }
        }

        // ---- recover from duplicate-config error by refreshing cache ----
        if let Some(ref cache) = self.trial_cache {
            if let Err(ref err) = result {
                if err.starts_with(DUPLICATE_CONFIG_PREFIX) {
                    if let Ok(fresh) = build_trial_result_cache(self.inner.store()) {
                        let mut guard = cache.lock().unwrap();
                        *guard = fresh;
                        let key = self.cache_key(coords);
                        if let Some(score) = guard.get(&key) {
                            return Ok(*score);
                        }
                    }
                }
            }
        }

        result
    }

    pub fn inner(&self) -> &crate::command::CommandObjective {
        &self.inner
    }

    pub fn dims(&self) -> usize {
        self.inner.dims()
    }

    pub fn store(&self) -> &crate::trial::store::TrialStore {
        self.inner.store()
    }
}

/// A flag set to true when SIGINT (Ctrl-C) is received.
#[derive(Clone)]
pub struct StopFlag(Arc<AtomicBool>);

impl StopFlag {
    pub fn new() -> Self {
        let flag = Arc::new(AtomicBool::new(false));
        let f = flag.clone();
        let _ = ctrlc::set_handler(move || {
            eprintln!("\nShutdown requested (Ctrl-C). Finishing current work...");
            f.store(true, Ordering::SeqCst);
        });
        StopFlag(flag)
    }

    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    pub fn inner(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

impl Default for StopFlag {
    fn default() -> Self {
        Self::new()
    }
}

/// Find all trials with status=`Running`, delete their artifact directories,
/// and mark them as `Error`.  Returns the number of trials swept.
///
/// This is called at the start of a resume to clean up stale entries left
/// by a Ctrl-C'd run so that:
///   - PSO's duplicate-config check does not block re-evaluation
///   - SHA promotion does not copy partial artifacts from the interrupted parent
pub fn sweep_stale_running_trials(
    store: &crate::trial::store::TrialStore,
    artifacts_dir: &Path,
) -> Result<usize, String> {
    use crate::trial::store::{TrialRecord, TrialStatus};

    let rows = store
        .load_rows()
        .map_err(|e| format!("failed to load trials for sweep: {e}"))?;

    let mut count = 0;
    for row in &rows {
        let is_running = row
            .get(FIELD_TRIAL_STATUS)
            .and_then(|s| s.parse::<TrialStatus>().ok())
            .map_or(false, |s| s == TrialStatus::Running);
        if !is_running {
            continue;
        }

        let trial_id = match row
            .get(FIELD_TRIAL_ID)
            .and_then(|v| v.parse::<usize>().ok())
        {
            Some(id) => id,
            None => continue,
        };

        // Delete the artifact directory for this trial.
        let trial_dir = artifacts_dir.join(format!("trial_{trial_id}"));
        if trial_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&trial_dir) {
                eprintln!("WARN: failed to remove stale trial dir {trial_dir:?}: {e}");
            }
        }

        // Mark the trial as Error so it doesn't block future evaluations.
        let mut fields: std::collections::BTreeMap<String, String> = row
            .iter()
            .filter(|(k, _)| *k != FIELD_TRIAL_STATUS)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        fields.insert(
            FIELD_TRIAL_ERROR.to_string(),
            "interrupted by previous run".to_string(),
        );

        if let Err(e) = store.update(&TrialRecord {
            trial_id,
            status: TrialStatus::Error,
            elapsed_ms: 0,
            error: Some("interrupted by previous run".to_string()),
            fields,
        }) {
            eprintln!("WARN: failed to update stale trial {trial_id}: {e}");
        }

        count += 1;
    }

    Ok(count)
}

/// Build a cache of completed trials. Maps sorted HP-field pairs → score.
/// Lets any sampler avoid re-running already-completed configs on resume.
pub fn build_trial_result_cache(
    store: &crate::trial::store::TrialStore,
) -> Result<HashMap<Vec<(String, String)>, f64>, String> {
    use crate::constants::FIELD_SCORE;
    use crate::trial::store::TrialStatus;

    let rows = store
        .load_rows()
        .map_err(|e| format!("failed to load trials for cache: {e}"))?;

    let mut cache = HashMap::new();

    for row in &rows {
        let status = row
            .get(FIELD_TRIAL_STATUS)
            .and_then(|s| s.parse::<TrialStatus>().ok())
            .map_or(false, |s| s == TrialStatus::Ok || s == TrialStatus::Error);
        if !status {
            continue;
        }
        let Some(score) = row.get(FIELD_SCORE).and_then(|v| v.parse::<f64>().ok()) else {
            continue;
        };

        let mut hps: Vec<(String, String)> = row
            .iter()
            .filter(|(k, _)| k.starts_with(HP_PREFIX))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        hps.sort_by(|a, b| a.0.cmp(&b.0));

        if !hps.is_empty() {
            cache.insert(hps, score);
        }
    }

    Ok(cache)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CommandTemplate;
    use crate::TRIALS_CSV_FILENAME;
    use crate::constants::{FIELD_SCORE, HP_PREFIX};
    use crate::trial::store::{TrialRecord, TrialStatus, TrialStore};
    use std::collections::BTreeMap;

    #[test]
    fn stop_flag_default_is_false() {
        let flag = StopFlag::new();
        assert!(!flag.is_set());
    }

    #[test]
    fn trial_cache_builds_from_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = CommandTemplate::new("echo".to_string());
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template);

        let mut fields = BTreeMap::new();
        fields.insert(format!("{HP_PREFIX}lr"), "0.1".to_string());
        fields.insert(FIELD_SCORE.to_string(), "0.5".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Ok,
                elapsed_ms: 5,
                error: None,
                fields,
            })
            .expect("append");

        let cache = build_trial_result_cache(&store).expect("cache");
        assert_eq!(cache.len(), 1);

        let key: Vec<(String, String)> = vec![(format!("{HP_PREFIX}lr"), "0.1".to_string())];
        assert_eq!(cache.get(&key), Some(&0.5));
    }

    #[test]
    fn trial_cache_skips_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = CommandTemplate::new("echo".to_string());
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template);

        let mut fields = BTreeMap::new();
        fields.insert(format!("{HP_PREFIX}lr"), "0.1".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields,
            })
            .expect("append");

        let cache = build_trial_result_cache(&store).expect("cache");
        assert!(cache.is_empty(), "running trials should be excluded");
    }

    #[test]
    fn controllable_objective_returns_cached_score() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = crate::test_support::bin_command("emit_result");
        let template = CommandTemplate::new(template);
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template.clone());

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
        let objective = crate::command::CommandObjective::new(
            store,
            template,
            space,
            dir.path().join("artifacts"),
            "metric".to_string(),
            crate::Goal::Min,
            false,
            0,
        );

        let mut cache = HashMap::new();
        cache.insert(vec![(format!("{HP_PREFIX}x"), "0.5".to_string())], 0.42);

        let ctrl = ControllableObjective::new(objective, Arc::new(AtomicBool::new(false)))
            .with_cache(cache);

        let score = ctrl.eval(&[0.5]).expect("eval");
        assert_eq!(score, 0.42);
    }

    // -----------------------------------------------------------------------
    // Sweep tests
    // -----------------------------------------------------------------------

    #[test]
    // Scenario: a Ctrl-C'd run left a trial with status=Running and an
    // artifact directory.  The sweep must delete the directory and mark the
    // trial as Error so that PSO's duplicate check doesn't block it.
    fn sweep_removes_artifact_dir_and_marks_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = CommandTemplate::new("echo".to_string());
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template);

        let artifacts_dir = dir.path().join("artifacts");
        let trial_dir = artifacts_dir.join("trial_0");
        std::fs::create_dir_all(&trial_dir).expect("create trial dir");
        std::fs::write(trial_dir.join("checkpoint.pt"), "partial").expect("write");

        let mut fields = BTreeMap::new();
        fields.insert(format!("{HP_PREFIX}x"), "0.5".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields,
            })
            .expect("append");

        let count = sweep_stale_running_trials(&store, &artifacts_dir).expect("sweep");
        assert_eq!(count, 1, "should sweep one trial");

        // Artifact directory should be gone.
        assert!(!trial_dir.exists(), "trial dir should be deleted");

        // Trial should now be Error.
        let rows = store.load_rows().expect("load rows");
        let row = rows
            .iter()
            .find(|r| r.get("trial_id") == Some(&"0".to_string()))
            .expect("trial 0 should exist");
        assert_eq!(
            row.get(crate::FIELD_TRIAL_STATUS).map(String::as_str),
            Some("error")
        );
        assert_eq!(
            row.get(crate::FIELD_TRIAL_ERROR).map(String::as_str),
            Some("interrupted by previous run")
        );
    }

    #[test]
    // Scenario: a Running trial with no artifact directory (e.g. Ctrl-C
    // happened before the dir was created).  The sweep should still mark it
    // as Error without crashing.
    fn sweep_handles_missing_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = CommandTemplate::new("echo".to_string());
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template);

        let mut fields = BTreeMap::new();
        fields.insert(format!("{HP_PREFIX}x"), "0.5".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields,
            })
            .expect("append");

        // Sweep when artifact dir doesn't exist — should not crash.
        let artifacts_dir = dir.path().join("artifacts");
        let count = sweep_stale_running_trials(&store, &artifacts_dir).expect("sweep");
        assert_eq!(count, 1, "should sweep one trial");

        // Trial should now be Error.
        let rows = store.load_rows().expect("load rows");
        let row = rows
            .iter()
            .find(|r| r.get("trial_id") == Some(&"0".to_string()))
            .expect("trial 0 should exist");
        assert_eq!(
            row.get(crate::FIELD_TRIAL_STATUS).map(String::as_str),
            Some("error")
        );
    }

    #[test]
    // Scenario: only Running trials are swept; Ok and Error trials are
    // left untouched.
    fn sweep_ignores_non_running() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = CommandTemplate::new("echo".to_string());
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template);
        let artifacts_dir = dir.path().join("artifacts");

        // An Ok trial with artifacts.
        let ok_dir = artifacts_dir.join("trial_0");
        std::fs::create_dir_all(&ok_dir).expect("create");
        let mut ok_fields = BTreeMap::new();
        ok_fields.insert(format!("{HP_PREFIX}x"), "0.5".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Ok,
                elapsed_ms: 100,
                error: None,
                fields: ok_fields,
            })
            .expect("append");

        // An Error trial with artifacts.
        let err_dir = artifacts_dir.join("trial_1");
        std::fs::create_dir_all(&err_dir).expect("create");
        let mut err_fields = BTreeMap::new();
        err_fields.insert(format!("{HP_PREFIX}x"), "0.6".to_string());
        store
            .append(&TrialRecord {
                trial_id: 1,
                status: TrialStatus::Error,
                elapsed_ms: 50,
                error: Some("previous error".to_string()),
                fields: err_fields,
            })
            .expect("append");

        let count = sweep_stale_running_trials(&store, &artifacts_dir).expect("sweep");
        assert_eq!(count, 0, "should not sweep any trials");

        // Both directories should still exist.
        assert!(ok_dir.exists(), "Ok trial dir should remain");
        assert!(err_dir.exists(), "Error trial dir should remain");
    }

    #[test]
    // Scenario: `find_duplicate_config` (used by PSO) already skips
    // Running and Error trials.  The sweep marks stale Running trials as
    // Error, confirming they stay skipped — no regression.
    fn sweep_does_not_regress_duplicate_check() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = CommandTemplate::new("echo".to_string());
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template);

        // Create a Running trial (simulates Ctrl-C).
        let mut fields = BTreeMap::new();
        fields.insert(format!("{HP_PREFIX}x"), "0.5".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields,
            })
            .expect("append");

        // Even before sweep, Running trials should not block (the
        // duplicate check only considers Ok trials).
        let mut query = BTreeMap::new();
        query.insert(format!("{HP_PREFIX}x"), "0.5".to_string());
        let dup = store.find_duplicate_config(None, &query).expect("find");
        assert!(dup.is_none(), "Running trial should not block PSO");

        // Run sweep — marks it as Error.
        let artifacts_dir = dir.path().join("artifacts");
        sweep_stale_running_trials(&store, &artifacts_dir).expect("sweep");

        // After sweep: Error trials should also not block.
        let dup_after = store.find_duplicate_config(None, &query).expect("find");
        assert!(
            dup_after.is_none(),
            "Error trial should not block after sweep"
        );
    }
}

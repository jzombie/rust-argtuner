use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use line_ending::LineEnding;

use crate::constants::{DUPLICATE_CONFIG_PREFIX, FIELD_TRIAL_ID, FIELD_TRIAL_STATUS, HP_PREFIX};

/// Maps sorted (key, value) HP pairs → previously-scored cost.
type TrialCache = HashMap<Vec<(String, String)>, f64>;

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
    trial_cache: Option<Mutex<TrialCache>>,
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
    pub fn with_cache(mut self, cache: TrialCache) -> Self {
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
        if let (Some(cache), Ok(score)) = (self.trial_cache.as_ref(), result.as_ref()) {
            let key = self.cache_key(coords);
            cache.lock().unwrap().insert(key, *score);
        }

        // ---- recover from duplicate-config error by refreshing cache ----
        if let (Some(cache), Err(err)) = (self.trial_cache.as_ref(), result.as_ref())
            && err.starts_with(DUPLICATE_CONFIG_PREFIX)
            && let Ok(fresh) = build_trial_result_cache(self.inner.store())
        {
            let mut guard = cache.lock().unwrap();
            *guard = fresh;
            let key = self.cache_key(coords);
            if let Some(score) = guard.get(&key) {
                return Ok(*score);
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
            eprintln!(
                "{}Shutdown requested (Ctrl-C). Finishing current work...",
                LineEnding::from_current_platform().as_str()
            );
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
/// and reset them to a clean state so the sampler can re-issue `eval` with
/// the same `trial_id` and restart the interrupted command.
///
/// This is called at the start of a resume to clean up stale entries left
/// by a Ctrl-C'd run so that:
///   - PSO's duplicate-config check does not block re-evaluation
///   - SHA promotion does not copy partial artifacts from the interrupted parent
///   - The trial slot is preserved and the command re-runs with a fresh artifact dir
pub fn sweep_stale_running_trials(
    store: &crate::trial::store::TrialStore,
    artifacts_dir: &Path,
) -> Result<usize, String> {
    use crate::trial::store::TrialStatus;

    let rows = store
        .load_rows()
        .map_err(|e| format!("failed to load trials for sweep: {e}"))?;

    let mut count = 0;
    for row in &rows {
        let is_running = row
            .get(FIELD_TRIAL_STATUS)
            .and_then(|s| s.parse::<TrialStatus>().ok())
            == Some(TrialStatus::Running);
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

        // Delete the stale artifact directory.
        let trial_dir = artifacts_dir.join(format!("trial_{trial_id}"));
        if trial_dir.exists()
            && let Err(e) = std::fs::remove_dir_all(&trial_dir)
        {
            eprintln!("WARN: failed to remove stale trial dir {trial_dir:?}: {e}");
        }

        // Reset the trial record to a clean Running state.
        if let Err(e) = store.reset_trial(trial_id) {
            eprintln!("WARN: failed to reset stale trial {trial_id}: {e}");
        }

        eprintln!("INFO: reset stale trial {trial_id} (interrupted by previous run)");
        count += 1;
    }

    if count > 0 {
        eprintln!("INFO: cleaned up {count} stale trial(s) from previous run");
    }

    Ok(count)
}

/// Build a cache of completed trials. Maps sorted HP-field pairs → score.
/// Lets any sampler avoid re-running already-completed configs on resume.
pub fn build_trial_result_cache(
    store: &crate::trial::store::TrialStore,
) -> Result<TrialCache, String> {
    use crate::constants::FIELD_SCORE;
    use crate::trial::store::TrialStatus;

    let rows = store
        .load_rows()
        .map_err(|e| format!("failed to load trials for cache: {e}"))?;

    let mut cache = TrialCache::new();

    for row in &rows {
        let status = row
            .get(FIELD_TRIAL_STATUS)
            .and_then(|s| s.parse::<TrialStatus>().ok())
            .is_some_and(|s| s == TrialStatus::Ok || s == TrialStatus::Error);
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
        let template = crate::test_support::bin_command("mock_emit_result");
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
                parent: None,
                parent_values: None,
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
    // artifact directory.  The sweep must delete the directory and reset
    // the trial to a clean Running state so the sampler can re-issue eval
    // with the same trial_id and restart the interrupted command.
    fn sweep_resets_running_trial() {
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

        // Trial should still be Running (reset, not Error).
        let rows = store.load_rows().expect("load rows");
        let row = rows
            .iter()
            .find(|r| r.get("trial_id") == Some(&"0".to_string()))
            .expect("trial 0 should exist");
        assert_eq!(
            row.get(crate::FIELD_TRIAL_STATUS).map(String::as_str),
            Some("running")
        );
        // Fields should be empty (cleared by reset).
        assert!(
            row.get(format!("{HP_PREFIX}x").as_str())
                .map(String::as_str)
                .unwrap_or("")
                .is_empty(),
            "HP fields should be cleared after reset"
        );
        // Error should be empty (cleared by reset, fill_row sets header to "").
        assert_eq!(
            row.get(crate::FIELD_TRIAL_ERROR).map(String::as_str),
            Some(""),
            "error should be cleared after reset"
        );
    }

    #[test]
    // Scenario: a Running trial with no artifact directory (e.g. Ctrl-C
    // happened before the dir was created).  The sweep should still reset
    // it without crashing.
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

        // Trial should still be Running (reset).
        let rows = store.load_rows().expect("load rows");
        let row = rows
            .iter()
            .find(|r| r.get("trial_id") == Some(&"0".to_string()))
            .expect("trial 0 should exist");
        assert_eq!(
            row.get(crate::FIELD_TRIAL_STATUS).map(String::as_str),
            Some("running")
        );
    }

    #[test]
    // Scenario: only Running trials are swept; Ok and Error trials are
    // left completely untouched.
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
    // Scenario: `find_duplicate_config` (used by PSO) skips Running trials.
    // After sweep resets the trial, it stays Running so it still doesn't
    // block.  No regression.
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

        // Run sweep — resets the trial to clean Running.
        let artifacts_dir = dir.path().join("artifacts");
        sweep_stale_running_trials(&store, &artifacts_dir).expect("sweep");

        // After sweep: still Running so still should not block.
        let dup_after = store.find_duplicate_config(None, &query).expect("find");
        assert!(
            dup_after.is_none(),
            "reset Running trial should not block after sweep"
        );
    }

    #[test]
    // Scenario: after sweep, a reset Running trial can be re-evaluated with
    // the same trial_id via eval_with_overrides_retryable (simulating the
    // sampler re-issuing the interrupted trial).  The eval code path takes
    // the "existing" branch (non-None load_fields) and renders fresh fields.
    fn sweep_allows_re_eval_with_same_trial_id() {
        let dir = tempfile::tempdir().expect("tempdir");

        let template =
            CommandTemplate::new(crate::test_support::bin_command("mock_emit_env_result"));
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template.clone());

        let space = crate::SearchSpace {
            params: vec![crate::ParamSpec::Float {
                name: "x".to_string(),
                min: 0.0,
                max: 1.0,
                log_scale: false,
                step: None,
                format: None,
                parent: None,
                parent_values: None,
            }],
        };

        let objective = crate::command::CommandObjective::new(
            store,
            template,
            space,
            dir.path().join("artifacts"),
            "metric".to_string(),
            crate::Goal::Min,
            true,
            0,
        );

        // Create a stale Running trial.
        let mut fields = BTreeMap::new();
        fields.insert(format!("{HP_PREFIX}x"), "0.5".to_string());
        objective
            .store()
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields,
            })
            .expect("append");

        // Sweep it.
        sweep_stale_running_trials(objective.store(), &dir.path().join("artifacts"))
            .expect("sweep");

        // Now re-evaluate with the same trial_id — should succeed.
        let mut overrides = crate::TrialOverrides::default();
        overrides
            .fields
            .insert(crate::FIELD_TRIAL_CONFIG_ID.to_string(), "0".to_string());
        overrides
            .fields
            .insert(format!("{HP_PREFIX}x"), "0.5".to_string());

        let result = objective.eval_with_overrides_retryable(&[0.5], &overrides, Some(0));
        assert!(
            result.is_ok(),
            "should allow re-eval with same trial_id after sweep: {:?}",
            result.err()
        );
    }

    #[test]
    // Scenario: an Ok trial is a parent in an SHA chain (has config_id).
    // A stale Running trial also exists.  Sweep must only touch the Running
    // trial and leave the Ok parent completely untouched.
    fn sweep_does_not_touch_parent_trial() {
        let dir = tempfile::tempdir().expect("tempdir");
        let template = CommandTemplate::new("echo".to_string());
        let store = TrialStore::new(dir.path().join(TRIALS_CSV_FILENAME), template);
        let artifacts_dir = dir.path().join("artifacts");

        // Ok parent trial (config_id=0) with artifact dir and fields.
        let parent_dir = artifacts_dir.join("trial_0");
        std::fs::create_dir_all(&parent_dir).expect("create");
        std::fs::write(parent_dir.join("checkpoint.pt"), "weights").expect("write");
        let mut parent_fields = BTreeMap::new();
        parent_fields.insert(crate::FIELD_TRIAL_CONFIG_ID.to_string(), "0".to_string());
        parent_fields.insert(format!("{HP_PREFIX}x"), "0.5".to_string());
        parent_fields.insert(crate::FIELD_SCORE.to_string(), "0.42".to_string());
        store
            .append(&TrialRecord {
                trial_id: 0,
                status: TrialStatus::Ok,
                elapsed_ms: 100,
                error: None,
                fields: parent_fields,
            })
            .expect("append parent");

        // Stale Running trial (config_id=1) with artifact dir.
        let stale_dir = artifacts_dir.join("trial_1");
        std::fs::create_dir_all(&stale_dir).expect("create");
        std::fs::write(stale_dir.join("partial.pt"), "partial").expect("write");
        let mut stale_fields = BTreeMap::new();
        stale_fields.insert(crate::FIELD_TRIAL_CONFIG_ID.to_string(), "1".to_string());
        stale_fields.insert(format!("{HP_PREFIX}x"), "0.6".to_string());
        store
            .append(&TrialRecord {
                trial_id: 1,
                status: TrialStatus::Running,
                elapsed_ms: 0,
                error: None,
                fields: stale_fields,
            })
            .expect("append stale");

        // Run sweep.
        let count = sweep_stale_running_trials(&store, &artifacts_dir).expect("sweep");
        assert_eq!(count, 1, "should sweep only the Running trial");

        // Parent trial is completely untouched.
        assert!(parent_dir.exists(), "parent trial dir should still exist");
        assert!(
            parent_dir.join("checkpoint.pt").exists(),
            "parent checkpoint should still exist"
        );
        let rows = store.load_rows().expect("load rows");
        let parent_row = rows
            .iter()
            .find(|r| r.get("trial_id") == Some(&"0".to_string()))
            .expect("parent should exist");
        assert_eq!(
            parent_row
                .get(crate::FIELD_TRIAL_STATUS)
                .map(String::as_str),
            Some("ok"),
            "parent status unchanged"
        );
        assert_eq!(
            parent_row.get(crate::FIELD_SCORE).map(String::as_str),
            Some("0.42"),
            "parent score unchanged"
        );
        // find_last_trial_for_config should still return the parent.
        let parent_id = store
            .find_last_trial_for_config(0)
            .expect("find parent")
            .expect("should find parent trial");
        assert_eq!(parent_id, 0, "parent trial relationship intact");

        // Stale trial dir is gone.
        assert!(!stale_dir.exists(), "stale trial dir should be deleted");
    }
}

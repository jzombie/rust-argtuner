use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Wraps a `CommandObjective` with a stop flag check.
/// Any sampler can use this — it encapsulates the "check for Ctrl-C
/// before a trial" concern so the sampler logic stays self-contained.
pub struct ControllableObjective {
    inner: crate::command::CommandObjective,
    stop_flag: Arc<AtomicBool>,
}

impl ControllableObjective {
    pub fn new(
        objective: crate::command::CommandObjective,
        stop_flag: Arc<AtomicBool>,
    ) -> Self {
        Self {
            inner: objective,
            stop_flag,
        }
    }

    /// Evaluate coords, returning an error if the stop flag is set.
    pub fn eval(&self, coords: &[f64]) -> Result<f64, String> {
        if self.stop_flag.load(Ordering::SeqCst) {
            return Err("interrupted by user".to_string());
        }
        self.inner.eval(coords)
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

/// Build a cache of completed trials. Maps sorted HP-field pairs → score.
/// Lets any sampler avoid re-running already-completed configs on resume.
pub fn build_trial_result_cache(
    store: &crate::trial::store::TrialStore,
) -> Result<HashMap<Vec<(String, String)>, f64>, String> {
    use crate::constants::{FIELD_SCORE, FIELD_TRIAL_STATUS, HP_PREFIX};

    let rows = store
        .load_rows()
        .map_err(|e| format!("failed to load trials for cache: {e}"))?;

    let mut cache = HashMap::new();

    for row in &rows {
        let status = match row.get(FIELD_TRIAL_STATUS).map(|s| s.as_str()) {
            Some("ok") | Some("error") => true,
            _ => false,
        };
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
    use crate::constants::{FIELD_SCORE, HP_PREFIX};
    use crate::trial::store::{TrialRecord, TrialStatus, TrialStore};
    use crate::CommandTemplate;
    use crate::TRIALS_CSV_FILENAME;
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

        let key: Vec<(String, String)> =
            vec![(format!("{HP_PREFIX}lr"), "0.1".to_string())];
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
}

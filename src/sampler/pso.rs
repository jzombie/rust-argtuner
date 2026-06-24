use std::error::Error;
use std::sync::Arc;

use argmin::core::observers::{Observe, ObserverMode};
use argmin::core::PopulationState;
use argmin::core::{CostFunction, Executor, KV, State};
use argmin::solver::particleswarm::Particle;
use argmin::solver::particleswarm::ParticleSwarm;

use crate::checkpoint::ControllableObjective;
use crate::trial::store::TrialStore;

// ---------------------------------------------------------------------------
// Metadata keys stored in the project database
// ---------------------------------------------------------------------------

const PSO_CHECKPOINT_KEY: &str = "pso_checkpoint";

// ---------------------------------------------------------------------------
// Observer that periodically saves a PSO checkpoint
// ---------------------------------------------------------------------------

struct PsoCheckpointSaver {
    store: TrialStore,
    frequency: u64,
}

impl Observe<PopulationState<Particle<Vec<f64>, f64>, f64>> for PsoCheckpointSaver {
    fn observe_iter(
        &mut self,
        state: &PopulationState<Particle<Vec<f64>, f64>, f64>,
        _kv: &KV,
    ) -> Result<(), argmin::core::Error> {
        if state.iter % self.frequency == 0 {
            save_checkpoint(&self.store, state).map_err(|e| {
                argmin::core::Error::msg(format!("checkpoint save failed: {e}"))
            })?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Checkpoint persistence via project database metadata
// ---------------------------------------------------------------------------

/// Replace non-finite float fields so that `serde_json` can round-trip the
/// state without error.  `serde_json` by default rejects `NaN`, `Infinity`,
/// and `-Infinity`.
fn sanitize_state(
    state: &PopulationState<Particle<Vec<f64>, f64>, f64>,
) -> PopulationState<Particle<Vec<f64>, f64>, f64> {
    let mut s = state.clone();
    if !s.cost.is_finite() {
        s.cost = 0.0;
    }
    if !s.prev_cost.is_finite() {
        s.prev_cost = 0.0;
    }
    if !s.best_cost.is_finite() {
        s.best_cost = 0.0;
    }
    if !s.prev_best_cost.is_finite() {
        s.prev_best_cost = 0.0;
    }
    if !s.target_cost.is_finite() {
        s.target_cost = 0.0;
    }
    // Also sanitize the cost fields inside each Particle (they are Eq+Clone
    // but we need to rebuild the Vec).  Particle itself does not expose
    // setters for `cost` or `velocity`, so we work at the state level.
    // After a few iterations these will always be finite.
    s
}

fn save_checkpoint(
    store: &TrialStore,
    state: &PopulationState<Particle<Vec<f64>, f64>, f64>,
) -> Result<(), String> {
    let sanitized = sanitize_state(state);
    let json = serde_json::to_string(&sanitized)
        .map_err(|e| format!("failed to serialize PSO checkpoint: {e}"))?;
    store
        .save_metadata(PSO_CHECKPOINT_KEY, &json)
        .map_err(|e| format!("failed to save PSO checkpoint: {e}"))?;
    Ok(())
}

fn load_checkpoint(
    store: &TrialStore,
) -> Result<Option<PopulationState<Particle<Vec<f64>, f64>, f64>>, String> {
    let json = store
        .load_metadata(PSO_CHECKPOINT_KEY)
        .map_err(|e| format!("failed to load PSO checkpoint: {e}"))?;
    match json {
        None => Ok(None),
        Some(j) => {
            if j.is_empty() {
                return Ok(None);
            }
            match serde_json::from_str(&j) {
                Ok(state) => Ok(Some(state)),
                Err(e) => {
                    eprintln!("WARN: ignoring corrupt PSO checkpoint ({e})");
                    let _ = store.save_metadata(PSO_CHECKPOINT_KEY, "");
                    Ok(None)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public PSO entry point
// ---------------------------------------------------------------------------

pub fn run_pso(
    objective: ControllableObjective,
    iters: usize,
    particles: usize,
) -> Result<(), Box<dyn Error>> {
    #[derive(Clone)]
    struct ObjWrapper(Arc<ControllableObjective>);

    impl CostFunction for ObjWrapper {
        type Param = Vec<f64>;
        type Output = f64;

        fn cost(&self, param: &Self::Param) -> Result<Self::Output, argmin::core::Error> {
            self.0
                .eval(param)
                .map_err(|e| argmin::core::Error::msg(e))
        }
    }

    let dims = objective.dims();
    let lower = vec![0.0; dims];
    let upper = vec![1.0; dims];
    let store = objective.store().clone();

    // ---- checkpoint loading (warm restart) ----
    let loaded_state = load_checkpoint(&store)?.and_then(|mut s| {
        let pop_len = s.population.as_ref().map(|p| p.len()).unwrap_or(0);
        if pop_len == particles {
            s.max_iters = s.iter + iters as u64;
            Some(s)
        } else {
            if pop_len > 0 {
                eprintln!(
                    "INFO: particle count mismatch (saved: {pop_len}, \
                     requested: {particles}); starting fresh"
                );
            }
            None
        }
    });

    let solver = ParticleSwarm::new((lower, upper), particles);
    let obj_wrapper = ObjWrapper(Arc::new(objective));

    // ---- build executor (with optional warm-start) ----
    let mut executor = Executor::new(obj_wrapper, solver).configure(
        |state: PopulationState<Particle<Vec<f64>, f64>, f64>| {
            loaded_state.unwrap_or_else(|| state.max_iters(iters as u64))
        },
    );

    // ---- periodic checkpoint observer ----
    let frequency = (iters as u64 / 10).max(1);
    executor = executor.add_observer(
        PsoCheckpointSaver {
            store: store.clone(),
            frequency,
        },
        ObserverMode::Always,
    );

    let result = executor.run();

    // ---- final checkpoint save (on success) ----
    if let Ok(ref result) = result {
        let _ = save_checkpoint(&store, &result.state);
    }

    match result {
        Ok(result) => {
            eprintln!(
                "INFO: PSO finished after {} iterations (best cost: {:?})",
                result.state.get_iter(),
                result.state.best_cost,
            );
            Ok(())
        }
        Err(e) => {
            let msg = format!("{e}");
            if msg.contains("interrupted by user") {
                eprintln!("INFO: PSO interrupted by user; partial results saved in store");
                Ok(())
            } else {
                Err(e.into())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    fn dummy_store(dir: &tempfile::TempDir) -> TrialStore {
        TrialStore::new(
            dir.path().join(crate::TRIALS_CSV_FILENAME),
            crate::CommandTemplate::new(String::new()),
        )
    }

    fn make_state() -> PopulationState<Particle<Vec<f64>, f64>, f64> {
        let particle = Particle::new(vec![0.1, 0.2], 0.42, vec![0.01, 0.02]);
        let mut state: PopulationState<Particle<Vec<f64>, f64>, f64> =
            PopulationState::new();
        state.max_iters = 100;
        state.population = Some(vec![particle]);
        state.cost = 0.42;
        state.best_cost = 0.42;
        state.prev_cost = 0.42;
        state.prev_best_cost = 0.42;
        state.target_cost = 0.0; // finite
        state.best_individual = Some(Particle::new(vec![0.1, 0.2], 0.42, vec![0.01, 0.02]));
        state.individual = Some(Particle::new(vec![0.1, 0.2], 0.42, vec![0.01, 0.02]));
        state.prev_individual = Some(Particle::new(vec![0.1, 0.2], 0.42, vec![0.01, 0.02]));
        state.prev_best_individual = Some(Particle::new(vec![0.1, 0.2], 0.42, vec![0.01, 0.02]));
        state.iter = 10;
        state
    }

    #[test]
    fn checkpoint_save_load_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dummy_store(&dir);

        let state = make_state();
        save_checkpoint(&store, &state).expect("save");

        // Load from a fresh store reference (same SQLite file).
        let store2 = dummy_store(&dir);
        let meta = store2
            .load_metadata(PSO_CHECKPOINT_KEY)
            .expect("load metadata");
        assert!(
            meta.is_some(),
            "checkpoint should exist in database, got None"
        );

        let loaded = load_checkpoint(&store2)
            .expect("load")
            .expect("checkpoint");
        assert_eq!(loaded.iter, 10);
        assert_eq!(
            loaded.population.as_ref().map(|p| p.len()),
            Some(1),
            "one particle"
        );
        assert_eq!(
            loaded.best_cost.to_bits(),
            0.42f64.to_bits()
        );
    }

    #[test]
    fn checkpoint_none_when_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dummy_store(&dir);
        let loaded = load_checkpoint(&store).expect("load");
        assert!(loaded.is_none());
    }

    #[test]
    fn checkpoint_none_when_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dummy_store(&dir);
        store
            .save_metadata(PSO_CHECKPOINT_KEY, "not valid json")
            .expect("save corrupt");
        let loaded = load_checkpoint(&store).expect("load");
        assert!(loaded.is_none(), "corrupt checkpoint should be discarded");
    }

    #[test]
    fn sanitize_replaces_non_finite() {
        let particle = Particle::new(vec![0.1, 0.2], f64::INFINITY, vec![0.01, 0.02]);
        let mut state: PopulationState<Particle<Vec<f64>, f64>, f64> =
            PopulationState::new();
        // PopulationState::new() leaves cost/best_cost/etc at inf by
        // default.  Override only the particle field.
        state.population = Some(vec![particle]);
        state.cost = f64::INFINITY;
        state.best_cost = f64::NEG_INFINITY;
        state.target_cost = f64::NAN;
        state.best_individual = Some(Particle::new(vec![0.1, 0.2], 0.0, vec![0.01, 0.02]));
        state.iter = 5;

        let sanitized = sanitize_state(&state);
        assert!(sanitized.cost.is_finite());
        assert!(sanitized.best_cost.is_finite());
        assert!(sanitized.target_cost.is_finite());
        assert!(sanitized.prev_cost.is_finite());
        assert!(sanitized.prev_best_cost.is_finite());
    }
}

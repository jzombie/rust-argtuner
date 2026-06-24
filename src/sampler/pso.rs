use std::error::Error;
use std::sync::Arc;

use argmin::core::PopulationState;
use argmin::core::observers::{Observe, ObserverMode};
use argmin::core::{CostFunction, Executor, KV, State};
use argmin::solver::particleswarm::Particle;
use argmin::solver::particleswarm::ParticleSwarm;

use crate::checkpoint::{ControllableObjective, build_trial_result_cache};
use crate::trial::store::TrialStore;

// ---------------------------------------------------------------------------
// Metadata keys stored in the project database
// ---------------------------------------------------------------------------

const PSO_CHECKPOINT_KEY: &str = "pso_checkpoint";

/// Checkpoint config key — stores solver weight parameters so that we can
/// reject a checkpoint whose configuration differs from the current run.
const PSO_CHECKPOINT_CFG_KEY: &str = "pso_checkpoint_config";

// ---------------------------------------------------------------------------
// Observer that periodically saves a PSO checkpoint
// ---------------------------------------------------------------------------

struct PsoCheckpointSaver {
    store: TrialStore,
    solver_config: PsoSolverConfig,
    frequency: u64,
}

impl Observe<PopulationState<Particle<Vec<f64>, f64>, f64>> for PsoCheckpointSaver {
    fn observe_iter(
        &mut self,
        state: &PopulationState<Particle<Vec<f64>, f64>, f64>,
        _kv: &KV,
    ) -> Result<(), argmin::core::Error> {
        if state.iter % self.frequency == 0 {
            save_checkpoint(&self.store, &self.solver_config, state)
                .map_err(|e| argmin::core::Error::msg(format!("checkpoint save failed: {e}")))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Solver configuration stored alongside each checkpoint
// ---------------------------------------------------------------------------

/// PSO weight defaults, defined once and passed explicitly to `ParticleSwarm`
/// via its builder setters.  This keeps our checkpoint config validation in
/// sync with the actual solver regardless of upstream default changes.
fn pso_default_inertia() -> f64 {
    1.0 / (2.0 * 2.0f64.ln())
}
fn pso_default_cognitive() -> f64 {
    0.5 + 2.0f64.ln()
}
fn pso_default_social() -> f64 {
    0.5 + 2.0f64.ln()
}

/// Lightweight solver configuration snapshot written with every checkpoint so
/// that we can reject a checkpoint whose params don't match the current run.
#[derive(Clone, serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct PsoSolverConfig {
    weight_inertia: f64,
    weight_cognitive: f64,
    weight_social: f64,
    num_particles: usize,
}

impl PsoSolverConfig {
    fn from_parts(num_particles: usize) -> Self {
        Self {
            weight_inertia: pso_default_inertia(),
            weight_cognitive: pso_default_cognitive(),
            weight_social: pso_default_social(),
            num_particles,
        }
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
    if let Some(ref mut pop) = s.population {
        for p in pop.iter_mut() {
            if !p.cost.is_finite() {
                p.cost = 0.0;
            }
        }
    }
    s
}

fn save_checkpoint(
    store: &TrialStore,
    config: &PsoSolverConfig,
    state: &PopulationState<Particle<Vec<f64>, f64>, f64>,
) -> Result<(), String> {
    let sanitized = sanitize_state(state);
    let json = serde_json::to_string(&sanitized)
        .map_err(|e| format!("failed to serialize PSO checkpoint: {e}"))?;
    store
        .save_metadata(PSO_CHECKPOINT_KEY, &json)
        .map_err(|e| format!("failed to save PSO checkpoint: {e}"))?;
    let config_json = serde_json::to_string(config)
        .map_err(|e| format!("failed to serialize PSO checkpoint config: {e}"))?;
    store
        .save_metadata(PSO_CHECKPOINT_CFG_KEY, &config_json)
        .map_err(|e| format!("failed to save PSO checkpoint config: {e}"))?;
    Ok(())
}

fn load_checkpoint(
    store: &TrialStore,
    current_config: &PsoSolverConfig,
) -> Result<Option<PopulationState<Particle<Vec<f64>, f64>, f64>>, String> {
    let json = match store.load_metadata(PSO_CHECKPOINT_KEY) {
        Ok(Some(v)) => v,
        Ok(None) => return Ok(None),
        Err(e) => return Err(format!("failed to load PSO checkpoint: {e}")),
    };

    if json.is_empty() {
        return Ok(None);
    }

    // Validate the saved solver config matches the current run.
    let saved_cfg_json = match store.load_metadata(PSO_CHECKPOINT_CFG_KEY) {
        Ok(Some(v)) => v,
        Ok(None) => String::new(),
        Err(_) => String::new(),
    };

    if !saved_cfg_json.is_empty() {
        if let Ok(saved_cfg) = serde_json::from_str::<PsoSolverConfig>(&saved_cfg_json) {
            if saved_cfg != *current_config {
                let msg = format!(
                    "PSO checkpoint config mismatch (saved: inertia={}, cognitive={}, \
                     social={}, particles={}; current: inertia={}, cognitive={}, \
                     social={}, particles={})",
                    saved_cfg.weight_inertia,
                    saved_cfg.weight_cognitive,
                    saved_cfg.weight_social,
                    saved_cfg.num_particles,
                    current_config.weight_inertia,
                    current_config.weight_cognitive,
                    current_config.weight_social,
                    current_config.num_particles,
                );
                return Err(msg);
            }
        }
    }

    match serde_json::from_str(&json) {
        Ok(state) => Ok(Some(state)),
        Err(e) => Err(format!("corrupt PSO checkpoint: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Public PSO entry point
// ---------------------------------------------------------------------------

pub fn run_pso(
    mut objective: ControllableObjective,
    iters: usize,
    particles: usize,
) -> Result<(), Box<dyn Error>> {
    #[derive(Clone)]
    struct ObjWrapper(Arc<ControllableObjective>);

    impl CostFunction for ObjWrapper {
        type Param = Vec<f64>;
        type Output = f64;

        fn cost(&self, param: &Self::Param) -> Result<Self::Output, argmin::core::Error> {
            self.0.eval(param).map_err(|e| argmin::core::Error::msg(e))
        }
    }

    let dims = objective.dims();
    let lower = vec![0.0; dims];
    let upper = vec![1.0; dims];
    let store = objective.store().clone();
    let solver_config = PsoSolverConfig::from_parts(particles);

    // ---- checkpoint loading (warm restart) ----
    let loaded_state = match load_checkpoint(&store, &solver_config) {
        Ok(Some(mut s)) => {
            let pop_len = s.population.as_ref().map(|p| p.len()).unwrap_or(0);
            if pop_len == particles {
                // Clear termination status so the executor doesn't immediately exit.
                s.termination_status = argmin::core::TerminationStatus::NotTerminated;
                s.max_iters = s.iter + iters as u64;
                Some(s)
            } else {
                eprintln!(
                    "INFO: particle count mismatch (saved: {pop_len}, \
                     requested: {particles}); starting fresh"
                );
                None
            }
        }
        Ok(None) => None,
        Err(msg) => {
            // Corrupt or incompatible checkpoint — clear it and start fresh.
            let _ = store.save_metadata(PSO_CHECKPOINT_KEY, "");
            let _ = store.save_metadata(PSO_CHECKPOINT_CFG_KEY, "");
            eprintln!("WARN: {msg}; starting fresh");
            None
        }
    };

    // Always attach a mutable trial cache so that within a single PSO run,
    // previously-evaluated HP configs are returned without re-running the
    // command (avoids duplicate config errors as particles converge).
    // Across warm restarts the cache is pre-populated from the store.
    if let Ok(cache) = build_trial_result_cache(&store) {
        objective = objective.with_cache(cache);
    }

    let solver = ParticleSwarm::new((lower, upper), particles)
        .with_inertia_factor(pso_default_inertia())
        .map_err(|e| format!("invalid inertia weight: {e}"))?
        .with_cognitive_factor(pso_default_cognitive())
        .map_err(|e| format!("invalid cognitive weight: {e}"))?
        .with_social_factor(pso_default_social())
        .map_err(|e| format!("invalid social weight: {e}"))?;
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
            solver_config: solver_config.clone(),
            frequency,
        },
        ObserverMode::Always,
    );

    let result = executor.run();

    // ---- final checkpoint save (on success) ----
    if let Ok(ref result) = result {
        let _ = save_checkpoint(&store, &solver_config, &result.state);
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
        let mut state: PopulationState<Particle<Vec<f64>, f64>, f64> = PopulationState::new();
        state.max_iters = 100;
        state.population = Some(vec![particle]);
        state.cost = 0.42;
        state.best_cost = 0.42;
        state.prev_cost = 0.42;
        state.prev_best_cost = 0.42;
        state.target_cost = 0.0;
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
        let cfg = PsoSolverConfig::from_parts(42);
        save_checkpoint(&store, &cfg, &state).expect("save");

        let store2 = dummy_store(&dir);
        let meta = store2
            .load_metadata(PSO_CHECKPOINT_KEY)
            .expect("load metadata");
        assert!(meta.is_some(), "checkpoint should exist in database");

        let loaded = load_checkpoint(&store2, &cfg)
            .expect("load")
            .expect("checkpoint");
        assert_eq!(loaded.iter, 10);
        assert_eq!(
            loaded.population.as_ref().map(|p| p.len()),
            Some(1),
            "one particle"
        );
        assert_eq!(loaded.best_cost.to_bits(), 0.42f64.to_bits());
    }

    #[test]
    fn checkpoint_none_when_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dummy_store(&dir);
        let cfg = PsoSolverConfig::from_parts(10);
        let loaded = load_checkpoint(&store, &cfg).expect("load");
        assert!(loaded.is_none());
    }

    #[test]
    fn checkpoint_rejects_corrupt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dummy_store(&dir);
        let cfg = PsoSolverConfig::from_parts(10);
        store
            .save_metadata(PSO_CHECKPOINT_KEY, "not valid json")
            .expect("save corrupt");
        let err = load_checkpoint(&store, &cfg).expect_err("should reject corrupt");
        assert!(err.contains("corrupt PSO checkpoint"), "got: {err}");
    }

    #[test]
    fn checkpoint_rejects_config_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dummy_store(&dir);

        let state = make_state();
        let cfg_a = PsoSolverConfig::from_parts(10);
        save_checkpoint(&store, &cfg_a, &state).expect("save");

        // Try loading with a different particle count.
        let cfg_b = PsoSolverConfig::from_parts(20);
        let err = load_checkpoint(&store, &cfg_b).expect_err("should reject mismatch");
        assert!(err.contains("config mismatch"), "got: {err}");
    }

    #[test]
    fn checkpoint_rejects_weight_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = dummy_store(&dir);

        let state = make_state();
        let cfg_a = PsoSolverConfig {
            weight_inertia: 0.5,
            weight_cognitive: 1.0,
            weight_social: 2.0,
            num_particles: 10,
        };
        save_checkpoint(&store, &cfg_a, &state).expect("save");

        // Same particle count but different weights.
        let cfg_b = PsoSolverConfig {
            weight_inertia: 0.7,
            ..cfg_a
        };
        let err = load_checkpoint(&store, &cfg_b).expect_err("should reject weight mismatch");
        assert!(err.contains("config mismatch"));
    }

    #[test]
    fn sanitize_replaces_non_finite() {
        let particle = Particle::new(vec![0.1, 0.2], f64::INFINITY, vec![0.01, 0.02]);
        let mut state: PopulationState<Particle<Vec<f64>, f64>, f64> = PopulationState::new();
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
        assert!(
            sanitized
                .population
                .as_ref()
                .unwrap()
                .iter()
                .all(|p| p.cost.is_finite())
        );
    }

    // -----------------------------------------------------------------------
    // End-to-end test: runs actual PSO through the full command pipeline,
    // then verifies the checkpoint was persisted and can be resumed.
    // -----------------------------------------------------------------------

    /// Build a command that uses the compiled `emit_result` binary to emit a
    /// valid `::ARGTUNER::` result.  Every invocation returns metric=0.42 so
    /// convergence is deterministic.
    fn make_pso_objective(dir: &tempfile::TempDir) -> ControllableObjective {
        let template_str = crate::test_support::bin_command("emit_result");
        let template = crate::CommandTemplate::new(template_str);
        let store = TrialStore::new(
            dir.path().join(crate::TRIALS_CSV_FILENAME),
            template.clone(),
        );
        // 2D continuous space — the chance of two particles landing on the
        // exact same HP values approaches zero.
        let space = crate::SearchSpace {
            params: vec![
                crate::ParamSpec::Float {
                    name: "x0".to_string(),
                    min: 0.0,
                    max: 1.0,
                    log_scale: false,
                    step: None,
                    format: None,
                },
                crate::ParamSpec::Float {
                    name: "x1".to_string(),
                    min: 0.0,
                    max: 1.0,
                    log_scale: false,
                    step: None,
                    format: None,
                },
            ],
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
        ControllableObjective::new(objective, Arc::new(AtomicBool::new(false)))
    }

    #[test]
    fn pso_end_to_end_checkpoint_persisted_and_restored() {
        let dir = tempfile::tempdir().expect("tempdir");

        // ---- first run: 3 particles, 3 iterations ----
        let obj1 = make_pso_objective(&dir);
        run_pso(obj1, 3, 3).expect("first PSO run");

        // Verify checkpoint was written to the DB.
        let store = dummy_store(&dir);
        let chk_meta = store
            .load_metadata(PSO_CHECKPOINT_KEY)
            .expect("load metadata");
        assert!(
            chk_meta.is_some(),
            "PSO checkpoint should be in the database after the first run"
        );
        let cfg_meta = store
            .load_metadata(PSO_CHECKPOINT_CFG_KEY)
            .expect("load config metadata");
        assert!(
            cfg_meta.is_some(),
            "PSO checkpoint config should be in the database after the first run"
        );

        // ---- second run: loads checkpoint, runs more iterations ----
        let obj2 = make_pso_objective(&dir);
        // Use the same config (weights + particle count).
        run_pso(obj2, 3, 3).expect("second PSO run (warm restart)");

        // ---- verify checkpoint was updated ----
        let store2 = dummy_store(&dir);
        let chk2 = load_checkpoint(&store2, &PsoSolverConfig::from_parts(3))
            .expect("load")
            .expect("checkpoint after second run");
        // Second run started at iter 3 and ran 3 more → final iter should be 6.
        assert_eq!(chk2.iter, 6);
    }

    #[test]
    fn pso_checkpoint_config_mismatch_starts_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");

        let obj1 = make_pso_objective(&dir);
        run_pso(obj1, 3, 5).expect("first PSO run (5 particles)");

        // Second run with a different config (10 particles).
        let obj2 = make_pso_objective(&dir);
        run_pso(obj2, 3, 10).expect("second PSO run (10 particles, fresh start)");

        // The checkpoint should now reflect 10 particles.
        let store = dummy_store(&dir);
        let cfg = PsoSolverConfig::from_parts(10);
        let chk = load_checkpoint(&store, &cfg)
            .expect("load")
            .expect("checkpoint");
        assert_eq!(
            chk.population.as_ref().map(|p| p.len()),
            Some(10),
            "checkpoint should have 10 particles after fresh start"
        );
    }
}

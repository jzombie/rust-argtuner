use std::error::Error;
use std::sync::Arc;

use argmin::core::{CostFunction, Executor, State};
use argmin::solver::particleswarm::ParticleSwarm;
use argmin::core::PopulationState;

use crate::checkpoint::ControllableObjective;

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
    let solver = ParticleSwarm::new((lower, upper), particles);

    let obj_wrapper = ObjWrapper(Arc::new(objective));

    let _res = Executor::new(obj_wrapper, solver)
        .configure(|state: PopulationState<
            argmin::solver::particleswarm::Particle<Vec<f64>, f64>,
            f64,
        >| state.max_iters(iters as u64))
        .run();

    match _res {
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

/// Serializable snapshot of PSO solver state for resume.
pub struct PsoSnapshot {
    pub weight_inertia: f64,
    pub weight_cognitive: f64,
    pub weight_social: f64,
    pub num_particles: usize,
    pub state: serde_json::Value,
}

pub fn save_checkpoint(
    path: &std::path::Path,
    weight_inertia: f64,
    weight_cognitive: f64,
    weight_social: f64,
    num_particles: usize,
    state_json: serde_json::Value,
) -> Result<(), String> {
    use serde::Serialize;

    #[derive(Serialize)]
    struct Snapshot {
        weight_inertia: f64,
        weight_cognitive: f64,
        weight_social: f64,
        num_particles: usize,
        state: serde_json::Value,
    }

    let snapshot = Snapshot {
        weight_inertia,
        weight_cognitive,
        weight_social,
        num_particles,
        state: state_json,
    };
    let json = serde_json::to_string_pretty(&snapshot)
        .map_err(|e| format!("failed to serialize PSO checkpoint: {e}"))?;
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .map_err(|e| format!("failed to create temp checkpoint file: {e}"))?;
    use std::io::Write;
    tmp.write_all(json.as_bytes())
        .map_err(|e| format!("failed to write checkpoint: {e}"))?;
    tmp.persist(path)
        .map_err(|e| format!("failed to persist checkpoint: {e}"))?;
    Ok(())
}

pub fn load_checkpoint(
    path: &std::path::Path,
) -> Result<Option<PsoSnapshot>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read PSO checkpoint: {e}"))?;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct RawSnapshot {
        weight_inertia: f64,
        weight_cognitive: f64,
        weight_social: f64,
        num_particles: usize,
        state: serde_json::Value,
    }

    let raw: RawSnapshot = serde_json::from_str(&json)
        .map_err(|e| format!("failed to parse PSO checkpoint: {e}"))?;
    Ok(Some(PsoSnapshot {
        weight_inertia: raw.weight_inertia,
        weight_cognitive: raw.weight_cognitive,
        weight_social: raw.weight_social,
        num_particles: raw.num_particles,
        state: raw.state,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("pso_checkpoint.json");

        let state_json: serde_json::Value = serde_json::json!({
            "population": [{"position": [0.1, 0.2], "velocity": [0.0, 0.0]}],
            "best_cost": 0.42,
            "iter": 5,
        });

        save_checkpoint(&path, 0.5, 0.3, 0.2, 10, state_json.clone())
            .expect("save");

        let loaded = load_checkpoint(&path)
            .expect("load")
            .expect("snapshot exists");

        assert_eq!(loaded.weight_inertia, 0.5);
        assert_eq!(loaded.weight_cognitive, 0.3);
        assert_eq!(loaded.weight_social, 0.2);
        assert_eq!(loaded.num_particles, 10);
        assert_eq!(loaded.state, state_json);
    }

    #[test]
    fn checkpoint_none_when_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nonexistent.json");
        let loaded = load_checkpoint(&path).expect("load");
        assert!(loaded.is_none());
    }
}

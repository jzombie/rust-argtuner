use std::error::Error;
use std::sync::Arc;

use argmin::core::{CostFunction, Executor};
use argmin::solver::particleswarm::ParticleSwarm;

use crate::command::CommandObjective;

pub fn run_pso(
    objective: CommandObjective,
    iters: usize,
    particles: usize,
) -> Result<(), Box<dyn Error>> {
    #[derive(Clone)]
    struct ObjWrapper(Arc<CommandObjective>);

    impl CostFunction for ObjWrapper {
        type Param = Vec<f64>;
        type Output = f64;

        fn cost(&self, param: &Self::Param) -> Result<Self::Output, argmin::core::Error> {
            self.0.eval(param).map_err(argmin::core::Error::msg)
        }
    }

    let dims = objective.dims();
    let lower = vec![0.0; dims];
    let upper = vec![1.0; dims];
    let solver = ParticleSwarm::new((lower, upper), particles);
    let _res = Executor::new(ObjWrapper(Arc::new(objective)), solver)
        .configure(|state| state.max_iters(iters as u64))
        .run()?;
    Ok(())
}

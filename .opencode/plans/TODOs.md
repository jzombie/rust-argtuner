# TODO

Ongoing work items.

- Centralize configuration constants into constants.rs (i.e. DEFAULT_FRONT_CAPACITY)
- Fix `run {project}` vs. `watch --project {project}` inconsistencies
- Compare prior states before determining new state, to ensure new configs are
  always evaluated, or to stop early if the search space is exhausted.
- Add an example showing how to use argtuner with
  [Burn](https://crates.io/crates/burn) and use it as an integration test.

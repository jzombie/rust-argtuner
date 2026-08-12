# TODO

Ongoing work items.

- Compare prior states before determining new state, to ensure new configs are
  always evaluated, or to stop early if the search space is exhausted.
- Add an example showing how to use argtuner with
  [Burn](https://crates.io/crates/burn) and use it as an integration test.
- Mention in the README that `argtuner` expects a stateless environment for
  command execution; arguments sent to it should return as close to a
  deterministic result as possible.
- Shouldn't boolean be an option as well?

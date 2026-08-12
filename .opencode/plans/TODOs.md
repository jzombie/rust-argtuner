# TODO

Ongoing work items.

- Re-extract SDK... do not allow large dependencies to be compiled into ML workloads.  The deprecated "talkback" crates should point to the SDK instead, and the SDK should be very lightweight, communicating with the argtuner via stdio.
- Compare prior states before determining new state, to ensure new configs are
  always evaluated, or to stop early if the search space is exhausted.
- Add an example showing how to use argtuner with
  [Burn](https://crates.io/crates/burn) and use it as an integration test.

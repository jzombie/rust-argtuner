# TODO

Ongoing work items.

- Re-extract SDK... do not allow large dependencies to be compiled into ML workloads.  The deprecated "talkback" crates should point to the SDK instead, and the SDK should be very lightweight, communicating with the argtuner via stdio.  Be sure to document that the extra overhead for using the SDK *per project* will be extremely low, and the SDK can be skipped entirely if critical performance is paramount.
- Compare prior states before determining new state, to ensure new configs are
  always evaluated, or to stop early if the search space is exhausted.
- Add an example showing how to use argtuner with
  [Burn](https://crates.io/crates/burn) and use it as an integration test.

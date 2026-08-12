# argtuner-mock-bin

Test-only mock subprocesses for the argtuner test harness. Each binary acts as a
fake training script that emits `::ARGTUNER::` events over stdout, simulating a
trial command for integration tests. They are consumed by
`argtuner::test_support::bin_command` and are never published to crates.io.

Binaries: `mock_emit_result`, `mock_emit_env_result`, `mock_emit_invalid_result`,
`mock_emit_binding_version`, `mock_emit_x_used`.

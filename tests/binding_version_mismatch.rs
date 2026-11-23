use argtuner::CONFIG_FILENAME;
use indoc::indoc;
use std::process::Command;

#[test]
fn binding_version_mismatch_exits_with_message() {
    let dir = tempfile::tempdir().expect("tempdir");
    let project_root = dir.path();
    // NOTE: the target dir is not derived from `current_exe()`: newer
    // toolchains place test binaries outside `target/debug/deps/` (e.g. under
    // `target/debug/build/<pkg>/<hash>/out/`), so that layout assumption is
    // stale. `CARGO_BIN_EXE_argtuner` is set by Cargo for same-package
    // binaries; `bin_path` falls back to `target/debug/argtuner`.
    let argtuner_bin = argtuner::test_support::bin_path("argtuner");
    let emit_bin = argtuner::test_support::bin_path("mock_emit_binding_version");
    assert!(
        argtuner_bin.exists(),
        "argtuner bin not found at {}",
        argtuner_bin.display()
    );
    assert!(
        emit_bin.exists(),
        "mock_emit_binding_version bin not found at {}",
        emit_bin.display()
    );

    let template = format!(
        "'{}' --version 0.0.0 --metric 1.0 --checkpoint-dir {{trial_dir}}",
        emit_bin.display()
    );
    let toml = format!(
        "{template_line}\n\n{project}\n\n{sampler}\n\n{scheduler}\n\n{space}\n",
        template_line = format!("template = \"{}\"", template.replace('\\', "\\\\")),
        project = indoc! {r#"
            [project]
            metric_key = "value"
            goal = "min"
            pruner = "none"
            inject_trial_placeholders = true
        "#},
        sampler = indoc! {r#"
            [sampler]
            type = "random"
        "#},
        scheduler = indoc! {r#"
            [scheduler]
            type = "fixed"
            n_trials = 1
            seed = 7
        "#},
        space = "[space]\nparams = []"
    );
    std::fs::write(project_root.join(CONFIG_FILENAME), toml).expect("write config");

    let output = Command::new(argtuner_bin)
        .arg("run")
        .arg(project_root)
        .env(argtuner_common::FORCE_PIPES_ENV, "1")
        .output()
        .expect("run argtuner");

    let status = output.status.code();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        status,
        Some(2),
        "unexpected status {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stderr.contains("binding version mismatch"),
        "stderr missing mismatch message: {stderr}"
    );
}

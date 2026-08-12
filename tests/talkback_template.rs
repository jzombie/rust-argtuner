use argtuner::UnifiedConfig;
use argtuner_derive::talkback_args;

#[allow(dead_code)] // template-only definition: fields are read by the derive, not this test
#[talkback_args]
struct TemplateArgs {
    #[param(default = 0.001, min = 0.0001, max = 0.1, log = true)]
    lr: f64,
    #[param(default = 100, min = 10, max = 1000)]
    steps: usize,
    #[param(choices = ["adam", "adamw"])]
    optimizer: String,
    #[param(value_name = "trial_dir")]
    checkpoint_dir: Option<String>,
}

fn space_names(config: &UnifiedConfig) -> Vec<&str> {
    config.space.params.iter().map(|p| p.name()).collect()
}

#[test]
fn talkback_template_toml_is_valid() {
    let toml_text = argtuner::render_template_toml::<TemplateArgs>();
    let parsed: UnifiedConfig =
        toml::from_str(&toml_text).expect("template must parse as UnifiedConfig");
    parsed
        .space
        .validate_specs()
        .expect("template space must validate");

    let names = space_names(&parsed);
    assert!(names.contains(&"lr"), "lr missing from space: {names:?}");
    assert!(names.contains(&"steps"), "steps missing from space: {names:?}");
    assert!(
        names.contains(&"optimizer"),
        "optimizer missing from space: {names:?}"
    );
    assert!(
        !names.contains(&"checkpoint_dir"),
        "reserved value_name must be excluded from space: {names:?}"
    );
}

#[test]
fn talkback_template_renders_placeholders() {
    let cmd = argtuner::render_template_command::<TemplateArgs>();
    assert!(cmd.contains("--lr {lr}"), "template: {cmd}");
    assert!(cmd.contains("--steps {steps}"), "template: {cmd}");
    assert!(cmd.contains("--optimizer {optimizer}"), "template: {cmd}");
    assert!(
        cmd.contains("--checkpoint-dir {trial_dir}"),
        "reserved placeholder must render as {{trial_dir}}: {cmd}"
    );
}

#[allow(dead_code)]
#[talkback_args]
struct TrickyArgs {
    #[param(default = 0.001, min = 0.0001, max = 0.1, step = 0.001)]
    lr: f64,
    #[param(choices = ["a\"b", "c\\d", "plain"])]
    kernel: String,
}

#[test]
fn talkback_template_round_trips_tricky_values() {
    let toml_text = argtuner::render_template_toml::<TrickyArgs>();
    let parsed: UnifiedConfig = toml::from_str(&toml_text)
        .expect("template must parse as UnifiedConfig even with quotes/backslashes");

    // Choice renders as an inline array and round-trips the values (toml_edit
    // chooses literal vs basic strings as appropriate — both are valid TOML).
    assert!(
        toml_text.contains("values = ["),
        "choice must render as an inline array:\n{toml_text}"
    );
    let kernel = parsed
        .space
        .params
        .iter()
        .find(|p| p.name() == "kernel")
        .expect("kernel in space");
    match kernel {
        argtuner::ParamSpec::Choice { values, .. } => assert_eq!(
            values,
            &["a\"b".to_string(), "c\\d".to_string(), "plain".to_string()]
        ),
        other => panic!("expected Choice, got {other:?}"),
    }

    // Float with a step round-trips.
    let lr = parsed
        .space
        .params
        .iter()
        .find(|p| p.name() == "lr")
        .expect("lr in space");
    match lr {
        argtuner::ParamSpec::Float { step, .. } => assert_eq!(*step, Some(0.001)),
        other => panic!("expected Float, got {other:?}"),
    }
}

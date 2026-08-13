use argtuner::UnifiedConfig;
use argtuner_sdk::Params;
use argtuner_sdk::talkback_args;

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
    let toml_text = argtuner_sdk::render_template_toml::<TemplateArgs>();
    let parsed: UnifiedConfig =
        toml::from_str(&toml_text).expect("template must parse as UnifiedConfig");
    parsed
        .space
        .validate_specs()
        .expect("template space must validate");

    let names = space_names(&parsed);
    assert!(names.contains(&"lr"), "lr missing from space: {names:?}");
    assert!(
        names.contains(&"steps"),
        "steps missing from space: {names:?}"
    );
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
    let cmd = argtuner_sdk::render_template_command::<TemplateArgs>();
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
    let toml_text = argtuner_sdk::render_template_toml::<TrickyArgs>();
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

#[allow(dead_code)]
#[talkback_args]
struct BoolParamArgs {
    #[param(default = true)]
    use_dropout: bool,
    #[param(skip = true, default = false)]
    verbose: bool,
}

#[test]
fn talkback_template_renders_bool() {
    let toml_text = argtuner_sdk::render_template_toml::<BoolParamArgs>();
    let parsed: UnifiedConfig =
        toml::from_str(&toml_text).expect("template must parse as UnifiedConfig");

    let names = space_names(&parsed);
    assert!(
        names.contains(&"use_dropout"),
        "bool must be a tunable space param: {names:?}"
    );
    assert!(
        !names.contains(&"verbose"),
        "skipped bool must be excluded from the space: {names:?}"
    );
    match parsed
        .space
        .params
        .iter()
        .find(|p| p.name() == "use_dropout")
        .expect("use_dropout in space")
    {
        argtuner::ParamSpec::Bool { .. } => {}
        other => panic!("expected Bool, got {other:?}"),
    }

    // Tunable bool renders as a placeholder, not a bare flag.
    let cmd = argtuner_sdk::render_template_command::<BoolParamArgs>();
    assert!(
        cmd.contains("--use-dropout {use_dropout}"),
        "template: {cmd}"
    );
}

#[test]
fn talkback_template_skipped_bool_parses_on_cli() {
    let command = BoolParamArgs::command();

    // `--verbose` (missing value) parses as true.
    let matches = command
        .clone()
        .try_get_matches_from(["app", "--verbose"])
        .expect("--verbose without a value must parse");
    assert!(
        *matches.get_one::<bool>("verbose").expect("verbose"),
        "--verbose should parse as true"
    );

    // `--verbose false` parses as false.
    let matches = command
        .clone()
        .try_get_matches_from(["app", "--verbose", "false"])
        .expect("--verbose false must parse");
    assert!(
        !*matches.get_one::<bool>("verbose").expect("verbose"),
        "--verbose false should parse as false"
    );

    // Absent uses the default.
    let matches = command
        .try_get_matches_from(["app"])
        .expect("absent verbose must fall back to default");
    assert!(
        !*matches.get_one::<bool>("verbose").expect("verbose"),
        "absent verbose should use the default (false)"
    );
}

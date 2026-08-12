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

use argtuner::UnifiedConfig;
use argtuner_talkback_derive::talkback_args;
use clap::Parser;

#[test]
fn talkback_template_toml_is_valid() {
    #[talkback_args]
    #[derive(Debug, Parser)]
    struct TemplateArgs {
        #[arg(long)]
        lr: Option<f64>,
        #[arg(long, value_name = "PATH")]
        checkpoint_dir: Option<String>,
    }

    let toml_text = argtuner_talkback::render_template_toml::<TemplateArgs>();
    let parsed: UnifiedConfig =
        toml::from_str(&toml_text).expect("template must parse as UnifiedConfig");
    parsed
        .space
        .validate_specs()
        .expect("template space must validate");
    assert!(
        parsed.template.contains("--checkpoint-dir {trial_dir}"),
        "template missing checkpoint-dir placeholder: {}",
        parsed.template
    );
}

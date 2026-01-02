use argtuner::UnifiedConfig;
use indoc::indoc;

fn base_project_section() -> &'static str {
    indoc! {
        r#"
        [project]
        metric_key = "metric"
        goal = "min"
        pruner = "none"
        inject_trial_placeholders = true
        "#
    }
}

fn sampler_section_with(extra: &str) -> String {
    let mut section = String::from(indoc! {
        r#"
        [sampler]
        type = "random"
        "#
    });
    section.push_str(extra);
    section
}

fn scheduler_section_with(extra: &str) -> String {
    let mut section = String::from(indoc! {
        r#"
        [scheduler]
        type = "fixed"
        n_trials = 1
        seed = 7
        "#
    });
    section.push_str(extra);
    section
}

fn space_section() -> &'static str {
    indoc! {
        r#"
        [space]
        [[space.params]]
        type = "Float"
        name = "lr"
        min = 0.0
        max = 1.0
        log = false
        "#
    }
}

fn template_line() -> &'static str {
    "template = \"echo {lr}\""
}

#[test]
fn scientific_notation_values_parse_in_space() {
    let toml = indoc! {r#"
        template = "echo {lr} {dec}"

        [project]
        metric_key = "metric"
        goal = "min"
        pruner = "none"
        inject_trial_placeholders = true

        [sampler]
        type = "random"

        [scheduler]
        type = "fixed"
        n_trials = 1
        seed = 7

        [space]
        [[space.params]]
        type = "Float"
        name = "lr"
        min = 1e-3
        max = 0.1
        log = false

        [[space.params]]
        type = "Float"
        name = "dec"
        min = 0.0005
        max = 1e-2
        log = false
    "#};
    
    let config = toml::from_str::<UnifiedConfig>(toml).expect("config parses");
    let mut floats = config.space.params.iter().filter_map(|param| match param {
        argtuner::ParamSpec::Float { name, min, max, .. } => Some((name.as_str(), *min, *max)),
        _ => None,
    });
    let (name, min, max) = floats.next().expect("lr param");
    assert_eq!(name, "lr");
    assert!((min - 1e-3).abs() < 1e-12);
    assert!((max - 0.1).abs() < 1e-12);

    let (name, min, max) = floats.next().expect("dec param");
    assert_eq!(name, "dec");
    assert!((min - 0.0005).abs() < 1e-12);
    assert!((max - 1e-2).abs() < 1e-12);
}

#[test]
fn missing_sampler_section_is_error() {
    let toml = format!(
        "{template}\n\n{project}\n\n{scheduler}\n\n{space}\n",
        template = template_line(),
        project = base_project_section(),
        scheduler = scheduler_section_with(""),
        space = space_section(),
    );
    let result = toml::from_str::<UnifiedConfig>(&toml);
    assert!(result.is_err(), "config without [sampler] should fail");
}

#[test]
fn extra_project_field_is_error() {
    let project_with_extra = format!("{base}\nunknown = \"nope\"", base = base_project_section(),);
    let toml = format!(
        "{template}\n\n{project}\n\n{sampler}\n\n{scheduler}\n\n{space}\n",
        template = template_line(),
        project = project_with_extra,
        sampler = sampler_section_with(""),
        scheduler = scheduler_section_with(""),
        space = space_section(),
    );
    let err = toml::from_str::<UnifiedConfig>(&toml).expect_err("extra project key should fail");
    assert!(
        err.to_string().contains("unknown field"),
        "unexpected error: {err}"
    );
}

#[test]
fn extra_sampler_field_is_error() {
    let sampler_with_extra = sampler_section_with("bogus = 1\n");
    let toml = format!(
        "{template}\n\n{project}\n\n{sampler}\n\n{scheduler}\n\n{space}\n",
        template = template_line(),
        project = base_project_section(),
        sampler = sampler_with_extra,
        scheduler = scheduler_section_with(""),
        space = space_section(),
    );
    toml::from_str::<UnifiedConfig>(&toml).expect_err("unknown sampler key should fail");
}

use crate::command::CommandTemplate;
use crate::constants::{PLACEHOLDER_TRIAL_DIR, PLACEHOLDER_TRIAL_ID};
use crate::project::ProjectConfig;
use crate::scheduler::SchedulerBinding;
use crate::search_space::SearchSpace;

pub fn validate_project_config(
    config: &ProjectConfig,
    template: &CommandTemplate,
    space: &SearchSpace,
    template_placeholders: &[String],
) -> Result<(), String> {
    let checkpoint_arg = config
        .checkpoint_arg
        .as_deref()
        .unwrap_or("--checkpoint-dir");
    if !template_has_checkpoint_dir(template, checkpoint_arg) {
        return Err(format!(
            "template must include {checkpoint_arg} {{trial_dir}}"
        ));
    }

    space.validate_specs()?;
    validate_conditional_flags(space, template)?;

    let space_params: Vec<_> = space.params.iter().map(|p| p.name()).collect();
    let scheduler_binding = SchedulerBinding::new(config);
    for p in template_placeholders {
        if !space_params.contains(&p.as_str())
            && p != PLACEHOLDER_TRIAL_ID
            && p != PLACEHOLDER_TRIAL_DIR
            && !scheduler_binding.allows_placeholder(p)
        {
            return Err(format!(
                "template placeholder {{{}}} not found in search space",
                p
            ));
        }
    }
    for param in &space_params {
        if !template_placeholders.contains(&param.to_string()) {
            eprintln!(
                "Warning: parameter '{}' defined in search space but not used in template",
                param
            );
        }
    }

    if let Err(err) = scheduler_binding.validate_template(template_placeholders) {
        if matches!(
            config.scheduler.kind,
            crate::scheduler::Scheduler::SuccessiveHalving
        ) {
            let placeholders = if template_placeholders.is_empty() {
                "<none>".to_string()
            } else {
                template_placeholders
                    .iter()
                    .map(|p| format!("{{{p}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return Err(format!(
                "{err}; successive_halving requires the budget placeholder \
{{{}}} in the template. template placeholders: {placeholders}. \
Either add {{{}}} to the template or set \
[scheduler.successive_halving].budget_placeholder to match your template.",
                config.scheduler.successive_halving.budget_placeholder,
                config.scheduler.successive_halving.budget_placeholder
            ));
        }
        return Err(err);
    }
    Ok(())
}

fn template_has_checkpoint_dir(template: &CommandTemplate, checkpoint_arg: &str) -> bool {
    let text = template.as_str();
    if let Ok(tokens) = shell_words::split(text) {
        return tokens_have_checkpoint_dir(&tokens, checkpoint_arg);
    }
    let has_flag = text.contains(checkpoint_arg);
    has_flag && text.contains("{trial_dir}")
}

/// Conditional params (those with a `parent`) must be bound to a named `--flag`
/// in the template; there is no flag token to strip for a positional
/// placeholder.
fn validate_conditional_flags(space: &SearchSpace, template: &CommandTemplate) -> Result<(), String> {
    let tokens = shell_words::split(template.as_str())
        .map_err(|err| format!("template tokenize failed: {err}"))?;
    for spec in space.params.iter().filter(|s| s.is_conditional()) {
        let name = spec.name();
        let ph = format!("{{{name}}}");
        let bound = tokens.iter().enumerate().any(|(i, token)| {
            if !token.contains(&ph) {
                return false;
            }
            token.starts_with('-') || (i > 0 && tokens[i - 1].starts_with('-'))
        });
        if !bound {
            return Err(format!(
                "conditional parameter '{name}' must be bound to a named --flag \
                 in the template (e.g. --{name} {{{name}}} or --{name}={{{name}}}); \
                 positional placeholders are not supported"
            ));
        }
    }
    Ok(())
}

fn tokens_have_checkpoint_dir(tokens: &[String], checkpoint_arg: &str) -> bool {
    for (idx, token) in tokens.iter().enumerate() {
        let arg_eq = format!("{checkpoint_arg}=");
        if let Some(value) = token.strip_prefix(&arg_eq)
            && value.contains("{trial_dir}")
        {
            return true;
        }
        if token == checkpoint_arg
            && let Some(next) = tokens.get(idx + 1)
            && next.contains("{trial_dir}")
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{template_has_checkpoint_dir, tokens_have_checkpoint_dir, validate_conditional_flags};
    use crate::command::CommandTemplate;

    #[test]
    fn tokens_detect_checkpoint_dir() {
        assert!(tokens_have_checkpoint_dir(
            &["--checkpoint-dir".to_string(), "{trial_dir}".to_string()],
            "--checkpoint-dir"
        ));
        assert!(tokens_have_checkpoint_dir(
            &["--checkpoint_dir={trial_dir}".to_string()],
            "--checkpoint_dir"
        ));
        assert!(!tokens_have_checkpoint_dir(
            &["--checkpoint-dir".to_string(), "/tmp".to_string()],
            "--checkpoint-dir"
        ));
    }

    #[test]
    fn template_detects_checkpoint_dir() {
        let template = CommandTemplate::new("train --checkpoint-dir {trial_dir}".to_string());
        assert!(template_has_checkpoint_dir(&template, "--checkpoint-dir"));
        let template = CommandTemplate::new("train --checkpoint-dir /tmp".to_string());
        assert!(!template_has_checkpoint_dir(&template, "--checkpoint-dir"));
    }

    #[test]
    fn rejects_positional_conditional_param() {
        let template = CommandTemplate::new("run {momentum}".to_string());
        let space = crate::SearchSpace {
            params: vec![
                crate::ParamSpec::Bool {
                    name: "use".to_string(),
                    parent: None,
                    parent_values: None,
                },
                crate::ParamSpec::Float {
                    name: "momentum".to_string(),
                    min: 0.0,
                    max: 1.0,
                    log_scale: false,
                    step: None,
                    format: None,
                    parent: Some("use".to_string()),
                    parent_values: Some(vec!["true".to_string()]),
                },
            ],
        };
        let err =
            validate_conditional_flags(&space, &template).expect_err("positional placeholder");
        assert!(err.contains("must be bound"));
    }

    #[test]
    fn accepts_flag_bound_conditional_params() {
        let template =
            CommandTemplate::new("run --use {use} --momentum={momentum}".to_string());
        let space = crate::SearchSpace {
            params: vec![
                crate::ParamSpec::Bool {
                    name: "use".to_string(),
                    parent: None,
                    parent_values: None,
                },
                crate::ParamSpec::Float {
                    name: "momentum".to_string(),
                    min: 0.0,
                    max: 1.0,
                    log_scale: false,
                    step: None,
                    format: None,
                    parent: Some("use".to_string()),
                    parent_values: Some(vec!["true".to_string()]),
                },
            ],
        };
        validate_conditional_flags(&space, &template).expect("flag-bound is valid");
    }
}

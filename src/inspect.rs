//! Human-readable summary of how argtuner interprets a project's `argtuner.toml`:
//! the rendered template, each `{placeholder}` resolved against the search space
//! (or reserved/injected), and a one-glance execution summary. Used by the
//! `argtuner inspect` subcommand and echoed in the README.

use std::io;

use line_ending::LineEnding;

use crate::command::CommandTemplate;
use crate::constants::{PLACEHOLDER_TRIAL_DIR, PLACEHOLDER_TRIAL_ID};
use crate::project::{Goal, Project, Sampler, UnifiedConfig};
use crate::scheduler::Scheduler;
use crate::search_space::{ParamSpec, SearchSpace};

/// Render the inspect summary for a project.
///
/// Output is deterministic for a given config and a stable `project.root()`
/// path: the first line is the project dir (forward-slash normalized so the
/// output is byte-identical across platforms), then the flattened template, the
/// placeholder analysis, and the execution summary.
pub fn render_inspect(project: &Project) -> io::Result<String> {
    let config = project.load_unified_config()?;
    let root = project.root().to_string_lossy().replace('\\', "/");

    let lf = LineEnding::LF.as_str();
    let mut out = String::new();
    out.push_str(&format!("project: {root}{lf}{lf}"));
    out.push_str(&format!("template:{lf}"));
    out.push_str(&format!("  {}{lf}{lf}", flatten_template(&config.template)));
    out.push_str(&placeholder_analysis(&config));
    out.push(LineEnding::LF.as_char());
    out.push_str(&execution_summary(&config));
    Ok(out)
}

/// Collapse a multiline template onto a single line: trim each line, drop the
/// trailing `\` POSIX line-continuations and blank lines, then join with
/// spaces.
fn flatten_template(template: &str) -> String {
    template
        .lines()
        .map(|line| line.trim().trim_end_matches('\\').trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// One-line description of each `{placeholder}` in the template, resolved
/// against the search space and the scheduler's budget placeholder.
fn placeholder_analysis(config: &UnifiedConfig) -> String {
    let budget = if matches!(&config.scheduler.kind, Scheduler::SuccessiveHalving) {
        Some(config.scheduler.successive_halving.budget_placeholder.clone())
    } else {
        None
    };
    let template = CommandTemplate::new(config.template.clone());
    let lf = LineEnding::LF.as_str();
    let placeholders = match template.placeholders() {
        Ok(placeholders) => placeholders,
        Err(err) => return format!("placeholders:{lf}  template error: {err}{lf}"),
    };

    // `{name}:` is name.len() + 3 (both braces + the colon); the format string
    // below adds a separating space, so the widest label is the alignment width.
    let width = placeholders
        .iter()
        .map(|name| name.len() + 3)
        .max()
        .unwrap_or(0);
    let mut out = format!("placeholders:{lf}");
    for name in placeholders {
        let label = format!("{{{name}}}:");
        let desc = describe_placeholder(&name, &config.space, budget.as_deref());
        out.push_str(&format!("  {label:<width$} {desc}{lf}"));
    }
    out
}

/// What argtuner will optimize and how the search is managed.
fn execution_summary(config: &UnifiedConfig) -> String {
    let sampler = match config.sampler.kind {
        Sampler::Pso => "pso",
        Sampler::Random => "random",
    };
    let scheduler = match &config.scheduler.kind {
        Scheduler::Fixed => "fixed",
        Scheduler::SuccessiveHalving => "successive_halving",
    };
    let lf = LineEnding::LF.as_str();
    let mut out = format!("execution:{lf}");
    if config.project.objectives.is_empty() {
        let goal = match config.project.goal {
            Goal::Min => "minimize",
            Goal::Max => "maximize",
        };
        out.push_str(&format!(
            "  {:<10} {} ({goal}){lf}",
            "metric:", config.project.metric_key
        ));
    } else {
        let parts: Vec<String> = config
            .project
            .objectives
            .iter()
            .map(|objective| {
                let goal = match objective.goal {
                    Goal::Min => "minimize",
                    Goal::Max => "maximize",
                };
                let primary = if objective.primary { " (primary)" } else { "" };
                format!("{}({goal}){primary}", objective.name)
            })
            .collect();
        out.push_str(&format!("  {:<10} {}{lf}", "objectives:", parts.join(", ")));
    }
    out.push_str(&format!("  {:<10} {sampler}{lf}", "sampler:"));
    out.push_str(&format!(
        "  {:<10} {scheduler} ({} trials){lf}",
        "scheduler:",
        config.scheduler.n_trials
    ));
    out
}

fn describe_placeholder(name: &str, space: &SearchSpace, budget: Option<&str>) -> String {
    let mut desc = match name {
        PLACEHOLDER_TRIAL_ID => "reserved: numeric trial id (auto-injected)".to_string(),
        PLACEHOLDER_TRIAL_DIR => {
            "reserved: per-trial artifact directory (auto-injected)".to_string()
        }
        _ => match space.params.iter().find(|param| param.name() == name) {
            Some(param) => describe_param(param),
            None => "not in search space (passed through as a literal value)".to_string(),
        },
    };
    if budget == Some(name) {
        desc.push_str("; scheduler budget placeholder (overridden per rung)");
    }
    desc
}

fn describe_param(spec: &ParamSpec) -> String {
    let base = match spec {
        ParamSpec::Float {
            min, max, log_scale, step, ..
        } => {
            let mut desc = format!("space param Float in [{min}, {max}]");
            if *log_scale {
                desc.push_str(", log-scale");
            }
            if let Some(step) = step {
                desc.push_str(&format!(", step {step}"));
            }
            desc
        }
        ParamSpec::Int { min, max, step, .. } => {
            let mut desc = format!("space param Int in [{min}, {max}]");
            if let Some(step) = step {
                desc.push_str(&format!(", step {step}"));
            }
            desc
        }
        ParamSpec::Choice { values, .. } => {
            format!("space param Choice: {}", values.join(", "))
        }
        ParamSpec::Bool { .. } => "space param Bool (true/false)".to_string(),
    };
    match (spec.parent(), spec.parent_values()) {
        (Some(parent), Some(values)) => {
            format!("{base}; conditional: sampled only when {parent} ∈ [{}]", values.join(", "))
        }
        _ => base,
    }
}

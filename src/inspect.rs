//! Human-readable dump of what argtuner parsed from a project's `argtuner.toml`:
//! the normalized config structs (defaults applied) plus a template placeholder
//! analysis. Used by the `argtuner inspect` subcommand and echoed in the README.

use std::io;

use crate::command::CommandTemplate;
use crate::constants::{PLACEHOLDER_TRIAL_DIR, PLACEHOLDER_TRIAL_ID};
use crate::project::{Project, UnifiedConfig};
use crate::scheduler::Scheduler;
use crate::search_space::{ParamSpec, SearchSpace};

/// Render the inspect dump for a project.
///
/// Output is deterministic for a given config and a stable `project.root()`
/// path: the first line is the project dir (forward-slash normalized so the
/// output is byte-identical across platforms), followed by the round-tripped
/// config TOML, the template, and the placeholder analysis.
pub fn render_inspect(project: &Project) -> io::Result<String> {
    let config = project.load_unified_config()?;
    let root = project.root().to_string_lossy().replace('\\', "/");

    let mut out = String::new();
    out.push_str(&format!("project: {root}\n\n"));
    out.push_str(&normalized_config_toml(&config)?);
    out.push('\n');
    out.push_str("template:\n");
    out.push_str(&indent_lines(config.template.trim(), 2));
    out.push_str("\n\n");
    out.push_str(&placeholder_analysis(&config));
    Ok(out)
}

/// Re-serialize the parsed config structs as TOML, minus the `template` key
/// (which is shown separately, unescaped, below). Round-tripping shows the
/// structs exactly as deserialized, including applied defaults and normalized
/// field names (e.g. an input `log = true` round-trips as `log_scale = true`).
fn normalized_config_toml(config: &UnifiedConfig) -> io::Result<String> {
    let mut table = toml::Value::try_from(config)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    table
        .as_table_mut()
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "config did not serialize to a table")
        })?
        .remove("template");
    let mut rendered = toml::to_string_pretty(&table)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    // The toml serializer collapses a section that holds only an array-of-tables
    // (a `[space]` whose sole key is `params` renders as bare `[[space.params]]`);
    // re-insert the header so the round-trip mirrors the source layout.
    if let Some(index) = rendered.find("[[space.params]]") {
        rendered.insert_str(index, "[space]\n");
    }
    Ok(rendered)
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
    let placeholders = match template.placeholders() {
        Ok(placeholders) => placeholders,
        Err(err) => return format!("placeholders:\n  template error: {err}\n"),
    };

    let mut out = String::from("placeholders:\n");
    for name in placeholders {
        let desc = describe_placeholder(&name, &config.space, budget.as_deref());
        out.push_str(&format!("  {{{name}}}: {desc}\n"));
    }
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
    match spec {
        ParamSpec::Float {
            name: _,
            min,
            max,
            log_scale,
            step,
            format: _,
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
        ParamSpec::Int {
            name: _,
            min,
            max,
            step,
        } => {
            let mut desc = format!("space param Int in [{min}, {max}]");
            if let Some(step) = step {
                desc.push_str(&format!(", step {step}"));
            }
            desc
        }
        ParamSpec::Choice { name: _, values, .. } => {
            format!("space param Choice: {}", values.join(", "))
        }
    }
}

fn indent_lines(text: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    text.lines()
        .map(|line| format!("{indent}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

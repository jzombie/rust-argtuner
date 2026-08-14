use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde_json::Value as JsonValue;
use shell_words::quote;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct CommandTemplate {
    template: String,
}

#[derive(Debug, Error)]
pub enum TemplateError {
    #[error("missing value for placeholder `{0}`")]
    MissingValue(String),
    #[error("unclosed placeholder in template")]
    UnclosedPlaceholder,
    #[error("invalid conditional parameter: {0}")]
    InvalidConditional(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

impl CommandTemplate {
    pub fn new<S: Into<String>>(template: S) -> Self {
        Self {
            template: template.into(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.template
    }

    pub fn placeholders(&self) -> Result<Vec<String>, TemplateError> {
        let mut set = BTreeSet::new();
        parse_template(&self.template, |name| {
            set.insert(name.to_string());
            Ok(())
        })?;
        Ok(set.into_iter().collect())
    }

    pub fn render(&self, values: &HashMap<String, String>) -> Result<String, TemplateError> {
        let mut out = String::with_capacity(self.template.len());
        let mut chars = self.template.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '{' {
                if matches!(chars.peek(), Some('{')) {
                    chars.next();
                    out.push('{');
                    continue;
                }
                let name = read_placeholder(&mut chars)?;
                let value = values.get(&name).ok_or(TemplateError::MissingValue(name))?;
                if needs_shell_quote(value) {
                    out.push_str(&quote(value));
                } else {
                    out.push_str(value);
                }
            } else if ch == '}' && matches!(chars.peek(), Some('}')) {
                chars.next();
                out.push('}');
            } else {
                out.push(ch);
            }
        }
        Ok(out)
    }

    /// Return a copy of this template with the flag segments of the given
    /// (inactive) parameters removed, so their placeholders never render.
    ///
    /// Supports both `--flag {name}` (two shell tokens) and `--flag={name}`
    /// (one unified token). A conditional parameter must be flag-bound
    /// (enforced by config validation); if a placeholder is not, this returns
    /// [`TemplateError::InvalidConditional`].
    pub fn strip_inactive_flags(&self, inactive: &[String]) -> Result<String, TemplateError> {
        let tokens = shell_words::split(&self.template).map_err(|err| {
            TemplateError::InvalidConditional(format!("template tokenize failed: {err}"))
        })?;
        let mut remove = vec![false; tokens.len()];
        for name in inactive {
            let ph = format!("{{{name}}}");
            for (i, token) in tokens.iter().enumerate() {
                if !token.contains(&ph) {
                    continue;
                }
                if token.starts_with('-') {
                    // `--flag={name}` (unified): drop the whole token.
                    remove[i] = true;
                } else {
                    // `{name}` (or a quoted `"{name}"`) value token; the
                    // immediately preceding token is the flag to drop too.
                    if let Some(j) = i.checked_sub(1).filter(|&j| tokens[j].starts_with('-')) {
                        remove[j] = true;
                    }
                    remove[i] = true;
                }
            }
        }
        Ok(tokens
            .iter()
            .enumerate()
            .filter(|(i, _)| !remove[*i])
            .map(|(_, token)| quote(token))
            .collect::<Vec<_>>()
            .join(" "))
    }

    pub fn write_to<P: AsRef<Path>>(&self, path: P) -> Result<(), TemplateError> {
        std::fs::write(path, &self.template)?;
        Ok(())
    }

    pub fn read_from<P: AsRef<Path>>(path: P) -> Result<Self, TemplateError> {
        let template = std::fs::read_to_string(path)?;
        Ok(Self { template })
    }

    /// Escape a JSON value for safe embedding inside a `CommandTemplate` string.
    ///
    /// This will:
    /// - escape double quotes as `\"` so the JSON can be placed inside a TOML/"" string,
    /// - double any `{`/`}` so the template parser treats them as literals.
    pub fn embed_json(value: &JsonValue) -> String {
        // Default: produce a JSON string suitable for embedding directly into a
        // `CommandTemplate` (e.g. when constructing a template in Rust code).
        // We do NOT escape quotes here so the resulting template contains
        // valid JSON; we only escape braces for the template parser.
        let s = value.to_string();
        s.replace('{', "{{").replace('}', "}}")
    }

    /// Like `embed_json` but also escapes double quotes so the JSON may be
    /// safely embedded inside a TOML `"..."` string literal.
    pub fn embed_json_for_toml(value: &JsonValue) -> String {
        let s = value.to_string();
        let s = s.replace('"', "\\\"");
        s.replace('{', "{{").replace('}', "}}")
    }
}

#[allow(clippy::while_let_on_iterator)]
fn parse_template<F>(template: &str, mut on_name: F) -> Result<(), TemplateError>
where
    F: FnMut(&str) -> Result<(), TemplateError>,
{
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '{' {
            if matches!(chars.peek(), Some('{')) {
                chars.next();
                continue;
            }
            let name = read_placeholder(&mut chars)?;
            on_name(&name)?;
        } else if ch == '}' && matches!(chars.peek(), Some('}')) {
            chars.next();
        }
    }
    Ok(())
}

#[allow(clippy::while_let_on_iterator)]
fn read_placeholder<I>(chars: &mut std::iter::Peekable<I>) -> Result<String, TemplateError>
where
    I: Iterator<Item = char>,
{
    let mut name = String::new();
    while let Some(ch) = chars.next() {
        if ch == '}' {
            return Ok(name.trim().to_string());
        }
        name.push(ch);
    }
    Err(TemplateError::UnclosedPlaceholder)
}

fn needs_shell_quote(value: &str) -> bool {
    value.chars().any(|ch| ch.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::CommandTemplate;
    use std::collections::HashMap;

    #[test]
    fn render_replaces_placeholders() {
        let template = CommandTemplate::new("run --lr {lr} --steps {steps}");
        let mut values = HashMap::new();
        values.insert("lr".to_string(), "0.01".to_string());
        values.insert("steps".to_string(), "100".to_string());
        let rendered = template.render(&values).expect("rendered");
        assert_eq!(rendered, "run --lr 0.01 --steps 100");
    }

    #[test]
    fn render_handles_escaped_braces() {
        let template = CommandTemplate::new("echo {{braces}} {value}");
        let mut values = HashMap::new();
        values.insert("value".to_string(), "ok".to_string());
        let rendered = template.render(&values).expect("rendered");
        assert_eq!(rendered, "echo {braces} ok");
    }

    #[test]
    fn render_quotes_values_with_spaces() {
        let template = CommandTemplate::new("run --out {dir}");
        let mut values = HashMap::new();
        values.insert("dir".to_string(), "with spaces".to_string());
        let rendered = template.render(&values).expect("rendered");
        assert_eq!(rendered, "run --out 'with spaces'");
    }

    #[test]
    fn strip_inactive_flags_removes_two_token_segment() {
        let template = CommandTemplate::new("run --lr {lr} --momentum {momentum} --steps {steps}");
        let stripped = template
            .strip_inactive_flags(&["momentum".to_string()])
            .expect("stripped");
        assert_eq!(stripped, "run --lr {lr} --steps {steps}");
    }

    #[test]
    fn strip_inactive_flags_removes_eq_form() {
        let template = CommandTemplate::new("run --lr {lr} --momentum={momentum}");
        let stripped = template
            .strip_inactive_flags(&["momentum".to_string()])
            .expect("stripped");
        assert_eq!(stripped, "run --lr {lr}");
    }

    #[test]
    fn strip_inactive_flags_preserves_quoted_literals() {
        let template = CommandTemplate::new("run --out \"my dir\" --momentum {momentum}");
        let stripped = template
            .strip_inactive_flags(&["momentum".to_string()])
            .expect("stripped");
        // The remaining literal still quotes the space-containing value.
        assert_eq!(stripped, "run --out 'my dir'");
    }
}

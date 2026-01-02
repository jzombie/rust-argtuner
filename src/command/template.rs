use std::collections::{BTreeSet, HashMap};
use std::path::Path;

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

    pub fn write_to<P: AsRef<Path>>(&self, path: P) -> Result<(), TemplateError> {
        std::fs::write(path, &self.template)?;
        Ok(())
    }

    pub fn read_from<P: AsRef<Path>>(path: P) -> Result<Self, TemplateError> {
        let template = std::fs::read_to_string(path)?;
        Ok(Self { template })
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
}
